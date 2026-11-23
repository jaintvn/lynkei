import RelayKit
import SwiftUI

extension FfiPaused: @retroactive Identifiable {
    public var id: String { "\(txId)-\(response ? "res" : "req")" }
}

/// Breakpoint editor (canvas board 5): a held request or response, with the queue of everything
/// else waiting. It takes over the window while something is held.
struct BreakpointScreen: View {
    @Environment(TrafficStore.self) private var store
    @State private var picked: String?

    private var item: FfiPaused? {
        store.paused.first { $0.id == picked } ?? store.paused.first
    }

    var body: some View {
        if let item {
            VStack(spacing: 0) {
                banner(item)
                HStack(alignment: .top, spacing: 0) {
                    queue(item)
                        .frame(width: 260)
                        .background(Theme.panel)
                        .overlay(alignment: .trailing) { Rectangle().fill(Theme.line).frame(width: 1) }
                    BreakpointEditor(item: item).id(item.id)
                }
            }
        }
    }

    private func banner(_ item: FfiPaused) -> some View {
        HStack(spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: "pause.fill").font(.system(size: 11))
                Text("Paused · \(item.response ? "Response" : "Request")").font(.system(size: 12, weight: .semibold))
            }
            .foregroundStyle(Theme.paused)
            .padding(.horizontal, 10).frame(height: 28)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.paused.opacity(0.16)))
            VStack(alignment: .leading, spacing: 2) {
                Text("\(item.method) \(path(item))").font(.system(size: 14, weight: .semibold, design: .monospaced)).lineLimit(1)
                Text("matched “\(item.rule)”").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            Spacer()
            Text("The app waits until you continue or abort.").font(.system(size: 12)).foregroundStyle(Theme.muted)
        }
        .padding(.leading, 78).padding(.trailing, 22).padding(.vertical, 14)
        .background(Theme.paused.opacity(0.09).blended(over: Theme.panel))
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
    }

    private func queue(_ current: FfiPaused) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionLabel("Paused · \(store.paused.count)").padding(.leading, 8).padding(.bottom, 6)
            ForEach(store.paused) { p in
                let on = p.id == current.id
                Button { picked = p.id } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("\(p.method) \(path(p))").font(.system(size: 12, weight: .medium, design: .monospaced)).lineLimit(1)
                        Text("\(p.response ? "response" : "request") · \(p.rule)").font(.system(size: 11.5)).foregroundStyle(Theme.muted).lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10).padding(.vertical, 9)
                    .background(RoundedRectangle(cornerRadius: 8).fill(on ? Theme.selection : .clear))
                    .overlay(alignment: .leading) { if on { Rectangle().fill(Theme.accent).frame(width: 2) } }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer()
            Button { for p in store.paused { store.resumePaused(p, edit: nil) } } label: {
                Text("Continue all").frame(maxWidth: .infinity).font(.system(size: 13, weight: .medium)).frame(height: 32)
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 10).padding(.vertical, 14)
        .frame(maxHeight: .infinity, alignment: .top)
    }

    private func path(_ p: FfiPaused) -> String {
        guard let u = URL(string: p.url) else { return p.url }
        return u.path.isEmpty ? "/" : u.path
    }
}

private extension Color {
    /// A translucent tint laid over a base colour.
    func blended(over base: Color) -> some View { base.overlay(self) }
}

private struct HeaderRow: Identifiable {
    let id = UUID()
    var name: String
    var value: String
    var original: String?
    var removed = false
    var edited: Bool { original == nil || original != value }
}

private struct BreakpointEditor: View {
    @Environment(TrafficStore.self) private var store
    let item: FfiPaused
    @State private var rows: [HeaderRow]
    @State private var status: String
    @State private var bodyText: String

    init(item: FfiPaused) {
        self.item = item
        _rows = State(initialValue: item.headers.map { HeaderRow(name: $0.name, value: $0.value, original: $0.value) })
        _status = State(initialValue: item.status.map(String.init) ?? "")
        _bodyText = State(initialValue: item.body)
    }

    private var changed: Bool {
        rows.contains { $0.removed || $0.edited }
            || rows.count != item.headers.count
            || status != (item.status.map(String.init) ?? "")
            || (item.bodyIsText && bodyText != item.body)
    }

    private var jsonValid: Bool? {
        guard item.bodyIsText, !bodyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return (try? JSONSerialization.jsonObject(with: Data(bodyText.utf8), options: [.fragmentsAllowed])) != nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .bottom, spacing: 8) {
                labelled("Method") {
                    Text(item.method).font(.system(size: 12, weight: .semibold, design: .monospaced))
                        .padding(.horizontal, 10).frame(width: 90, height: 36, alignment: .leading)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
                }
                labelled("URL") {
                    Text(item.url).font(.system(size: 12, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                        .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.accent))
                }
                if item.response {
                    labelled("Status") {
                        TextField("200", text: $status).textFieldStyle(.plain)
                            .font(.system(size: 12, weight: .semibold, design: .monospaced))
                            .padding(.horizontal, 10).frame(width: 80, height: 36)
                            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
                    }
                }
            }

