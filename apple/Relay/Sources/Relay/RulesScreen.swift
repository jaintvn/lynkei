import RelayKit
import SwiftUI

/// Rule builder (canvas board 6): rules grouped by kind on the left, the selected rule as
/// When / Then in the middle, a live preview of what it matches on the right.
struct RulesScreen: View {
    @Environment(TrafficStore.self) private var store
    @Environment(Sheets.self) private var sheets
    @Binding var screen: Screen

    @State private var selected: String?
    @State private var draft: RewriteDraft?
    @State private var showYAML = false
    @State private var yaml = ""
    @State private var problems: [String] = []
    @State private var error: String?

    private var rule: FfiRule? { store.rules.first { $0.id == selected } }

    var body: some View {
        VStack(spacing: 0) {
            AppHeader(screen: $screen) {
                Spacer(minLength: 0)
                Text("Saved automatically · \(Text("rules.yaml").font(.system(size: 12, design: .monospaced)))")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                Button(showYAML ? "Show builder" : "Edit YAML") {
                    if !showYAML { yaml = store.rulesYAML() }
                    problems = []
                    showYAML.toggle()
                }
                .buttonStyle(PillButtonStyle())
            }
            HStack(spacing: 0) {
                list
                    .frame(width: 260)
                    .background(Theme.panel)
                    .overlay(alignment: .trailing) { Rectangle().fill(Theme.line).frame(width: 1) }
                if showYAML {
                    yamlEditor.frame(maxWidth: .infinity)
                } else {
                    editor.frame(maxWidth: .infinity)
                    preview
                        .frame(width: 380)
                        .background(Theme.panel)
                        .overlay(alignment: .leading) { Rectangle().fill(Theme.line).frame(width: 1) }
                }
            }
        }
        .onAppear {
            store.refreshRules()
            if let id = sheets.ruleToOpen { select(id); sheets.ruleToOpen = nil }
            else if selected == nil, let first = store.rules.first { select(first.id) }
        }
    }

