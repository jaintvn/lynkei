import AppKit
import Foundation
import RelayKit
import UniformTypeIdentifiers

// A `.relay` file: a whole capture session in one file, to send to a teammate. It holds every call
// with headers, body previews, WebSocket frames and which rules touched it, plus (optionally) the
// rules, recorded scenarios and the device logs. Opening it shows the calls in the list.

struct SharedHeader: Codable { var name: String; var value: String }
struct SharedWs: Codable { var sent: Bool; var kind: String; var size: UInt64; var preview: String?; var atMs: UInt64 }

struct SharedCall: Codable {
    var id: UInt64
    var startedAtMs: UInt64
    var durationMs: UInt64
    var method: String
    var url: String
    var scheme: String
    var host: String
    var port: UInt16
    var path: String
    var status: UInt16?
    var state: String
    var error: String?
    var decrypted: Bool
    var httpVersion: String
    var upstreamHttpVersion: String?
    var requestHeaders: [SharedHeader]
    var responseHeaders: [SharedHeader]
    var requestBodySize: UInt64
    var responseBodySize: UInt64
    var requestBodyPreview: String?
    var responseBodyPreview: String?
    var clientAddr: String
    var websocketMessages: [SharedWs]
    var clientProcess: String?
    var appliedRules: [String]
}

struct SharedScenario: Codable {
    var scenario: Scenario
    /// File name → body text.
    var bodies: [String: String]
}

struct SessionFile: Codable {
    var format = "relay-session"
    var version = 1
    var name: String
    var createdAtMs: Int64
    var relayVersion: String
    var androidDevice: String?
    var secretsHidden: Bool
    var calls: [SharedCall]
    var rulesYaml: String?
    var scenarios: [SharedScenario]
    var logs: [LogLine]
}

struct SessionOptions {
    var rules = true
    var scenarios = true
    var logs = true
    var hideSecrets = true
}

@MainActor
enum SessionIO {
    static let type = UTType(exportedAs: "dev.relayproxy.session", conformingTo: .json)
    private static let secretHeaders: Set<String> = ["authorization", "proxy-authorization", "cookie", "set-cookie", "x-api-key", "x-auth-token", "x-csrf-token"]

    // MARK: Build

    @MainActor
    static func make(name: String, calls: [FfiTransaction], store: TrafficStore, options: SessionOptions) -> SessionFile {
        func headers(_ h: [FfiHeader]) -> [SharedHeader] {
            h.map { SharedHeader(name: $0.name, value: options.hideSecrets && secretHeaders.contains($0.name.lowercased()) ? "‹hidden›" : $0.value) }
        }
        let shared = calls.map { t in
            SharedCall(id: t.id, startedAtMs: t.startedAtMs, durationMs: t.durationMs, method: t.method, url: t.url, scheme: t.scheme, host: t.host,
                       port: t.port, path: t.path, status: t.status, state: stateName(t.state), error: t.error, decrypted: t.decrypted,
                       httpVersion: t.httpVersion, upstreamHttpVersion: t.upstreamHttpVersion, requestHeaders: headers(t.requestHeaders),
                       responseHeaders: headers(t.responseHeaders), requestBodySize: t.requestBodySize, responseBodySize: t.responseBodySize,
                       requestBodyPreview: t.requestBodyPreview, responseBodyPreview: t.responseBodyPreview, clientAddr: t.clientAddr,
                       websocketMessages: t.websocketMessages.map { SharedWs(sent: $0.sent, kind: $0.kind, size: $0.size, preview: $0.preview, atMs: $0.atMs) },
                       clientProcess: t.clientProcess, appliedRules: t.appliedRules)
        }
        var scenarios: [SharedScenario] = []
        if options.scenarios {
            ScenarioStore.shared.reload()
            for s in ScenarioStore.shared.scenarios {
                var bodies: [String: String] = [:]
                for x in s.exchanges {
                    let url = TrafficStore.dataDirectory.appendingPathComponent("scenarios/\(s.name)/\(x.file)")
                    bodies[x.file] = try? String(contentsOf: url, encoding: .utf8)
                }
                scenarios.append(SharedScenario(scenario: s, bodies: bodies))
            }
        }
        var logs: [LogLine] = []
        if options.logs {
            // Only the lines around the shared calls, so the file stays small.
            let spans = calls.map { (Double($0.startedAtMs) - 2000, Double($0.startedAtMs + $0.durationMs) + 2000) }
            logs = LogStore.shared.lines.filter { l in spans.contains { l.timeMs >= $0.0 && l.timeMs <= $0.1 } }
        }
        let yaml = options.rules ? store.rulesYAML() : nil
        return SessionFile(name: name, createdAtMs: Int64(Analytics.nowMs()), relayVersion: relayCoreVersion(), androidDevice: Analytics.androidDevice,
                           secretsHidden: options.hideSecrets, calls: shared, rulesYaml: yaml, scenarios: scenarios, logs: logs)
    }

