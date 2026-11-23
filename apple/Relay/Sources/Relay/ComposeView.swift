import RelayKit
import SwiftUI

/// A request being composed: usually an edited copy of a captured one.
struct ComposeDraft: Identifiable {
    let id = UUID()
    var method = "GET"
    var url = "https://"
    /// One `Name: value` per line.
    var headers = ""
    var body = ""
    /// Set when the original body could not be copied in full (binary or larger than Relay keeps).
    var bodyWarning: String?

    init() {}

    init(copying tx: FfiTransaction) {
        method = tx.method
        url = tx.url
        headers = tx.requestHeaders
            .filter { !["host", "content-length", "connection", "transfer-encoding"].contains($0.name.lowercased()) }
            .map { "\($0.name): \($0.value)" }
            .joined(separator: "\n")
        body = tx.requestBodyPreview ?? ""
        if tx.requestBodySize > 0 && UInt64(body.utf8.count) < tx.requestBodySize {
            bodyWarning = tx.requestBodyPreview == nil
                ? "The original body (\(tx.requestBodySize) bytes) isn't text, so it was not copied."
                : "The original body was longer than Relay keeps, so only the start was copied."
        }
    }

    var request: FfiComposeRequest {
        let parsed = headers.split(whereSeparator: \.isNewline).compactMap { line -> FfiHeader? in
            guard let colon = line.firstIndex(of: ":") else { return nil }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else { return nil }
            return FfiHeader(name: name, value: line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces))
        }
        return FfiComposeRequest(method: method.trimmingCharacters(in: .whitespaces), url: url.trimmingCharacters(in: .whitespaces), headers: parsed, body: body)
    }
}

/// Compose / Edit & Repeat: edit a request, send it through Relay one or more times, read the answers.
struct ComposeView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State var draft: ComposeDraft

    @State private var times = "1"
    @State private var allAtOnce = false
    @State private var sending = false
    @State private var results: [FfiComposeResult] = []
    @State private var shown = 0
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Compose").font(.system(size: 16, weight: .semibold))
            HStack(spacing: 8) {
                TextField("GET", text: $draft.method).frame(width: 80)
                TextField("https://api.example.com/path", text: $draft.url).onSubmit(send)
            }
            .textFieldStyle(.roundedBorder).font(Theme.mono)

            label("Headers (one per line, Name: value)")
            editor($draft.headers, height: 90)
            label("Body")
            editor($draft.body, height: 100)
            if let w = draft.bodyWarning { Text(w).font(.system(size: 11.5)).foregroundStyle(Theme.accent) }

            HStack(spacing: 10) {
                Text("Send").font(.system(size: 12)).foregroundStyle(Theme.muted)
                TextField("1", text: $times).textFieldStyle(.roundedBorder).frame(width: 52)
                Text("time(s)").font(.system(size: 12)).foregroundStyle(Theme.muted)
                Toggle("all at once", isOn: $allAtOnce).toggleStyle(.checkbox).font(.system(size: 12))
                Spacer()
                if sending { ProgressView().controlSize(.small) }
                Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Send") { send() }
                    .keyboardShortcut(.return, modifiers: .command)
                    .buttonStyle(.borderedProminent)
                    .disabled(sending || draft.url.count < 8)
            }
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            if !results.isEmpty { resultsView }
        }
        .padding(18)
        .frame(width: 640)
        .background(Theme.panel)
    }

    private func label(_ text: String) -> some View {
        Text(text).font(.system(size: 11.5, weight: .medium)).foregroundStyle(Theme.muted)
    }

    private func editor(_ text: Binding<String>, height: CGFloat) -> some View {
        TextEditor(text: text)
            .font(Theme.mono)
            .scrollContentBackground(.hidden)
            .padding(6)
            .frame(height: height)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
    }

    private var resultsView: some View {
        VStack(alignment: .leading, spacing: 6) {
            let ok = results.filter { $0.error == nil && $0.status < 400 }.count
            let times = results.map(\.durationMs).filter { $0 > 0 }
            Text("\(results.count) sent · \(ok) ok" + (times.isEmpty ? "" : " · \(times.min()!)–\(times.max()!) ms"))
                .font(.system(size: 12, weight: .semibold))
            if results.count > 1 {
                ScrollView(.horizontal) {
                    HStack(spacing: 4) {
                        ForEach(Array(results.enumerated()), id: \.offset) { i, r in
                            Button("#\(i + 1) \(r.error == nil ? String(r.status) : "ERR")") { shown = i }
                                .buttonStyle(.bordered).controlSize(.small)
                                .tint(i == shown ? Theme.accent : nil)
                        }
                    }
                }
            }
            if shown < results.count { detail(results[shown]) }
        }
    }

    private func detail(_ r: FfiComposeResult) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                if let e = r.error {
                    Text(e).foregroundStyle(Theme.s5xx)
                } else {
                    Text("\(r.status)  ·  \(r.bodySize) B  ·  \(r.durationMs) ms").foregroundStyle(Theme.accent)
                    Text(r.headers.map { "\($0.name): \($0.value)" }.joined(separator: "\n")).foregroundStyle(Theme.muted)
                    Text(r.bodyPreview ?? "(binary body)")
                }
            }
            .font(Theme.monoSmall)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
        .frame(height: 170)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
    }

    private func send() {
        guard !sending else { return }
        sending = true
        error = nil
        let count = max(1, min(200, Int(times) ?? 1))
        let request = draft.request
        let parallel = allAtOnce
        Task {
            do {
                results = try await store.compose(request, count: count, parallel: parallel)
                shown = 0
            } catch {
                self.error = "\(error)"
            }
            sending = false
        }
    }
}
