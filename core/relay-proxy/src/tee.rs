//! Streaming capture: a body wrapper that forwards frames untouched while keeping a bounded copy.
//!
//! Bodies are never buffered whole — memory per transaction is capped at `capture_limit` no matter
//! how large the transfer is (a 200 MB download buffered whole got the Android app killed in P0).

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use tokio::sync::oneshot;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// The body type used on every leg of the proxy.
pub type ProxyBody = BoxBody<Bytes, BoxError>;

pub fn full(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed()
}

/// What a finished (or interrupted) body looked like.
#[derive(Debug, Clone, Default)]
pub struct TeeOutcome {
    /// The first `capture_limit` bytes of the body.
    pub captured: Bytes,
    /// Total bytes that passed through.
    pub total: u64,
    /// True if more bytes passed than were captured.
    pub truncated: bool,
    /// Set if the stream failed or was dropped before its end.
    pub error: Option<String>,
}

pub struct TeeBody<B> {
    inner: B,
    buf: BytesMut,
    limit: usize,
    total: u64,
    /// Declared length (Content-Length). Reaching it means complete, even if the inner body never
    /// reports its end (e.g. HTTP/1.1 client leg stops polling an HTTP/2 upstream body at the length).
    expected: Option<u64>,
    done: Option<oneshot::Sender<TeeOutcome>>,
}

impl<B> TeeBody<B> {
    pub fn new(inner: B, limit: usize) -> (Self, oneshot::Receiver<TeeOutcome>) {
        let (tx, rx) = oneshot::channel();
        (Self { inner, buf: BytesMut::new(), limit, total: 0, expected: None, done: Some(tx) }, rx)
    }

    /// Declares the body length (from `Content-Length`), so reaching it counts as complete.
    pub fn with_expected_len(mut self, len: Option<u64>) -> Self {
        self.expected = len;
        self
    }

    fn finish(&mut self, error: Option<String>) {
        if let Some(tx) = self.done.take() {
            let truncated = self.total > self.buf.len() as u64;
            let _ = tx.send(TeeOutcome {
                captured: std::mem::take(&mut self.buf).freeze(),
                total: self.total,
                truncated,
                error,
            });
        }
    }
}

impl<B> Body for TeeBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.total += data.len() as u64;
                    let room = self.limit.saturating_sub(self.buf.len());
                    if room > 0 {
                        let take = room.min(data.len());
                        self.buf.extend_from_slice(&data[..take]);
                    }
                }
                // hyper stops polling once a length-delimited body reports its end, so the final
                // `None` may never come: finish here instead.
                if self.inner.is_end_stream() || self.expected == Some(self.total) {
                    self.finish(None);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                let e: BoxError = e.into();
                self.finish(Some(e.to_string()));
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.finish(None);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for TeeBody<B> {
    fn drop(&mut self) {
        // Dropped before the end: the peer went away mid-transfer.
        if self.done.is_some() {
            // Completed bodies were already finished in `poll_frame`; an empty body that was never
            // polled is not an error.
            let complete = self.total == 0 || self.expected == Some(self.total);
            let msg = (!complete).then(|| "stream closed before the end of the body".to_string());
            self.finish(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::StreamBody;

    fn chunks(parts: Vec<&'static [u8]>) -> ProxyBody {
        let stream = futures_lite_iter(parts.into_iter().map(|p| Ok::<_, BoxError>(Frame::data(Bytes::from_static(p)))));
        StreamBody::new(stream).boxed()
    }

    /// Minimal Stream over an iterator (avoids a futures dependency).
    fn futures_lite_iter<I: Iterator + Unpin>(it: I) -> IterStream<I> {
        IterStream(it)
    }
    struct IterStream<I>(I);
    impl<I: Iterator + Unpin> futures_core::Stream for IterStream<I> {
        type Item = I::Item;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<I::Item>> {
            Poll::Ready(self.0.next())
        }
    }

    #[tokio::test]
    async fn forwards_everything_and_caps_the_capture() {
        let (tee, rx) = TeeBody::new(chunks(vec![b"hello ", b"streaming ", b"world"]), 8);
        let out = tee.collect().await.unwrap().to_bytes();
        assert_eq!(&out[..], b"hello streaming world", "client receives every byte");
        let o = rx.await.unwrap();
        assert_eq!((&o.captured[..], o.total, o.truncated, o.error), (&b"hello st"[..], 21, true, None));
    }

    #[tokio::test]
    async fn reaching_content_length_counts_as_complete() {
        // Two chunks, declared length 16; the consumer stops after the bytes, never polling the end.
        let (tee, rx) = TeeBody::new(chunks(vec![b"part one", b"part two"]), 1024);
        let mut tee = tee.with_expected_len(Some(16));
        for _ in 0..2 {
            std::future::poll_fn(|cx| Pin::new(&mut tee).poll_frame(cx)).await.unwrap().unwrap();
        }
        drop(tee);
        let o = rx.await.unwrap();
        assert_eq!((o.total, o.error), (16, None));
    }

    #[tokio::test]
    async fn reports_early_drop_as_error() {
        let (mut tee, rx) = TeeBody::new(chunks(vec![b"part one", b"part two"]), 1024);
        let first = std::future::poll_fn(|cx| Pin::new(&mut tee).poll_frame(cx)).await;
        assert!(first.is_some());
        drop(tee);
        let o = rx.await.unwrap();
        assert_eq!(o.total, 8);
        assert!(o.error.unwrap().contains("closed before the end"));
    }
}