    private static func stateName(_ s: FfiState) -> String {
        switch s {
        case .complete: "complete"
        case .failed: "failed"
        case .tunneled: "tunneled"
        case .inProgress: "inProgress"
        }
    }

    private static func state(_ s: String) -> FfiState {
        switch s {
        case "failed": .failed
        case "tunneled": .tunneled
        case "inProgress": .failed  // a call that was still open when shared never finished
        default: .complete
        }
    }

    // MARK: Read

    /// The calls of a session file as list rows, with fresh ids so they never clash with live traffic.
    static func transactions(_ f: SessionFile, firstId: UInt64) -> [FfiTransaction] {
        f.calls.enumerated().map { i, c in
            FfiTransaction(
                id: firstId + UInt64(i), startedAtMs: c.startedAtMs, durationMs: c.durationMs, method: c.method, url: c.url, scheme: c.scheme,
                host: c.host, port: c.port, path: c.path, status: c.status, state: state(c.state), error: c.error, decrypted: c.decrypted,
                httpVersion: c.httpVersion, upstreamHttpVersion: c.upstreamHttpVersion,
                requestHeaders: c.requestHeaders.map { FfiHeader(name: $0.name, value: $0.value) },
                responseHeaders: c.responseHeaders.map { FfiHeader(name: $0.name, value: $0.value) },
                requestBodySize: c.requestBodySize, responseBodySize: c.responseBodySize, requestBodyPreview: c.requestBodyPreview,
                responseBodyPreview: c.responseBodyPreview, clientAddr: c.clientAddr,
                websocketMessages: c.websocketMessages.map { FfiWsMessage(sent: $0.sent, kind: $0.kind, size: $0.size, preview: $0.preview, atMs: $0.atMs) },
                clientProcess: c.clientProcess, appliedRules: c.appliedRules)
        }
    }

    /// Writes the session's scenarios into this Mac's scenario folder (they are not switched on).
    static func addScenarios(_ f: SessionFile) -> Int {
        var added = 0
        let root = TrafficStore.dataDirectory.appendingPathComponent("scenarios", isDirectory: true)
        for s in f.scenarios {
            let name = ScenarioStore.cleanName(s.scenario.name)
            guard !name.isEmpty else { continue }
            let dir = root.appendingPathComponent(name, isDirectory: true)
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            for (file, body) in s.bodies where !file.contains("/") && !file.contains("..") {
                try? body.write(to: dir.appendingPathComponent(file), atomically: true, encoding: .utf8)
            }
            var sc = s.scenario
            sc.name = name
            let enc = JSONEncoder()
            enc.outputFormatting = [.prettyPrinted, .sortedKeys]
            if let data = try? enc.encode(sc), (try? data.write(to: dir.appendingPathComponent("scenario.json"))) != nil { added += 1 }
        }
        ScenarioStore.shared.reload()
        return added
    }

    /// Adds the rules of a shared session that this Mac does not have yet, switched off so nothing changes until you look.
    static func mergeRules(_ yaml: String, into current: String) -> (yaml: String, added: Int) {
        func items(_ y: String) -> [[String]] {
            var out: [[String]] = []
            for line in y.components(separatedBy: "\n") {
                if line.hasPrefix("- ") { out.append([line]) } else if !out.isEmpty, line.hasPrefix(" ") { out[out.count - 1].append(line) }
            }
            return out
        }
        func id(_ item: [String]) -> String? { item.first { $0.hasPrefix("  id: ") || $0.hasPrefix("- id: ") }.map { String($0.drop { $0 != ":" }.dropFirst().trimmingCharacters(in: .whitespaces)) } }
        let have = Set(items(current).compactMap(id))
        var block = ""
        var added = 0
        for item in items(yaml) {
            guard let i = id(item), !have.contains(i) else { continue }
            added += 1
            block += item.map { $0 == "  enabled: true" ? "  enabled: false" : ($0 == "- enabled: true" ? "- enabled: false" : $0) }.joined(separator: "\n") + "\n"
        }
        return (added == 0 ? current : ScenarioStore.inserting(block, into: current.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "rules:\n" : current), added)
    }

    // MARK: Panels

