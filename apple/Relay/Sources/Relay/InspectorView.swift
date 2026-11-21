import RelayKit
import SwiftUI

struct InspectorView: View {
    let tx: FfiTransaction?
    @State private var tab = "Response"

    var body: some View {
        if let tx {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    header(tx)
                    facts(tx)
                    if let error = tx.error {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.system(size: 12))
                            .foregroundStyle(tx.state == .failed ? Theme.s5xx : Theme.muted)
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.s5xx.opacity(0.08)))
                    }
                    Picker("", selection: $tab) {
                        Text("Request").tag("Request")
                        Text("Response").tag("Response")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    let isRequest = tab == "Request"
                    section("Headers") {
                        HeadersView(headers: isRequest ? tx.requestHeaders : tx.responseHeaders)
                    }
                    section("Body") {
                        BodyView(
                            preview: isRequest ? tx.requestBodyPreview : tx.responseBodyPreview,
                            size: isRequest ? tx.requestBodySize : tx.responseBodySize
                        )
                    }
                }
                .padding(18)
            }
        } else {
            VStack(spacing: 8) {
                Image(systemName: "sidebar.right").font(.system(size: 22)).foregroundStyle(Theme.faint)
                Text("Select a request").foregroundStyle(Theme.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func header(_ tx: FfiTransaction) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                StatusPill(tx: tx)
                Text(tx.method).font(Theme.monoSmall).foregroundStyle(Theme.accent)
                Text(tx.path).font(.system(size: 13, weight: .medium, design: .monospaced)).lineLimit(2)
            }
            Text("\(tx.host) · \(tx.clientAddr)").font(.system(size: 12)).foregroundStyle(Theme.muted)
            HStack(spacing: 6) {
                Button("Copy URL") { copy(tx.url) }
                Button("Copy as cURL") { copy(curl(tx)) }
            }
            .buttonStyle(PillButtonStyle())
        }
    }

    private func facts(_ tx: FfiTransaction) -> some View {
        let items: [(String, String)] = [
            ("Duration", formatMs(tx.durationMs)),
            ("Response", formatBytes(tx.responseBodySize)),
            ("Request", formatBytes(tx.requestBodySize)),
            ("TLS", tx.decrypted ? "Decrypted" : (tx.scheme == "https" ? "Encrypted" : "None")),
            ("State", "\(tx.state)".capitalized),
            ("Protocol", protocolLabel(tx)),
        ]
        return LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 1), count: 3), spacing: 1) {
            ForEach(items, id: \.0) { k, v in
                VStack(alignment: .leading, spacing: 3) {
                    Text(k).font(.system(size: 11)).foregroundStyle(Theme.faint)
                    Text(v).font(Theme.mono)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Theme.panel)
            }
        }
        .background(Theme.line)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
    }

    /// "HTTP/2", or "HTTP/1.1 → HTTP/2" when the app and the server legs differ.
    private func protocolLabel(_ tx: FfiTransaction) -> String {
        guard let upstream = tx.upstreamHttpVersion, upstream != tx.httpVersion else { return tx.httpVersion }
        return "\(tx.httpVersion) → \(upstream)"
    }

    private func section<C: View>(_ title: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.system(size: 12, weight: .semibold))
            content()
        }
    }

    private func copy(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }

    private func curl(_ tx: FfiTransaction) -> String {
        var parts = ["curl", "-X", tx.method, "'\(tx.url)'"]
        for h in tx.requestHeaders where !["host", "content-length"].contains(h.name.lowercased()) {
            parts.append("-H '\(h.name): \(h.value.replacingOccurrences(of: "'", with: "'\\''"))'")
        }
        if let body = tx.requestBodyPreview { parts.append("--data-raw '\(body.replacingOccurrences(of: "'", with: "'\\''"))'") }
        return parts.joined(separator: " ")
    }
}

struct HeadersView: View {
    let headers: [FfiHeader]
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if headers.isEmpty {
                Text("None").foregroundStyle(Theme.faint).padding(10)
            }
            ForEach(Array(headers.enumerated()), id: \.offset) { _, h in
                HStack(alignment: .top, spacing: 10) {
                    Text(h.name).foregroundStyle(Color(hex: 0x9CB4FF)).frame(width: 150, alignment: .leading)
                    Text(h.value).foregroundStyle(Theme.fg).textSelection(.enabled)
                }
                .font(.system(size: 11.5, design: .monospaced))
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
            }
        }
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
    }
}

struct BodyView: View {
    let preview: String?
    let size: UInt64
    var body: some View {
        Group {
            if let preview {
                Text(prettyJSON(preview) ?? preview)
                    .font(.system(size: 12, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text(size == 0 ? "Empty" : "\(formatBytes(size)) binary or not captured")
                    .foregroundStyle(Theme.faint)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
    }

    private func prettyJSON(_ s: String) -> String? {
        guard let data = s.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data),
              let out = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
        else { return nil }
        return String(data: out, encoding: .utf8)
    }
}