    // MARK: Left: list

    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Button { newRule() } label: { Text("+ New rule").frame(maxWidth: .infinity) }
                    .buttonStyle(AccentButtonStyle(height: 32))
                ForEach(RuleStyle.order, id: \.self) { kind in
                    let items = store.rules.filter { $0.kind == kind }
                    if !items.isEmpty {
                        VStack(alignment: .leading, spacing: 2) {
                            SectionLabel(RuleStyle.group(kind)).padding(.leading, 8).padding(.bottom, 4)
                            ForEach(items, id: \.id) { r in
                                let on = r.id == selected
                                HStack(spacing: 8) {
                                    Text(r.name.isEmpty ? r.summary : r.name)
                                        .font(.system(size: 12.5)).lineLimit(1)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                    RelaySwitch(isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                                        .scaleEffect(0.87)
                                }
                                .padding(.horizontal, 8).padding(.vertical, 7)
                                .background(RoundedRectangle(cornerRadius: 7).fill(on ? Theme.selection : .clear))
                                .overlay(alignment: .leading) { if on { Rectangle().fill(Theme.accent).frame(width: 2) } }
                                .contentShape(Rectangle())
                                .onTapGesture { select(r.id) }
                            }
                        }
                    }
                }
                if store.rules.isEmpty {
                    Text("No rules yet. Right-click a call in Traffic, or press New rule.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.horizontal, 8)
                }
            }
            .padding(.horizontal, 10).padding(.vertical, 14)
        }
    }

    // MARK: Middle: editor

    @ViewBuilder private var editor: some View {
        if draft != nil {
            rewriteEditor
        } else if let rule {
            otherEditor(rule)
        } else {
            VStack(spacing: 8) {
                Text("Pick a rule").font(.system(size: 16, weight: .semibold))
                Text("or create one with New rule.").foregroundStyle(Theme.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var kindTray: some View {
        PillGroup(
            items: ["rewrite", "map local", "map remote", "breakpoint", "block", "throttle"],
            title: { $0 == "breakpoint" ? "Breakpoint" : RuleStyle.group($0) },
            selection: .constant(draft != nil ? "rewrite" : (rule?.kind ?? "rewrite")))
            .allowsHitTesting(false)
            .fixedSize()
    }

    private var rewriteEditor: some View {
        let d = Binding(get: { draft ?? RewriteDraft() }, set: { draft = $0 })
        return ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack(spacing: 12) {
                    TextField("Rule name", text: d.name)
                        .textFieldStyle(.plain)
                        .font(.system(size: 20, weight: .semibold))
                    kindTray
                }

                VStack(alignment: .leading, spacing: 12) {
                    SectionLabel("When")
                    HStack(spacing: 8) {
                        whenField("Method", text: d.method, placeholder: "Any", width: 70)
                        whenField("Host", text: d.host, placeholder: "Any", width: 190)
                        whenField("Path", text: d.path, placeholder: "Any", width: 250)
                    }
                    Text("Leave a field empty for any. A path matches that prefix; * works too.")
                        .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
                }
                .card(padding: 18).frame(maxWidth: .infinity, alignment: .leading)

                VStack(alignment: .leading, spacing: 10) {
                    SectionLabel("Then")
                    if (draft?.sides.first?.steps.isEmpty ?? true) {
                        Text("Nothing changes yet. Add a change below.").font(.system(size: 12)).foregroundStyle(Theme.muted)
                    }
                    ForEach(d.sides[0].steps) { $step in
                        let n = (d.wrappedValue.sides[0].steps.firstIndex { $0.id == step.id } ?? 0) + 1
                        ActionRow(n: n, step: $step) { d.wrappedValue.sides[0].steps.removeAll { $0.id == step.id } }
                    }
                    FlowLayout(spacing: 6) {
                        Text("Add:").font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.trailing, 4)
                        ForEach(Self.addable, id: \.title) { a in
                            Button { d.wrappedValue.sides[0].steps.append(a.make()) } label: {
                                Text(a.title)
                                    .font(.system(size: 12)).padding(.horizontal, 10).frame(height: 28)
                                    .overlay(Capsule().stroke(Theme.line2))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.top, 4)
                }
                .card(padding: 18).frame(maxWidth: .infinity, alignment: .leading)

                if let error { Text(error).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }

                HStack(spacing: 8) {
                    Menu {
                        ForEach(["request", "response", "both"], id: \.self) { p in
                            Button(p.capitalized) { d.wrappedValue.sides[0].phase = p }
                        }
                    } label: {
                        Text("Applies on: \(Text(d.wrappedValue.sides[0].phase.capitalized).foregroundStyle(Theme.fg).fontWeight(.medium))")
                            .font(.system(size: 12)).foregroundStyle(Theme.muted)
                    }
                    .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
                    Spacer()
                    if let id = selected {
                        Button("Delete") { store.removeRule(id); selected = nil; draft = nil; if let f = store.rules.first { select(f.id) } }
                            .buttonStyle(.plain).foregroundStyle(Theme.s5xx).font(.system(size: 12, weight: .medium))
                    }
                    Button("Duplicate") { saveRewrite(asCopy: true) }
                        .buttonStyle(.plain).font(.system(size: 13, weight: .medium))
                        .padding(.horizontal, 14).frame(height: 34)
                        .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.line2))
                    Button("Save & enable") { saveRewrite(asCopy: false) }
                        .buttonStyle(AccentButtonStyle(height: 34))
                        .keyboardShortcut(.defaultAction)
                }
            }
            .padding(.horizontal, 26).padding(.vertical, 22)
        }
    }

    private func whenField(_ label: String, text: Binding<String>, placeholder: String, width: CGFloat) -> some View {
        HStack(spacing: 6) {
            Text(label).font(.system(size: 11.5)).foregroundStyle(Theme.muted)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .font(.system(size: 12, weight: .medium, design: .monospaced))
                .frame(width: width)
        }
        .padding(.horizontal, 10).frame(height: 32)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
    }

    private func otherEditor(_ r: FfiRule) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack(spacing: 12) {
                    Text(r.name.isEmpty ? r.id : r.name).font(.system(size: 20, weight: .semibold)).lineLimit(1)
                    Spacer()
                    kindTray
                }
                VStack(alignment: .leading, spacing: 12) {
                    SectionLabel("When")
                    Text(r.summary)
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .padding(.horizontal, 10).frame(height: 32)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
                }
                .card(padding: 18).frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 10) {
                    SectionLabel("Then")
                    Text(thenText(r)).font(.system(size: 13))
                    if r.kind == "map local" { MapLocalFile(rule: r) }
                    Text("Change the details in Edit YAML. New rules of this kind are made by right-clicking a call in Traffic.")
                        .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
                }
                .card(padding: 18).frame(maxWidth: .infinity, alignment: .leading)
                HStack {
                    Text("Switch").font(.system(size: 12)).foregroundStyle(Theme.muted)
                    RelaySwitch(isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                    Spacer()
                    Button("Delete") { store.removeRule(r.id); selected = nil; if let f = store.rules.first { select(f.id) } }
                        .buttonStyle(.plain).foregroundStyle(Theme.s5xx).font(.system(size: 12, weight: .medium))
                }
            }
            .padding(.horizontal, 26).padding(.vertical, 22)
        }
    }

    private func thenText(_ r: FfiRule) -> String {
        switch r.kind {
        case "breakpoint": return "Pause matching calls so you can edit them before they continue."
        case "block": return "Refuse matching calls."
        case "map local": return "Answer from a local file."
        case "map remote": return "Send to another server."
        case "throttle": return "Slow down or drop matching calls."
        case "dns spoof": return "Look the host up at another address."
        default: return r.detail
        }
    }

    // MARK: Right: live preview

    private func matches() -> [FfiTransaction] {
        if let d = draft {
            let s = d.sides.first
            _ = s
            return store.transactions.filter { t in
                (d.method.isEmpty || t.method.caseInsensitiveCompare(d.method) == .orderedSame)
                    && (d.host.isEmpty || t.host.caseInsensitiveCompare(d.host) == .orderedSame)
                    && (d.path.isEmpty || t.path.hasPrefix(d.path.replacingOccurrences(of: "*", with: "")))
            }
        }
        guard let id = selected else { return [] }
        return store.transactions.filter { $0.appliedRules.contains(id) }
    }

    private var preview: some View {
        let hits = matches()
        return ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text("Live preview").font(.system(size: 13, weight: .semibold))
                    Text("\(hits.count) of the last \(store.transactions.count.formatted()) requests match")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
                if hits.isEmpty { Text("Nothing matches yet.").font(.system(size: 12)).foregroundStyle(Theme.muted) }
                ForEach(Array(hits.suffix(4).reversed())) { t in
                    HStack(spacing: 8) {
                        StatusPill(tx: t)
                        Text("\(t.method) \(t.path)").font(.system(size: 12, design: .monospaced)).lineLimit(1)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text("\(Analytics.deviceLabel(t)) · \(Analytics.ago(t.startedAtMs))")
                            .font(.system(size: 11.5)).foregroundStyle(Theme.muted).lineLimit(1)
                    }
                    .padding(.horizontal, 10).padding(.vertical, 8)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                }
                Text("Diff on latest match").font(.system(size: 13, weight: .semibold)).padding(.top, 6)
                diff(for: hits.last)
            }
            .padding(20)
        }
    }

    private func diff(for tx: FfiTransaction?) -> some View {
        let lines = diffLines(tx)
        return VStack(alignment: .leading, spacing: 0) {
            if lines.isEmpty {
                Text("Add a change to see before and after.").font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.horizontal, 12)
            }
            ForEach(Array(lines.enumerated()), id: \.offset) { _, l in
                Text(l.text)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(l.kind == 0 ? Theme.muted : l.kind < 0 ? Theme.s5xx : Theme.s2xx)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12).padding(.vertical, 3)
                    .background((l.kind < 0 ? Theme.s5xx : l.kind > 0 ? Theme.s2xx : Color.clear).opacity(0.12))
            }
        }
        .padding(.vertical, 10)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
    }

    private func diffLines(_ tx: FfiTransaction?) -> [(kind: Int, text: String)] {
        guard let tx, let d = draft, let side = d.sides.first else { return [] }
        let leaves = jsonLeaves(side.phase == "request" ? tx.requestBodyPreview : tx.responseBodyPreview, limit: 300)
        let headers = side.phase == "request" ? tx.requestHeaders : tx.responseHeaders
        var out: [(Int, String)] = []
        for s in side.steps {
            switch s.kind {
            case "set_json":
                if let old = leaves.first(where: { $0.path == s.path }) { out.append((-1, "-  \(s.path): \(old.json)")) }
                out.append((1, "+  \(s.path): \(s.value)"))
            case "remove_json":
                if let old = leaves.first(where: { $0.path == s.path }) { out.append((-1, "-  \(s.path): \(old.json)")) }
            case "set_header":
                if let old = headers.first(where: { $0.name.lowercased() == s.name.lowercased() }) { out.append((-1, "-  \(old.name): \(old.value)")) }
                out.append((1, "+  \(s.name): \(s.value)"))
            case "remove_header":
                if let old = headers.first(where: { $0.name.lowercased() == s.name.lowercased() }) { out.append((-1, "-  \(old.name): \(old.value)")) }
            case "set_status":
                out.append((-1, "-  status: \(tx.status.map(String.init) ?? "—")"))
                out.append((1, "+  status: \(s.value)"))
            case "delay":
                out.append((1, "+  delay: \(s.value) ms"))
            default:
                out.append((1, "~  \(s.title)"))
            }
        }
        return out
    }

    // MARK: YAML

    private var yamlEditor: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("rules.yaml").font(.system(size: 13, weight: .semibold))
            TextEditor(text: $yaml)
                .font(Theme.mono)
                .scrollContentBackground(.hidden)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line2))
            ForEach(problems, id: \.self) { Text($0).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            HStack {
                Spacer()
                Button("Apply") {
                    problems = store.applyRulesYAML(yaml)
                    if problems.isEmpty { showYAML = false; if let f = store.rules.first { select(f.id) } }
                }
                .buttonStyle(AccentButtonStyle(height: 34))
            }
        }
        .padding(26)
    }

    // MARK: Actions

    private func select(_ id: String) {
        selected = id
        error = nil
        if let r = store.rules.first(where: { $0.id == id }), r.kind == "rewrite", let rule = store.rewriteRule(id) {
            draft = RewriteDraft(rule: rule)
        } else {
            draft = nil
        }
    }

    private func newRule() {
        var d = RewriteDraft()
        d.name = "New rewrite"
        d.sides = [RewriteDraft.Side(phase: "response")]
        selected = nil
        error = nil
        draft = d
    }

    private func saveRewrite(asCopy: Bool) {
        guard var d = draft, let side = d.sides.first else { return }
        guard !side.steps.isEmpty else { error = "Add at least one change."; return }
        let name = d.name.trimmingCharacters(in: .whitespaces)
        let rule = FfiRewriteRule(
            id: asCopy ? "" : side.ruleId, name: (name.isEmpty ? "Rewrite \(d.host)\(d.path)" : name) + (asCopy ? " copy" : ""),
            enabled: true, method: d.method, host: d.host, path: d.path, phase: side.phase, steps: side.steps.map(\.ffi))
        do {
            let id = try store.saveRewrite(rule)
            d.sides[0].ruleId = id
            select(id)
        } catch {
            self.error = "\(error)"
        }
    }

    private static let addable: [(title: String, make: () -> StepDraft)] = [
        ("Set JSON value", { StepDraft(kind: "set_json") }),
        ("Set header", { StepDraft(kind: "set_header") }),
        ("Remove JSON key", { StepDraft(kind: "remove_json") }),
        ("Remove header", { StepDraft(kind: "remove_header") }),
        ("Override status", { var s = StepDraft(kind: "set_status"); s.value = "200"; return s }),
        ("Replace body text", { StepDraft(kind: "replace_body") }),
        ("Delay response", { var s = StepDraft(kind: "delay"); s.value = "1000"; return s }),
    ]
}

