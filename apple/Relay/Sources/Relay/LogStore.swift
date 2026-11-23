import Foundation
import Observation
import RelayKit
import SwiftUI

/// One line from a simulator's or phone's log.
struct LogLine: Identifiable, Codable {
    var id: Int
    var timeMs: Double
    /// V, D, I, W, E
    var level: String
    var process: String
    var message: String
    var device: String
}

/// Streams the logs of booted iOS simulators (`simctl spawn … log stream`) and Android devices
/// (`adb logcat`), so the Traffic inspector can show what an app logged around a request.
@MainActor
@Observable
final class LogStore {
    static let shared = LogStore()

    private(set) var lines: [LogLine] = []
    private(set) var sources: [String] = []
    var enabled: Bool = UserDefaults.standard.object(forKey: "captureLogs") as? Bool ?? true {
        didSet {
            UserDefaults.standard.set(enabled, forKey: "captureLogs")
            if enabled { start() } else { stop() }
        }
    }
    var hasLogs: Bool { !lines.isEmpty }

    private let capacity = 40_000
    private var nextId = 0
    @ObservationIgnored private var streams: [String: LogStream] = [:]
    @ObservationIgnored private var timer: Timer?

    func start() {
        guard enabled, timer == nil else { return }
        refreshSources()
        timer = Timer.scheduledTimer(withTimeInterval: 8, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refreshSources() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        for s in streams.values { s.stop() }
        streams.removeAll()
        sources = []
    }

    func clear() { lines.removeAll() }

    /// Adds lines that came with a shared session.
    func importLines(_ batch: [LogLine]) { add(batch) }

    /// Starts a stream for each booted simulator / connected Android device, and ends those that went away.
    private func refreshSources() {
        Task.detached {
            var found: [(key: String, label: String, make: () -> LogStream)] = []
            for sim in Simulators.booted() {
                found.append(("sim:\(sim.id)", sim.name, { LogStream.simulator(udid: sim.id, name: sim.name) }))
            }
            for serial in AdbLogs.devices() {
                found.append(("adb:\(serial)", serial, { LogStream.android(serial: serial) }))
            }
            await MainActor.run { [found] in
                let keys = Set(found.map(\.key))
                for (k, s) in self.streams where !keys.contains(k) || !s.running {
                    s.stop()
                    self.streams[k] = nil
                }
                for f in found where self.streams[f.key] == nil {
                    let s = f.make()
                    s.onLines = { [weak self] batch in Task { @MainActor in self?.add(batch) } }
                    s.start()
                    self.streams[f.key] = s
                }
                self.sources = found.map(\.label)
            }
        }
    }

    fileprivate func add(_ batch: [LogLine]) {
        for var l in batch {
            l.id = nextId
            nextId += 1
            lines.append(l)
        }
        if lines.count > capacity { lines.removeFirst(lines.count - capacity) }
    }

    /// Lines from `windowMs` before the call starts to `windowMs` after it ends, oldest first.
    func around(_ tx: FfiTransaction, windowMs: Double = 2000) -> [LogLine] {
        let from = Double(tx.startedAtMs) - windowMs
        let to = Double(tx.startedAtMs + tx.durationMs) + windowMs
        return lines.filter { $0.timeMs >= from && $0.timeMs <= to }
    }

    /// True when the log line comes from the app that made the call (names compared loosely: `AcmeShop` / `AcmeShop.debug`).
    static func isFromApp(_ line: LogLine, app: String) -> Bool {
        let p = line.process.lowercased(), a = app.lowercased()
        guard !p.isEmpty, !a.isEmpty else { return false }
        return p.contains(a) || a.contains(p)
    }
}

// MARK: - Streams

private final class LogStream: @unchecked Sendable {
    var onLines: (([LogLine]) -> Void)?
    private let process = Process()
    private let pipe = Pipe()
    private let parse: (String) -> LogLine?
    private var buffer = Data()
    private let lock = NSLock()
    private(set) var running = false

    init(executable: String, arguments: [String], parse: @escaping (String) -> LogLine?) {
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = pipe
        process.standardError = Pipe()
        self.parse = parse
    }

