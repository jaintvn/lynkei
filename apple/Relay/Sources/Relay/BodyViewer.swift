import AppKit
import RelayKit
import SwiftUI

private let truncationMarker = "\n…[truncated]"

/// What the inspector knows about one side's body: its text (as large as Relay captured), whether
/// that is everything, and whether it is JSON.
@MainActor struct CapturedBody {
    let text: String?
    let size: UInt64
    let truncated: Bool

    init(tx: FfiTransaction, isRequest: Bool) {
        let raw = isRequest ? tx.requestBodyPreview : tx.responseBodyPreview
        size = isRequest ? tx.requestBodySize : tx.responseBodySize
        if let raw, raw.hasSuffix(truncationMarker) {
            text = String(raw.dropLast(truncationMarker.count))
            truncated = true
        } else {
            text = raw
            truncated = false
        }
    }

    /// The text to show: pretty-printed when it is JSON. Parsing a multi-megabyte body is slow, so
    /// the last result is kept and reused while the same body stays on screen.
    var display: String? { parsed?.text }
    var isJSON: Bool { parsed?.json ?? false }

    @MainActor private static var memo: (source: String, text: String, json: Bool)?

    @MainActor private var parsed: (text: String, json: Bool)? {
        guard let text else { return nil }
        if let m = Self.memo, m.source.utf8.count == text.utf8.count, m.source == text { return (m.text, m.json) }
        let doc = JSONDoc(text: text)
        let shown = doc != nil ? JSONFormat.pretty(text) : text
        Self.memo = (text, shown, doc != nil)
        return (shown, doc != nil)
    }
}

/// The body in the inspector: the whole text, scrollable and selectable, with a button for the
/// full search-and-edit view.
struct BodyView: View {
    let tx: FfiTransaction
    let isRequest: Bool
    @State private var showSheet = false

    var body: some View {
        let captured = CapturedBody(tx: tx, isRequest: isRequest)
        VStack(alignment: .leading, spacing: 8) {
            if let shown = captured.display {
                CodeText(text: .constant(shown))
                    .frame(height: 280)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
                HStack(spacing: 8) {
                    if !GRPC.isGRPC(tx) {
                        Button { showSheet = true } label: { Label("Search & edit…", systemImage: "magnifyingglass") }
                    }
                    Button("Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(shown, forType: .string)
                    }
                    Text("\(formatBytes(captured.size))\(GRPC.isGRPC(tx) ? " · decoded gRPC, read-only" : (captured.isJSON ? " · JSON" : ""))")
                        .font(.system(size: 11.5)).foregroundStyle(Theme.faint)
                    if captured.truncated {
                        Label("only the first part was captured", systemImage: "exclamationmark.triangle")
                            .font(.system(size: 11.5)).foregroundStyle(Theme.accent)
                    }
                }
                .buttonStyle(PillButtonStyle())
            } else {
                Text(captured.size == 0 ? "Empty" : "\(formatBytes(captured.size)) binary or not captured")
                    .foregroundStyle(Theme.faint)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            }
        }
        .sheet(isPresented: $showSheet) { BodySheet(tx: tx, isRequest: isRequest) }
    }
}

/// The whole body in a large window: search with highlights, edit by hand or field by field, and
/// keep the edit as a rewrite rule.
struct BodySheet: View {
    @Environment(\.dismiss) private var dismiss
    let tx: FfiTransaction
    let isRequest: Bool

    private let captured: CapturedBody
    private let original: String
    @State private var text: String
    @State private var fields: [JSONField] = []
    @State private var mode = "Text"
    @State private var query = ""
    @State private var matches = 0
    @State private var current = 0
    @State private var editing = false
    @State private var ruleDraft: RewriteDraft?
    @State private var parseError = false

    init(tx: FfiTransaction, isRequest: Bool) {
        self.tx = tx
        self.isRequest = isRequest
        let c = CapturedBody(tx: tx, isRequest: isRequest)
        captured = c
        original = c.display ?? ""
        _text = State(initialValue: c.display ?? "")
    }

