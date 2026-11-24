import AppKit
import Foundation
import Observation
import LynkeiKit
import SwiftUI

// Scenarios: record the calls of a flow (login, then feed) and replay them later as mocks, with no
// server. Same files as the MCP server writes: `scenarios/<name>/scenario.json` + one body per call,
// replayed as rules with ids `scn-<name>-N`.

struct ScenarioExchange: Codable {
    var method: String
    var host: String
    var path: String
    var status: Int
    var headers: [String: String]
    var file: String
}

struct Scenario: Codable, Identifiable {
    var name: String
    var recorded_at_ms: Int64
    var exchanges: [ScenarioExchange]
    var id: String { name }
}

@MainActor
@Observable
final class ScenarioStore {
    static let shared = ScenarioStore()

    private(set) var scenarios: [Scenario] = []
    /// Set while recording: only calls that start after this are kept.
    private(set) var recordingSince: UInt64?
    var message: String?

    private static let framing: Set<String> = ["content-length", "transfer-encoding", "content-encoding", "connection", "keep-alive", "date"]

    private var root: URL { TrafficStore.dataDirectory.appendingPathComponent("scenarios", isDirectory: true) }

    func reload() {
        let dirs = (try? FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)) ?? []
        scenarios = dirs.compactMap { d in
            (try? Data(contentsOf: d.appendingPathComponent("scenario.json"))).flatMap { try? JSONDecoder().decode(Scenario.self, from: $0) }
        }.sorted { $0.recorded_at_ms > $1.recorded_at_ms }
    }

    // MARK: Recording

    func startRecording() {
        recordingSince = Analytics.nowMs()
        message = nil
    }

    func cancelRecording() { recordingSince = nil }

    func recordedCalls(_ store: TrafficStore) -> [FfiTransaction] {
        guard let since = recordingSince else { return [] }
        return store.transactions.filter { $0.startedAtMs >= since && $0.state == .complete && $0.status != nil }
    }

    /// Saves the calls made since recording began. Returns a one-line summary for the user.
    @discardableResult
    func finishRecording(_ store: TrafficStore, name: String) throws -> String {
        let calls = recordedCalls(store)
        recordingSince = nil
        return try save(name: name, from: calls)
    }

    static func cleanName(_ raw: String) -> String {
        let s = raw.trimmingCharacters(in: .whitespaces).map { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" ? String($0) : "-" }.joined()
        return String(s.prefix(64))
    }

    func save(name rawName: String, from txs: [FfiTransaction]) throws -> String {
        let name = Self.cleanName(rawName)
        guard !name.isEmpty else { throw NSError(domain: "Lynkei", code: 1, userInfo: [NSLocalizedDescriptionKey: "Give the scenario a name"]) }
        let dir = root.appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.removeItem(at: dir)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var seen = Set<String>()
        var picked: [(FfiTransaction, String, String)] = []  // newest first: keep the newest answer per endpoint
        var skipped = 0
        for t in txs.reversed() where t.state == .complete {
            let path = String(t.path.split(separator: "?", omittingEmptySubsequences: false).first ?? "/")
            guard seen.insert("\(t.method) \(t.host) \(path)").inserted else { continue }
            let body: String
            if GRPC.isGRPC(t) { skipped += 1; continue }
            if let b = t.responseBodyPreview {
                if b.contains("…[truncated") { skipped += 1; continue }
                body = b
            } else if t.responseBodySize == 0 {
                body = ""
            } else { skipped += 1; continue }
            picked.append((t, path, body))
        }
        var exchanges: [ScenarioExchange] = []
        for (n, item) in picked.reversed().enumerated() {
            let (t, path, body) = item
            try body.write(to: dir.appendingPathComponent("\(n).body"), atomically: true, encoding: .utf8)
            var headers: [String: String] = [:]
            for h in t.responseHeaders where !Self.framing.contains(h.name.lowercased()) { headers[h.name] = h.value }
            exchanges.append(ScenarioExchange(method: t.method, host: t.host, path: path, status: Int(t.status ?? 200), headers: headers, file: "\(n).body"))
        }
        let doc = Scenario(name: name, recorded_at_ms: Int64(Analytics.nowMs()), exchanges: exchanges)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(doc).write(to: dir.appendingPathComponent("scenario.json"))
        reload()
        return "Saved “\(name)” with \(exchanges.count) call\(exchanges.count == 1 ? "" : "s")" + (skipped > 0 ? " (\(skipped) skipped: binary, large or repeated)" : "")
    }

    // MARK: Replay

    func isReplaying(_ name: String, rules: [FfiRule]) -> Bool {
        rules.contains { $0.id.hasPrefix("scn-\(name)-") }
    }

    func replay(_ name: String, store: TrafficStore) {
        guard let s = scenarios.first(where: { $0.name == name }) else { return }
        var block = ""
        for (i, x) in s.exchanges.enumerated() { block += Self.ruleYAML(scenario: name, index: i, x) }
        let yaml = Self.inserting(block, into: Self.removing(prefix: "scn-\(name)-", from: store.rulesYAML()))
        let problems = store.applyRulesYAML(yaml)
        message = problems.isEmpty ? "Replaying “\(name)”: \(s.exchanges.count) calls are answered from the recording" : "Could not replay: \(problems.joined(separator: "; "))"
    }

    func stopReplay(_ name: String, store: TrafficStore) {
        let problems = store.applyRulesYAML(Self.removing(prefix: "scn-\(name)-", from: store.rulesYAML()))
        message = problems.isEmpty ? "Stopped “\(name)”: calls go to the real server again" : "Could not stop: \(problems.joined(separator: "; "))"
    }

    func delete(_ name: String, store: TrafficStore) {
        stopReplay(name, store: store)
        try? FileManager.default.removeItem(at: root.appendingPathComponent(name))
        reload()
    }

    private static func q(_ s: String) -> String {
        (try? String(data: JSONEncoder().encode(s), encoding: .utf8)) ?? "\"\""
    }

    private static func ruleYAML(scenario: String, index: Int, _ x: ScenarioExchange) -> String {
        var out = "- enabled: true\n  file: \(q("scenarios/\(scenario)/\(x.file)"))\n"
        if !x.headers.isEmpty {
            out += "  headers:\n"
            for k in x.headers.keys.sorted() { out += "    \(q(k)): \(q(x.headers[k]!))\n" }
        }
        out += "  id: scn-\(scenario)-\(index)\n  kind: map_local\n  match:\n    host: \(q(x.host))\n    method: \(q(x.method))\n    path: \(q(x.path))\n"
        out += "  name: \(q("Scenario \(scenario): \(x.method) \(x.path)"))\n  status: \(x.status)\n"
        return out
    }

    /// Adds rule items at the end of the `rules:` list (before any key that follows it, such as `version:`).
    static func inserting(_ block: String, into yaml: String) -> String {
        var lines = yaml.replacingOccurrences(of: "rules: []", with: "rules:").components(separatedBy: "\n")
        guard let start = lines.firstIndex(where: { $0.hasPrefix("rules:") }) else { return "rules:\n" + block + yaml }
        var end = lines.count
        for i in (start + 1)..<max(start + 1, lines.count) where !lines[i].isEmpty && !lines[i].hasPrefix("- ") && !lines[i].hasPrefix(" ") {
            end = i
            break
        }
        while end > start + 1, lines[end - 1].isEmpty { end -= 1 }
        lines.insert(contentsOf: block.components(separatedBy: "\n").dropLast(), at: end)
        return lines.joined(separator: "\n")
    }

    /// Drops the list items whose `id:` starts with `prefix` from a rules YAML (a list of `- ` items, as Lynkei writes it).
    static func removing(prefix: String, from yaml: String) -> String {
        var out: [String] = []
        var item: [String] = []
        func flush() {
            if !item.contains(where: { $0.hasPrefix("  id: \(prefix)") || $0.hasPrefix("  id: \"\(prefix)") }) { out += item }
            item = []
        }
        for line in yaml.components(separatedBy: "\n") {
            if line.hasPrefix("- ") { flush(); item = [line] }
            else if !item.isEmpty, line.isEmpty || line.hasPrefix(" ") { item.append(line) }
            else { flush(); out.append(line) }
        }
        flush()
        return out.joined(separator: "\n")
    }
}

