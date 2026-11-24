import LynkeiKit
import SwiftUI

/// One edit inside a rewrite rule, as the user sees and changes it.
struct StepDraft: Identifiable, Equatable {
    let id = UUID()
    var kind: String
    var name = ""
    var value = ""
    var path = ""
    var find = ""
    var replace = ""
    var regex = false

    init(kind: String) { self.kind = kind }

    init(_ s: FfiRewriteStep) {
        kind = s.kind; name = s.name; value = s.value; path = s.path
        find = s.find; replace = s.replace; regex = s.regex
    }

    var ffi: FfiRewriteStep {
        FfiRewriteStep(kind: kind, name: name, value: value, path: path, find: find, replace: replace, regex: regex)
    }

    /// Shown in the left-hand label of the row.
    var title: String {
        switch kind {
        case "set_header": return "Set header"
        case "remove_header": return "Remove header"
        case "set_status": return "Set status"
        case "set_json": return "Set JSON field"
        case "remove_json": return "Remove JSON field"
        case "replace_body": return "Replace text"
        case "set_body": return "Replace whole body"
        default: return "Delay (ms)"
        }
    }
}

/// A field of the captured JSON body that can be picked and changed.
struct JSONLeaf: Identifiable {
    var id: String { path }
    let path: String
    /// JSON text of the current value (a string keeps its quotes).
    let json: String
    var shown: String { json.count > 40 ? String(json.prefix(40)) + "…" : json }
}

/// Every field (leaf) of a JSON document with its path, up to a limit.
func jsonLeaves(_ text: String?, limit: Int = 60) -> [JSONLeaf] {
    guard let text, let data = text.data(using: .utf8),
          let root = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) else { return [] }
    var out: [JSONLeaf] = []
    func key(_ k: String) -> String {
        k.range(of: "^[A-Za-z0-9_-]+$", options: .regularExpression) != nil ? ".\(k)" : "[\"\(k)\"]"
    }
    func render(_ v: Any) -> String {
        if let s = v as? String, let d = try? JSONSerialization.data(withJSONObject: [s]),
           let arr = String(data: d, encoding: .utf8) { return String(arr.dropFirst().dropLast()) }
        if v is NSNull { return "null" }
        if let n = v as? NSNumber { return CFGetTypeID(n) == CFBooleanGetTypeID() ? (n.boolValue ? "true" : "false") : "\(n)" }
        return "\(v)"
    }
    func walk(_ v: Any, _ path: String) {
        guard out.count < limit else { return }
        if let d = v as? [String: Any] {
            if d.isEmpty { out.append(JSONLeaf(path: path, json: "{}")) }
            for k in d.keys.sorted() { walk(d[k]!, path + key(k)) }
        } else if let a = v as? [Any] {
            if a.isEmpty { out.append(JSONLeaf(path: path, json: "[]")) }
            for (i, x) in a.enumerated() { walk(x, "\(path)[\(i)]") }
        } else {
            out.append(JSONLeaf(path: path, json: render(v)))
        }
    }
    walk(root, "$")
    return out
}

/// A rewrite being created from a captured call, or an existing rule being edited.
struct RewriteDraft: Identifiable {
    struct Side: Identifiable {
        var id: String { phase }
        var phase: String  // request | response | both
        var steps: [StepDraft] = []
        /// Id of the saved rule behind this side ("" until first saved).
        var ruleId = ""
        var label: String { phase == "both" ? "Request and response" : phase.capitalized }
    }

    let id = UUID()
    var name = ""
    var enabled = true
    var method = ""
    var host = ""
    var path = ""
    var sides: [Side] = []
    var requestHeaders: [FfiHeader] = []
    var responseHeaders: [FfiHeader] = []
    var requestFields: [JSONLeaf] = []
    var responseFields: [JSONLeaf] = []
    var isNew: Bool { sides.allSatisfy { $0.ruleId.isEmpty } }
    /// Which tab the editor opens on.
    var startTab = 0

    /// A new rewrite for this call: its method, host and path are filled in, and its headers and
    /// JSON fields can be picked from the Add menu.
    init(copying tx: FfiTransaction) {
        method = tx.method
        host = tx.host
        path = tx.path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? "/"
        name = "Modify \(tx.host)\(path)"
        sides = [Side(phase: "request"), Side(phase: "response")]
        requestHeaders = tx.requestHeaders
        responseHeaders = tx.responseHeaders
        requestFields = jsonLeaves(tx.requestBodyPreview)
        responseFields = jsonLeaves(tx.responseBodyPreview)
    }

