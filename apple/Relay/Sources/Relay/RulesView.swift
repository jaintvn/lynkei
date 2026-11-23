import RelayKit
import SwiftUI

/// Lists the rules, switches them on and off, and edits the YAML file directly.
struct RulesView: View {
    @Environment(TrafficStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var showYAML = false
    @State private var yaml = ""
    @State private var problems: [String] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Rules").font(.system(size: 16, weight: .semibold))
                Spacer()
                Button(showYAML ? "Show list" : "Edit YAML") {
                    if !showYAML { yaml = store.rulesYAML() }
                    problems = []
                    showYAML.toggle()
                }
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            if showYAML { yamlEditor } else { list }
        }
        .padding(18)
        .frame(width: 640, height: 480)
        .background(Theme.panel)
        .onAppear { store.refreshRules() }
    }

    private var list: some View {
        Group {
            if store.rules.isEmpty {
                VStack(spacing: 6) {
                    Text("No rules yet").font(.system(size: 14, weight: .medium))
                    Text("Right-click a request and choose Block, Breakpoint, Throttle or Map local, or write rules as YAML.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(store.rules, id: \.id) { r in
                            HStack(spacing: 10) {
                                Toggle("", isOn: Binding(get: { r.enabled }, set: { store.setRule(r.id, enabled: $0) }))
                                    .labelsHidden().toggleStyle(.switch).controlSize(.small)
                                Text(r.kind.uppercased())
                                    .font(.system(size: 10, weight: .semibold))
                                    .padding(.horizontal, 6).padding(.vertical, 2)
                                    .background(Capsule().fill(Theme.selection))
                                    .foregroundStyle(Theme.accent)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(r.name.isEmpty ? r.id : r.name).font(.system(size: 13))
                                    Text(r.summary).font(Theme.mono).foregroundStyle(Theme.muted).lineLimit(1)
                                }
                                Spacer()
                                Button { store.removeRule(r.id) } label: { Image(systemName: "trash") }
                                    .buttonStyle(.plain).foregroundStyle(Theme.muted)
                            }
                            .opacity(r.enabled ? 1 : 0.5)
                            .padding(.vertical, 8)
                            Divider().overlay(Theme.line)
                        }
                    }
                }
            }
        }
    }

    private var yamlEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextEditor(text: $yaml)
                .font(Theme.mono)
                .scrollContentBackground(.hidden)
                .padding(6)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.line2))
            ForEach(problems, id: \.self) { Text($0).font(.system(size: 12)).foregroundStyle(Theme.s5xx) }
            HStack {
                Spacer()
                Button("Apply") {
                    problems = store.applyRulesYAML(yaml)
                    if problems.isEmpty { showYAML = false }
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }
}
