import RelayKit
import SwiftUI

extension FfiPaused: @retroactive Identifiable {
    public var id: String { "\(txId)-\(response ? "res" : "req")" }
}

/// The editor shown while a request or response is held at a breakpoint.
struct BreakpointView: View {
    @Environment(TrafficStore.self) private var store
    let item: FfiPaused

    @State private var status: String
    @State private var headers: String
    @State private var bodyText: String

    init(item: FfiPaused) {
        self.item = item
        _status = State(initialValue: item.status.map(String.init) ?? "")
        _headers = State(initialValue: item.headers.map { "\($0.name): \($0.value)" }.joined(separator: "\n"))
        _bodyText = State(initialValue: item.body)
    }

    private var changed: Bool {
        status != (item.status.map(String.init) ?? "")
            || headers != item.headers.map { "\($0.name): \($0.value)" }.joined(separator: "\n")
            || (item.bodyIsText && bodyText != item.body)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "pause.circle.fill").foregroundStyle(Theme.paused)
                Text(item.response ? "Response held" : "Request held")
                    .font(.system(size: 15, weight: .semibold))
                Text("rule \(item.rule)").font(.system(size: 12)).foregroundStyle(Theme.muted)
                Spacer()
                if store.paused.count > 1 {
                    Text("\(store.paused.count - 1) more waiting").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            Text("\(item.method) \(item.url)")
                .font(Theme.mono).lineLimit(2).truncationMode(.middle)
                .foregroundStyle(Theme.muted)

            if item.response {
                HStack {
                    Text("Status").font(.system(size: 12)).foregroundStyle(Theme.muted)
                    TextField("200", text: $status).textFieldStyle(.roundedBorder).frame(width: 80)
                }
            }
            editor("Headers  (one “Name: value” per line)", text: $headers, height: 120)
            if item.bodyIsText {
                editor("Body", text: $bodyText, height: 190)
            } else {
                Text("Binary body — it is sent unchanged.").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }

            HStack {
                Button("Abort") { store.abortPaused(item) }
                    .help("The app gets an error instead")
                Spacer()
                Button(changed ? "Send with edits" : "Resume") { send() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(18)
        .frame(width: 620)
        .background(Theme.panel)
    }

    private func editor(_ title: String, text: Binding<String>, height: CGFloat) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.system(size: 12)).foregroundStyle(Theme.muted)
            TextEditor(text: text)
                .font(Theme.mono)
                .scrollContentBackground(.hidden)
                .padding(6)
                .frame(height: height)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
        }
    }

    private func send() {
        guard changed else { return store.resumePaused(item, edit: nil) }
        let parsed: [FfiHeader] = headers.split(separator: "\n").compactMap { line in
            guard let i = line.firstIndex(of: ":") else { return nil }
            return FfiHeader(
                name: line[..<i].trimmingCharacters(in: .whitespaces),
                value: line[line.index(after: i)...].trimmingCharacters(in: .whitespaces))
        }
        store.resumePaused(
            item,
            edit: FfiEdit(
                status: UInt16(status.trimmingCharacters(in: .whitespaces)),
                headers: parsed,
                body: item.bodyIsText ? bodyText : item.body))
    }
}