    func start() {
        pipe.fileHandleForReading.readabilityHandler = { [weak self] h in
            let data = h.availableData
            if data.isEmpty { h.readabilityHandler = nil; self?.running = false; return }
            self?.consume(data)
        }
        process.terminationHandler = { [weak self] _ in self?.running = false }
        do { try process.run(); running = true } catch { running = false }
    }

    func stop() {
        pipe.fileHandleForReading.readabilityHandler = nil
        if process.isRunning { process.terminate() }
        running = false
    }

    private func consume(_ data: Data) {
        lock.lock()
        buffer.append(data)
        var batch: [LogLine] = []
        while let nl = buffer.firstIndex(of: 0x0A) {
            let line = buffer[buffer.startIndex..<nl]
            buffer.removeSubrange(buffer.startIndex...nl)
            if let s = String(data: line, encoding: .utf8), let l = parse(s) { batch.append(l) }
        }
        if buffer.count > 1_000_000 { buffer.removeAll() }
        lock.unlock()
        if !batch.isEmpty { onLines?(batch) }
    }

    static func simulator(udid: String, name: String) -> LogStream {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd HH:mm:ss.SSSSSSZ"
        return LogStream(executable: "/usr/bin/xcrun",
                         arguments: ["simctl", "spawn", udid, "log", "stream", "--style", "ndjson", "--level", "info"]) { text in
            guard text.hasPrefix("{"), let d = text.data(using: .utf8),
                  let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
                  let message = o["eventMessage"] as? String, !message.isEmpty,
                  let ts = (o["timestamp"] as? String).flatMap(f.date(from:)) else { return nil }
            let path = o["processImagePath"] as? String ?? ""
            let level: String
            switch o["messageType"] as? String {
            case "Debug": level = "D"
            case "Error", "Fault": level = "E"
            default: level = "I"
            }
            return LogLine(id: 0, timeMs: ts.timeIntervalSince1970 * 1000, level: level,
                           process: (path as NSString).lastPathComponent, message: message, device: name)
        }
    }

    static func android(serial: String) -> LogStream {
        guard let adb = AdbLogs.path else { return LogStream(executable: "/usr/bin/true", arguments: []) { _ in nil } }
        let re = try? NSRegularExpression(pattern: #"^\s*(\d+\.\d+)\s+(\d+)\s+\d+\s+([VDIWEF])\s+(.*?)\s*:\s(.*)$"#)
        var names: [String: String] = [:]
        var refreshedAt = Date.distantPast
        return LogStream(executable: adb, arguments: ["-s", serial, "logcat", "-v", "threadtime", "-v", "epoch", "-T", "1"]) { text in
            guard let m = re?.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) else { return nil }
            func g(_ i: Int) -> String { Range(m.range(at: i), in: text).map { String(text[$0]) } ?? "" }
            let pid = g(2)
            if names[pid] == nil, Date().timeIntervalSince(refreshedAt) > 5 {
                names = AdbLogs.processNames(serial: serial)
                refreshedAt = Date()
            }
            let level = g(3) == "F" ? "E" : g(3)
            return LogLine(id: 0, timeMs: (Double(g(1)) ?? 0) * 1000, level: level, process: names[pid] ?? g(4),
                           message: "\(g(4)): \(g(5))", device: serial)
        }
    }
}

enum AdbLogs {
    static let path: String? = {
        let home = NSHomeDirectory()
        var c: [String] = []
        for k in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] { if let v = ProcessInfo.processInfo.environment[k] { c.append("\(v)/platform-tools/adb") } }
        c += ["\(home)/Library/Android/sdk/platform-tools/adb", "\(home)/Android/Sdk/platform-tools/adb", "/opt/homebrew/bin/adb", "/usr/local/bin/adb"]
        return c.first { FileManager.default.isExecutableFile(atPath: $0) }
    }()

    static func run(_ args: [String]) -> String {
        guard let path else { return "" }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = Pipe()
        guard (try? p.run()) != nil else { return "" }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return String(data: data, encoding: .utf8) ?? ""
    }