    private var changed: Bool { text != original }
    private var canEdit: Bool { !captured.truncated }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("\(isRequest ? "Request" : "Response") body").font(.system(size: 16, weight: .semibold))
                Text("\(tx.method) \(tx.host)\(tx.path.split(separator: "?").first.map { String($0) } ?? "")")
                    .font(Theme.mono).foregroundStyle(Theme.muted).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            toolbar
            if captured.truncated {
                Label("Relay kept only the first part of this body, so editing is off.", systemImage: "exclamationmark.triangle")
                    .font(.system(size: 12)).foregroundStyle(Theme.accent)
            }
            Group {
                if mode == "Fields" { fieldsList } else {
                    CodeText(text: $text, editable: editing && canEdit, query: query, current: current) { matches = $0 }
                        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 10).stroke(editing ? Theme.accent.opacity(0.6) : Theme.line2))
                }
            }
            .frame(maxHeight: .infinity)
            footer
        }
        .padding(18)
        .frame(width: 920, height: 700)
        .background(Theme.panel)
        .onChange(of: text) { if mode == "Fields" { reloadFields() } }
        .sheet(item: $ruleDraft) { RewriteView(draft: $0) }
    }

    // MARK: Toolbar

    private var toolbar: some View {
        HStack(spacing: 10) {
            if captured.isJSON {
                Picker("", selection: $mode) {
                    Text("Text").tag("Text")
                    Text("Fields").tag("Fields")
                }
                .pickerStyle(.segmented).labelsHidden().frame(width: 140)
                .onChange(of: mode) { if mode == "Fields" { reloadFields() } }
            }
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                TextField(mode == "Fields" ? "Filter fields by name or value" : "Search in the body", text: $query)
                    .textFieldStyle(.plain).font(Theme.mono)
                    .onSubmit { current += 1 }
                    .onChange(of: query) { current = 0 }
                if mode == "Text", !query.isEmpty {
                    Text(matches == 0 ? "no matches" : "\((current % max(matches, 1) + max(matches, 1)) % max(matches, 1) + 1) of \(matches)")
                        .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
                    Button { current -= 1 } label: { Image(systemName: "chevron.up") }.disabled(matches == 0)
                    Button { current += 1 } label: { Image(systemName: "chevron.down") }.disabled(matches == 0)
                }
            }
            .padding(.horizontal, 10).frame(height: 28)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
            Spacer(minLength: 4)
            if mode == "Text" {
                Toggle("Edit", isOn: $editing).toggleStyle(.switch).controlSize(.small).disabled(!canEdit)
                Button("Format") { format() }.disabled(!canEdit || !editing)
            }
            Button("Copy") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(text, forType: .string)
            }
        }
        .buttonStyle(PillButtonStyle())
    }

    // MARK: Fields

    private var visibleFields: [JSONField] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return fields }
        return fields.filter { $0.path.lowercased().contains(q) || $0.display.lowercased().contains(q) }
    }

    private var fieldsList: some View {
        Group {
            if parseError {
                Text("The text is not valid JSON right now. Fix it in the Text view first.")
                    .foregroundStyle(Theme.s5xx).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(visibleFields) { f in
                            HStack(spacing: 10) {
                                Text(f.kind.rawValue)
                                    .font(.system(size: 9.5, weight: .bold)).foregroundStyle(kindColor(f.kind))
                                    .frame(width: 44, alignment: .leading)
                                Text(f.path).font(Theme.mono).foregroundStyle(Theme.muted)
                                    .lineLimit(1).truncationMode(.middle).frame(width: 330, alignment: .leading)
                                    .help(f.path)
                                if f.kind == .empty || !canEdit {
                                    Text(f.display).font(Theme.mono).lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
                                } else {
                                    TextField("", text: Binding(get: { f.display }, set: { edit(f, $0) }))
                                        .textFieldStyle(.roundedBorder).font(Theme.mono)
                                }
                            }
                            .padding(.vertical, 4).padding(.horizontal, 10)
                            Divider().overlay(Theme.line)
                        }
                        if fields.count >= 5000 { Text("Showing the first 5000 fields. Use the Text view for the rest.").font(.system(size: 11.5)).foregroundStyle(Theme.faint).padding(10) }
                    }
                }
                .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
            }
        }
    }

    private func kindColor(_ k: JSONField.Kind) -> Color {
        switch k {
        case .string: return Theme.s2xx
        case .number: return Theme.accent
        case .bool: return Theme.paused
        case .null: return Theme.faint
        case .empty: return Theme.muted
        }
    }

    private func reloadFields() {
        if let doc = JSONDoc(text: text) { fields = doc.fields(); parseError = false } else { parseError = true }
    }

    private func edit(_ f: JSONField, _ typed: String) {
        guard var doc = JSONDoc(text: text) else { parseError = true; return }
        doc.set(f, to: typed)
        text = doc.pretty
    }

    private func format() {
        if JSONDoc(text: text) != nil { text = JSONFormat.pretty(text) }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 10) {
            if changed {
                let steps = bodyChangeSteps(original: original, edited: text)
                Label("\(steps.count == 1 && steps[0].kind == "set_body" ? "Whole body replaced" : "\(steps.count) field\(steps.count == 1 ? "" : "s") changed")", systemImage: "pencil")
                    .font(.system(size: 12)).foregroundStyle(Theme.accent)
            } else {
                Text(editing || mode == "Fields" ? "Nothing changed yet." : "Turn on Edit to change the text by hand, or use Fields to change values one by one.")
                    .font(.system(size: 12)).foregroundStyle(Theme.faint)
            }
            Spacer()
            Button("Revert") { text = original }.disabled(!changed)
            Button("Keep as a modify rule…") { startRule() }
                .buttonStyle(.borderedProminent).disabled(!changed)
                .help("Opens the Modify editor with these changes, so they apply every time this call is made")
        }
    }

    private func startRule() {
        var d = RewriteDraft(copying: tx)
        let i = isRequest ? 0 : 1
        d.sides[i].steps = bodyChangeSteps(original: original, edited: text)
        d.startTab = i
        d.name = "Edit \(isRequest ? "request" : "response") of \(tx.host)\(d.path)"
        ruleDraft = d
    }
}