    @MainActor
    static func share(store: TrafficStore) {
        let calls = store.filtered
        guard !calls.isEmpty else { return alert("Nothing to share", "There are no calls in the list yet.") }
        // For scripted runs: RELAY_SHARE_TO=<file> saves with the default options and no dialogs.
        if let path = ProcessInfo.processInfo.environment["RELAY_SHARE_TO"] {
            let file = make(name: "scripted", calls: calls, store: store, options: SessionOptions())
            try? JSONEncoder().encode(file).write(to: URL(fileURLWithPath: path), options: .atomic)
            return
        }
        let a = NSAlert()
        a.messageText = "Share this session"
        a.informativeText = "Saves \(calls.count) call\(calls.count == 1 ? "" : "s") with headers, bodies and timings to one .relay file. Anyone with Relay can open it and see everything."
        let boxes: [(String, Bool)] = [("Hide secrets (Authorization, Cookie, API keys)", true), ("Include my rules", true),
                                       ("Include recorded scenarios", true), ("Include device logs around the calls", true)]
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        let buttons = boxes.map { title, on -> NSButton in
            let b = NSButton(checkboxWithTitle: title, target: nil, action: nil)
            b.state = on ? .on : .off
            stack.addArrangedSubview(b)
            return b
        }
        stack.frame = NSRect(x: 0, y: 0, width: 330, height: 100)
        a.accessoryView = stack
        a.addButton(withTitle: "Save…")
        a.addButton(withTitle: "Cancel")
        guard a.runModal() == .alertFirstButtonReturn else { return }
        let opts = SessionOptions(rules: buttons[1].state == .on, scenarios: buttons[2].state == .on, logs: buttons[3].state == .on, hideSecrets: buttons[0].state == .on)
        let panel = NSSavePanel()
        panel.allowedContentTypes = [type]
        let df = DateFormatter()
        df.dateFormat = "yyyy-MM-dd-HHmm"
        panel.nameFieldStringValue = "relay-session-\(df.string(from: Date())).relay"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let file = make(name: url.deletingPathExtension().lastPathComponent, calls: calls, store: store, options: opts)
            let enc = JSONEncoder()
            enc.outputFormatting = [.sortedKeys]
            try enc.encode(file).write(to: url, options: .atomic)
            NSWorkspace.shared.activateFileViewerSelecting([url])
        } catch {
            alert("Could not save", error.localizedDescription)
        }
    }

    @MainActor
    static func open(store: TrafficStore) {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [type, .json]
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        open(url, store: store)
    }

    @MainActor
    static func open(_ url: URL, store: TrafficStore) {
        do {
            let file = try JSONDecoder().decode(SessionFile.self, from: Data(contentsOf: url))
            guard file.format == "relay-session" else { throw NSError(domain: "Relay", code: 2, userInfo: [NSLocalizedDescriptionKey: "This is not a Relay session file"]) }
            let n = store.addSession(file)
            var extra: [String] = []
            let newRules = file.rulesYaml.map { mergeRules($0, into: store.rulesYAML()).added } ?? 0
            if newRules > 0 || !file.scenarios.isEmpty {
                let a = NSAlert()
                a.messageText = "Also add the rules and scenarios?"
                a.informativeText = "This session also has \(newRules) new rule\(newRules == 1 ? "" : "s") and \(file.scenarios.count) scenario\(file.scenarios.count == 1 ? "" : "s"). Rules are added switched off; scenarios are not replayed until you press Replay."
                a.addButton(withTitle: "Add them")
                a.addButton(withTitle: "Not now")
                if a.runModal() == .alertFirstButtonReturn {
                    if newRules > 0, let y = file.rulesYaml {
                        let merged = mergeRules(y, into: store.rulesYAML())
                        _ = store.applyRulesYAML(merged.yaml)
                        extra.append("\(merged.added) rules")
                    }
                    if !file.scenarios.isEmpty { extra.append("\(addScenarios(file)) scenarios") }
                }
            }
            store.ruleMessage = "Opened “\(file.name)”: \(n) calls" + (extra.isEmpty ? "" : " and \(extra.joined(separator: ", "))") + (file.secretsHidden ? " (secrets were hidden by the sender)" : "")
        } catch {
            alert("Could not open the session", error.localizedDescription)
        }
    }

    private static func alert(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text
        a.runModal()
    }
}

extension TrafficStore {
    /// Puts a shared session's calls in the list (above live traffic ids) and its logs in the log store.
    func addSession(_ f: SessionFile) -> Int {
        let base = max(transactions.map(\.id).max() ?? 0, 1 << 40) + 1
        let rows = SessionIO.transactions(f, firstId: base)
        for tx in rows { appendImported(tx) }
        if Analytics.androidDevice == nil { Analytics.androidDevice = f.androidDevice }
        LogStore.shared.importLines(f.logs)
        selection = rows.first?.id
        return rows.count
    }
}