/// One "Then" row: number, what it does, the two values, remove.
private struct ActionRow: View {
    let n: Int
    @Binding var step: StepDraft
    var remove: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Text("\(n)").font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.faint).frame(width: 24, alignment: .leading)
            Text(step.title).font(.system(size: 13, weight: .medium)).frame(width: 170, alignment: .leading)
            switch step.kind {
            case "set_header": field($step.name, "header", Theme.key); field($step.value, "value", Theme.str)
            case "remove_header": field($step.name, "header", Theme.key); Color.clear
            case "set_status": field($step.value, "200", Theme.num); Color.clear
            case "set_json": field($step.path, "$.path.to.field", Theme.key); field($step.value, "value", Theme.num)
            case "remove_json": field($step.path, "$.path.to.field", Theme.key); Color.clear
            case "replace_body": field($step.find, "find", Theme.key); field($step.replace, "replace with", Theme.str)
            case "set_body": field($step.value, "new body", Theme.str); Color.clear
            default: field($step.value, "ms", Theme.num); Color.clear
            }
            Button(action: remove) { Text("×").font(.system(size: 16)).foregroundStyle(Theme.muted).frame(width: 26, height: 26) }
                .buttonStyle(.plain).help("Remove")
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
    }

    private func field(_ text: Binding<String>, _ placeholder: String, _ color: Color) -> some View {
        TextField(placeholder, text: text)
            .textFieldStyle(.plain)
            .font(.system(size: 12, design: .monospaced))
            .foregroundStyle(color)
            .padding(.horizontal, 9).padding(.vertical, 6)
            .background(RoundedRectangle(cornerRadius: 7).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 7).stroke(Theme.line))
            .frame(maxWidth: .infinity)
    }
}