            HStack(alignment: .top, spacing: 16) {
                headersColumn.frame(maxWidth: .infinity)
                bodyColumn.frame(maxWidth: .infinity)
            }
            .frame(maxHeight: .infinity)

            HStack(spacing: 8) {
                Button { store.abortPaused(item) } label: {
                    Text("Abort").font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.s5xx)
                        .padding(.horizontal, 14).frame(height: 36)
                        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.s5xx.opacity(0.45)))
                }
                .buttonStyle(.plain)
                .help("The app gets an error instead")
                Spacer()
                Text("⌘↩ Execute · ⌥⌘↩ Continue unchanged").font(.system(size: 12)).foregroundStyle(Theme.faint)
                Button { store.resumePaused(item, edit: nil) } label: {
                    Text("Continue unchanged").font(.system(size: 13, weight: .medium)).padding(.horizontal, 14).frame(height: 36)
                        .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.line2))
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.return, modifiers: [.command, .option])
                Button { send() } label: { Text("Execute modified") }
                    .buttonStyle(AccentButtonStyle(height: 36))
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(!changed)
                    .opacity(changed ? 1 : 0.5)
            }
            .padding(.top, 14)
            .overlay(alignment: .top) { Rectangle().fill(Theme.line).frame(height: 1) }
        }
        .padding(.horizontal, 24).padding(.vertical, 20)
    }

    private func labelled<C: View>(_ title: String, @ViewBuilder _ content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
            content()
        }
    }

    private var headersColumn: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Headers").font(.system(size: 12.5, weight: .semibold))
                Spacer()
                Button("+ Add header") { rows.append(HeaderRow(name: "", value: "", original: nil)) }
                    .buttonStyle(.plain).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.accent)
            }
            ScrollView {
                VStack(spacing: 0) {
                    ForEach($rows) { $r in
                        HStack(spacing: 0) {
                            TextField("name", text: $r.name).textFieldStyle(.plain)
                                .font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.key)
                                .padding(.horizontal, 10).frame(width: 160, height: 34, alignment: .leading)
                                .overlay(alignment: .trailing) { Rectangle().fill(Theme.line).frame(width: 1) }
                            TextField("value", text: $r.value).textFieldStyle(.plain)
                                .font(.system(size: 12, design: .monospaced))
                                .strikethrough(r.removed)
                                .padding(.horizontal, 10).frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
                            Button { r.removed.toggle() } label: {
                                Image(systemName: r.removed ? "arrow.uturn.backward" : "xmark").font(.system(size: 10)).foregroundStyle(Theme.muted)
                                    .frame(width: 28, height: 34)
                            }
                            .buttonStyle(.plain).help(r.removed ? "Keep this header" : "Remove this header")
                        }
                        .background(r.edited && !r.removed ? Theme.accent.opacity(0.09) : .clear)
                        .overlay(alignment: .bottom) { Rectangle().fill(Theme.line).frame(height: 1) }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            Text("Edited values are highlighted. Struck-through headers will be removed.")
                .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
        }
    }

    private var bodyColumn: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text("Body").font(.system(size: 12.5, weight: .semibold))
                if let ok = jsonValid {
                    Text(ok ? "Valid JSON" : "Not valid JSON")
                        .font(.system(size: 11)).foregroundStyle(ok ? Theme.s2xx : Theme.s4xx)
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 4).fill((ok ? Theme.s2xx : Theme.s4xx).opacity(0.14)))
                }
                Spacer()
                Button("Format") { reformat(pretty: true) }.buttonStyle(.plain)
                Text("·").foregroundStyle(Theme.muted)
                Button("Minify") { reformat(pretty: false) }.buttonStyle(.plain)
            }
            .font(.system(size: 12)).foregroundStyle(Theme.muted)
            if item.bodyIsText {
                TextEditor(text: $bodyText)
                    .font(.system(size: 12, design: .monospaced))
                    .scrollContentBackground(.hidden)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            } else {
                Text("Binary body — it is sent unchanged.")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            }
        }
    }

    private func reformat(pretty: Bool) {
        guard let obj = try? JSONSerialization.jsonObject(with: Data(bodyText.utf8), options: [.fragmentsAllowed]),
              let data = try? JSONSerialization.data(withJSONObject: obj, options: pretty ? [.prettyPrinted, .withoutEscapingSlashes] : [.withoutEscapingSlashes]),
              let text = String(data: data, encoding: .utf8) else { return }
        bodyText = text
    }

    private func send() {
        let headers = rows.filter { !$0.removed && !$0.name.trimmingCharacters(in: .whitespaces).isEmpty }
            .map { FfiHeader(name: $0.name.trimmingCharacters(in: .whitespaces), value: $0.value) }
        store.resumePaused(
            item,
            edit: FfiEdit(
                status: UInt16(status.trimmingCharacters(in: .whitespaces)),
                headers: headers,
                body: item.bodyIsText ? bodyText : item.body))
    }
}