    static func devices() -> [String] {
        run(["devices"]).split(separator: "\n").dropFirst().compactMap { l in
            let f = l.split(whereSeparator: \.isWhitespace)
            return f.count == 2 && f[1] == "device" ? String(f[0]) : nil
        }
    }

    /// pid → process name (the package, for apps).
    static func processNames(serial: String) -> [String: String] {
        var map: [String: String] = [:]
        for l in run(["-s", serial, "shell", "ps", "-A", "-o", "PID,NAME"]).split(separator: "\n").dropFirst() {
            let f = l.split(whereSeparator: \.isWhitespace)
            if f.count >= 2 { map[String(f[0])] = String(f[1]) }
        }
        return map
    }
}

// MARK: - The inspector's Logs tab

struct LogsTab: View {
    let tx: FfiTransaction
    private let logs = LogStore.shared
    @AppStorage("logsAppOnly") private var appOnly = true

    private enum Item: Identifiable {
        case line(LogLine)
        case marker(id: String, timeMs: Double, text: String)
        var id: String {
            switch self {
            case .line(let l): "l\(l.id)"
            case .marker(let id, _, _): id
            }
        }
        var timeMs: Double {
            switch self {
            case .line(let l): l.timeMs
            case .marker(_, let t, _): t
            }
        }
    }

    var body: some View {
        let app = Analytics.appName(tx)
        let all = logs.around(tx)
        let mine = all.filter { LogStore.isFromApp($0, app: app) }
        let usingApp = appOnly && !mine.isEmpty
        let shown = Array((usingApp ? mine : all).prefix(400))
        let start = Double(tx.startedAtMs), end = Double(tx.startedAtMs + tx.durationMs)
        let items = (shown.map(Item.line) + [
            .marker(id: "start", timeMs: start, text: "▶ \(tx.method) \(tx.host) sent"),
            .marker(id: "end", timeMs: max(end, start), text: "◀ \(tx.status.map { "\($0)" } ?? "no response") after \(formatMs(tx.durationMs))"),
        ]).sorted { $0.timeMs < $1.timeMs }

        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Logs within 2 s of this call · \(logs.sources.joined(separator: ", "))")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
                Spacer()
                Toggle("Only \(app)", isOn: $appOnly).toggleStyle(.switch).controlSize(.small).font(.system(size: 12))
            }
            if appOnly && mine.isEmpty && !all.isEmpty {
                Text("No lines from \(app) in this window, so all processes are shown.").font(.system(size: 11.5)).foregroundStyle(Theme.faint)
            }
            if shown.isEmpty {
                Text("No log lines around this call. Logs are captured from booted simulators and Android devices while Relay is open.")
                    .font(.system(size: 12)).foregroundStyle(Theme.faint)
            }
            VStack(alignment: .leading, spacing: 0) {
                ForEach(items) { item in
                    switch item {
                    case .marker(_, let t, let text):
                        HStack {
                            Text(text).font(.system(size: 11.5, weight: .semibold)).foregroundStyle(Theme.accent)
                            Spacer()
                            Text(offset(t - start)).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.accent)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .background(Theme.accent.opacity(0.10))
                    case .line(let l):
                        HStack(alignment: .top, spacing: 8) {
                            Text(offset(l.timeMs - start)).foregroundStyle(Theme.faint).frame(width: 62, alignment: .trailing)
                            Text(l.level).fontWeight(.bold).foregroundStyle(color(l.level)).frame(width: 10)
                            if !usingApp { Text(l.process).foregroundStyle(Theme.key).lineLimit(1).frame(width: 90, alignment: .leading) }
                            Text(l.message).foregroundStyle(Theme.fg).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .font(.system(size: 11, design: .monospaced))
                        .padding(.horizontal, 10).padding(.vertical, 3)
                    }
                }
            }
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.raised))
        }
    }

    private func offset(_ ms: Double) -> String { String(format: "%+.2f s", ms / 1000) }
    private func color(_ level: String) -> Color { level == "E" ? Theme.s5xx : (level == "W" ? Theme.s4xx : Theme.muted) }
}