    /// A blank rewrite (from the Rules screen).
    init() {
        sides = [Side(phase: "request"), Side(phase: "response")]
    }

    /// An existing rule, to change its values.
    init(rule r: FfiRewriteRule) {
        name = r.name; enabled = r.enabled
        method = r.method; host = r.host; path = r.path
        sides = [Side(phase: r.phase, steps: r.steps.map(StepDraft.init), ruleId: r.id)]
    }
}

/// Modify: pick a call, say what to change in its request or response, keep it as a
/// rule that can be switched on and off and edited later.
struct RewriteView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State var draft: RewriteDraft
    @State private var tab: Int
    @State private var error: String?

    init(draft: RewriteDraft) {
        _draft = State(initialValue: draft)
        _tab = State(initialValue: draft.startTab)
    }

    private var side: Int { min(tab, draft.sides.count - 1) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(draft.isNew ? "Modify this call" : "Edit modify rule").font(.system(size: 16, weight: .semibold))
            TextField("Rule name", text: $draft.name).textFieldStyle(.roundedBorder)
            matchRow
            if draft.sides.count > 1 {
                Picker("", selection: $tab) {
                    ForEach(Array(draft.sides.enumerated()), id: \.offset) { i, s in
                        Text("\(s.label)\(s.steps.isEmpty ? "" : " (\(s.steps.count))")").tag(i)
                    }
                }
                .pickerStyle(.segmented).labelsHidden()
            } else if let s = draft.sides.first {
                Text("Changes to the \(s.label.lowercased())").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            stepsList
            addMenu
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            HStack {
                Toggle("Enabled", isOn: $draft.enabled).toggleStyle(.switch).controlSize(.small)
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save rule") { save() }.keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
            }
        }
        .padding(18)
        .frame(width: 700, height: 600)
        .background(Theme.panel)
    }

    // MARK: Match

    private var matchRow: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                TextField("Any method", text: $draft.method).frame(width: 90)
                TextField("Host, e.g. api.acme.com", text: $draft.host)
                TextField("Path, e.g. /v2/products", text: $draft.path)
            }
            .textFieldStyle(.roundedBorder).font(Theme.mono)
            Text("Applies to calls that match these (leave a field empty for any). A path matches that prefix; * works too.")
                .font(.system(size: 11)).foregroundStyle(Theme.faint)
        }
    }

    // MARK: Steps

    private var stepsList: some View {
        ScrollView {
            VStack(spacing: 6) {
                if draft.sides[side].steps.isEmpty {
                    Text("Nothing changed yet. Use “Add change” to set a header, edit a JSON field or change the status.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                        .frame(maxWidth: .infinity, minHeight: 120)
                }
                ForEach($draft.sides[side].steps) { $step in
                    StepRow(step: $step) { draft.sides[side].steps.removeAll { $0.id == step.id } }
                }
            }
            .padding(10)
        }
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line2))
    }

    private var phase: String { draft.sides[side].phase }
    private var headers: [FfiHeader] { phase == "request" ? draft.requestHeaders : draft.responseHeaders }
    private var fields: [JSONLeaf] { phase == "request" ? draft.requestFields : draft.responseFields }

    private func add(_ s: StepDraft) { draft.sides[side].steps.append(s) }

    private var addMenu: some View {
        Menu {
            Menu("Set header") {
                ForEach(Array(headers.enumerated()), id: \.offset) { _, h in
                    Button("\(h.name): \(h.value.count > 36 ? String(h.value.prefix(36)) + "…" : h.value)") {
                        var s = StepDraft(kind: "set_header"); s.name = h.name; s.value = h.value; add(s)
                    }
                }
                if !headers.isEmpty { Divider() }
                Button("New header…") { add(StepDraft(kind: "set_header")) }
            }
            Menu("Remove header") {
                ForEach(Array(headers.enumerated()), id: \.offset) { _, h in
                    Button(h.name) { var s = StepDraft(kind: "remove_header"); s.name = h.name; add(s) }
                }
                if !headers.isEmpty { Divider() }
                Button("Other header…") { add(StepDraft(kind: "remove_header")) }
            }
            Divider()
            Menu("Edit JSON field") {
                ForEach(fields) { f in
                    Button("\(f.path) = \(f.shown)") {
                        var s = StepDraft(kind: "set_json"); s.path = f.path; s.value = f.json; add(s)
                    }
                }
                if !fields.isEmpty { Divider() }
                Button("Other field…") { add(StepDraft(kind: "set_json")) }
            }
            Menu("Remove JSON field") {
                ForEach(fields) { f in Button(f.path) { var s = StepDraft(kind: "remove_json"); s.path = f.path; add(s) } }
                if !fields.isEmpty { Divider() }
                Button("Other field…") { add(StepDraft(kind: "remove_json")) }
            }
            Button("Replace text in the body…") { add(StepDraft(kind: "replace_body")) }
            Button("Replace the whole body…") { add(StepDraft(kind: "set_body")) }
            if phase != "request" {
                Divider()
                Button("Change status code…") { var s = StepDraft(kind: "set_status"); s.value = "200"; add(s) }
                Button("Delay the response…") { var s = StepDraft(kind: "delay"); s.value = "1000"; add(s) }
            }
        } label: {
            Label("Add change", systemImage: "plus.circle.fill")
        }
        .menuStyle(.borderlessButton).fixedSize()
    }

    // MARK: Save

    private func save() {
        let used = draft.sides.filter { !$0.steps.isEmpty }
        guard !used.isEmpty else { error = "Add at least one change."; return }
        do {
            for s in used {
                let suffix = draft.sides.count > 1 && used.count > 1 ? " (\(s.phase))" : ""
                let name = draft.name.trimmingCharacters(in: .whitespaces)
                let rule = FfiRewriteRule(
                    id: s.ruleId, name: (name.isEmpty ? "Modify \(draft.host)\(draft.path)" : name) + suffix,
                    enabled: draft.enabled, method: draft.method, host: draft.host, path: draft.path,
                    phase: s.phase, steps: s.steps.map(\.ffi))
                _ = try store.saveRewrite(rule)
            }
            dismiss()
        } catch {
            self.error = "\(error)"
        }
    }
}