// MARK: - UI

/// The Scenarios block at the top of the Rules list.
struct ScenariosSection: View {
    @Environment(TrafficStore.self) private var store
    private let scenarios = ScenarioStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                SectionLabel("Scenarios")
                Spacer()
                if scenarios.recordingSince == nil {
                    Button { scenarios.startRecording() } label: {
                        HStack(spacing: 4) { Circle().fill(Theme.s5xx).frame(width: 7, height: 7); Text("Record") }
                    }
                    .buttonStyle(PillButtonStyle()).font(.system(size: 11.5, weight: .medium))
                    .help("Record the calls of a flow, then replay them without a server")
                }
            }
            .padding(.leading, 8)
            if scenarios.recordingSince != nil { RecordingBar(compact: true) }
            ForEach(scenarios.scenarios) { s in
                let on = scenarios.isReplaying(s.name, rules: store.rules)
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(s.name).font(.system(size: 12.5, weight: .medium)).lineLimit(1)
                        Text("\(s.exchanges.count) calls\(on ? " · replaying" : "")").font(.system(size: 11)).foregroundStyle(on ? Theme.accent : Theme.faint)
                    }
                    Spacer()
                    Button(on ? "Stop" : "Replay") {
                        if on { scenarios.stopReplay(s.name, store: store) } else { scenarios.replay(s.name, store: store) }
                    }
                    .buttonStyle(PillButtonStyle()).font(.system(size: 11.5, weight: .medium))
                    Button { scenarios.delete(s.name, store: store) } label: { Image(systemName: "trash").font(.system(size: 11)) }
                        .buttonStyle(.plain).foregroundStyle(Theme.faint).help("Delete this scenario")
                }
                .padding(.horizontal, 8).padding(.vertical, 6)
                .background(RoundedRectangle(cornerRadius: 8).fill(on ? Theme.accent.opacity(0.10) : Theme.raised))
            }
            if scenarios.scenarios.isEmpty && scenarios.recordingSince == nil {
                Text("Record a flow once, then replay it offline for demos and UI tests.")
                    .font(.system(size: 11.5)).foregroundStyle(Theme.faint).padding(.horizontal, 8)
            }
            if let m = scenarios.message { Text(m).font(.system(size: 11.5)).foregroundStyle(Theme.muted).padding(.horizontal, 8) }
        }
        .onAppear { scenarios.reload() }
    }
}

/// Shown while a scenario is being recorded: a live count, and Stop & save.
struct RecordingBar: View {
    @Environment(TrafficStore.self) private var store
    private let scenarios = ScenarioStore.shared

    var compact = false

    var body: some View {
        let label = HStack(spacing: 8) {
            Circle().fill(Theme.s5xx).frame(width: 8, height: 8)
            Text("Recording · \(scenarios.recordedCalls(store).count) calls").font(.system(size: 12, weight: .medium)).lineLimit(1)
        }
        let buttons = HStack(spacing: 8) {
            Button("Cancel") { scenarios.cancelRecording() }.buttonStyle(.plain).foregroundStyle(Theme.muted).font(.system(size: 12))
            Button("Stop & save") { stopAndSave() }.buttonStyle(AccentButtonStyle(height: 24)).font(.system(size: 12, weight: .medium))
        }
        return Group {
            if compact {
                VStack(alignment: .leading, spacing: 6) { label; buttons }.frame(maxWidth: .infinity, alignment: .leading)
            } else {
                HStack(spacing: 8) { label; Spacer(); buttons }
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.s5xx.opacity(0.10)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.s5xx.opacity(0.35)))
    }

    private func stopAndSave() {
        let alert = NSAlert()
        alert.messageText = "Save scenario"
        alert.informativeText = "\(scenarios.recordedCalls(store).count) calls recorded. Name it so you can replay it later."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "login-and-feed"
        alert.accessoryView = field
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Keep recording")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        do { scenarios.message = try scenarios.finishRecording(store, name: field.stringValue) } catch { scenarios.message = "\(error.localizedDescription)" }
    }
}