private struct StepRow: View {
    @Binding var step: StepDraft
    var remove: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(step.title.uppercased())
                .font(.system(size: 9.5, weight: .bold))
                .foregroundStyle(Theme.accent)
                .padding(.horizontal, 6).padding(.vertical, 3)
                .background(Capsule().fill(Theme.selection))
                .frame(width: 118, alignment: .leading)
            fields
            Button(action: remove) { Image(systemName: "xmark.circle.fill") }
                .buttonStyle(.plain).foregroundStyle(Theme.muted)
        }
    }

    @ViewBuilder private var fields: some View {
        HStack(spacing: 6) {
            switch step.kind {
            case "set_header":
                TextField("Name", text: $step.name).frame(width: 170)
                Text("=").foregroundStyle(Theme.muted)
                TextField("Value", text: $step.value)
            case "remove_header":
                TextField("Name", text: $step.name)
            case "set_status":
                TextField("200", text: $step.value).frame(width: 80)
            case "set_json":
                TextField("$.path.to.field", text: $step.path).frame(width: 220)
                Text("=").foregroundStyle(Theme.muted)
                TextField("value (text, number, true, null…)", text: $step.value)
            case "remove_json":
                TextField("$.path.to.field", text: $step.path)
            case "set_body":
                TextEditor(text: $step.value)
                    .font(Theme.mono).scrollContentBackground(.hidden).padding(4)
                    .frame(height: 110)
                    .background(RoundedRectangle(cornerRadius: 6).fill(Theme.panel))
            case "replace_body":
                TextField("Find", text: $step.find)
                Image(systemName: "arrow.right").foregroundStyle(Theme.muted)
                TextField("Replace with", text: $step.replace)
                Toggle("regex", isOn: $step.regex).toggleStyle(.checkbox).font(.system(size: 11))
            default:
                TextField("ms", text: $step.value).frame(width: 90)
            }
        }
        .textFieldStyle(.roundedBorder).font(Theme.mono).controlSize(.small)
    }
}
