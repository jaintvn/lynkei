import Foundation
import Observation
import RelayKit

/// Bridges relay-core events onto the main actor for SwiftUI.
@MainActor
@Observable
final class TrafficStore {
    private(set) var transactions: [FfiTransaction] = []
    private(set) var port: UInt16?
    private(set) var caPath: String = ""
    var status: String = "Stopped"
    var errorMessage: String?
    var selection: FfiTransaction.ID?
    /// Requests/responses held at a breakpoint, waiting for a decision.
    private(set) var paused: [FfiPaused] = []
    private(set) var rules: [FfiRule] = []
    var search: String = "" { didSet { revision &+= 1 } }
    /// "Only this device/app" choice, as a search term (`app:AcmeShop`, `client:192.168.0.5`).
    var source: String? { didSet { revision &+= 1 } }
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var cache: (revision: Int, rows: [FfiTransaction])?
    @ObservationIgnored private var parsed: (query: String, filter: TrafficFilter?, error: String?)?

    private var engine: RelayEngine?

    /// For the Connect-a-device screen (device actions run on the same engine).
    var engineForDevices: RelayEngine? { engine }

    /// One store for the whole app, so closing and reopening the screen keeps its state.
    @ObservationIgnored private var deviceStoreCache: DeviceStore?
    func deviceStore() -> DeviceStore {
        if let deviceStoreCache { return deviceStoreCache }
        let s = DeviceStore(engine: { [weak self] in self?.engine })
        deviceStoreCache = s
        return s
    }

    var isRunning: Bool { port != nil }

    // MARK: Rules and breakpoints

    func refreshRules() { if let e = engine { rules = e.listRules() } }
    func rulesFor(_ tx: FfiTransaction) -> [FfiRule] { engine?.rulesFor(request: tx) ?? [] }
    func ruleKind(_ id: String) -> String { rules.first { $0.id == id }?.kind ?? "" }
    func rulesYAML() -> String { engine?.rulesYaml() ?? "" }
    func applyRulesYAML(_ yaml: String) -> [String] {
        let problems = engine?.setRules(yaml: yaml) ?? ["Relay is not running"]
        refreshRules()
        return problems
    }
    func setRule(_ id: String, enabled: Bool) {
        try? engine?.setRuleEnabled(id: id, enabled: enabled)
        refreshRules()
    }
    func removeRule(_ id: String) {
        try? engine?.removeRule(id: id)
        refreshRules()
    }
    /// Adds a rule aimed at one request (`block`, `breakpoint`, `throttle-3g`, `offline`, `map-local`).
    func addQuickRule(_ kind: String, for tx: FfiTransaction) {
        do {
            _ = try engine?.addQuickRule(kind: kind, request: tx)
            ruleMessage = nil
        } catch {
            ruleMessage = "\(error)"
        }
        refreshRules()
    }
    var ruleMessage: String?

    func resumePaused(_ p: FfiPaused, edit: FfiEdit?) {
        _ = engine?.resumePaused(txId: p.txId, response: p.response, edit: edit)
        paused.removeAll { $0.id == p.id }
    }
    func abortPaused(_ p: FfiPaused) {
        _ = engine?.abortPaused(txId: p.txId, response: p.response)
        paused.removeAll { $0.id == p.id }
    }

    /// Polls for held messages while the engine runs (cheap: a lock and a copy).
    private func watchBreakpoints() {
        guard pauseWatcher == nil else { return }
        pauseWatcher = Task { [weak self] in
            while !Task.isCancelled {
                if let engine = self?.engine {
                    let held = await Task.detached { engine.paused() }.value
                    if let self, held.map(\.id) != self.paused.map(\.id) { self.paused = held }
                }
                try? await Task.sleep(for: .milliseconds(300))
            }
        }
    }
    @ObservationIgnored private var pauseWatcher: Task<Void, Never>?

    private var query: String {
        [source, search.trimmingCharacters(in: .whitespaces)].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " ")
    }

    /// Why the search text is not a valid query, if it is not.
    var searchError: String? { _ = filter; return parsed?.error }

    private var filter: TrafficFilter? {
        let q = query
        if let parsed, parsed.query == q { return parsed.filter }
        do {
            let f = try TrafficFilter.parse(query: q)
            parsed = (q, f.isEmpty() ? nil : f, nil)
        } catch {
            parsed = (q, nil, "\(error)")
        }
        return parsed?.filter
    }

    var filtered: [FfiTransaction] {
        if let cache, cache.revision == revision { return cache.rows }
        let rows: [FfiTransaction]
        if let f = filter {
            rows = transactions.filter { f.matches(transaction: $0) }
        } else if parsed?.error != nil {
            rows = []
        } else {
            rows = transactions
        }
        cache = (revision, rows)
        return rows
    }

    /// Programs on this Mac that made requests, with counts, for the Source menu.
    var processesSeen: [(name: String, count: Int)] {
        var counts: [String: Int] = [:]
        for t in transactions { if let p = t.clientProcess { counts[p, default: 0] += 1 } }
        return counts.map { ($0.key, $0.value) }.sorted { $0.count > $1.count }
    }

    var selected: FfiTransaction? {
        guard let selection else { return nil }
        return transactions.first { $0.id == selection }
    }

    static var dataDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Relay", isDirectory: true)
    }

    func start(port requested: UInt16 = 8888) {
        do {
            let engine = try self.engine ?? RelayEngine(dataDir: Self.dataDirectory.path)
            self.engine = engine
            let listener = Listener { [weak self] tx in
                Task { @MainActor in self?.append(tx) }
            }
            // Listen on all interfaces so iPhones on the same Wi-Fi can connect later.
            let bound = try engine.start(listenIp: "0.0.0.0", port: requested, listener: listener)
            port = bound
            caPath = try engine.caCertificatePath()
            status = "Listening on port \(bound)"
            refreshRules()
            watchBreakpoints()
            errorMessage = nil
        } catch {
            errorMessage = "\(error)"
            status = "Failed to start"
        }
    }

    /// Called when the app quits: undo every device connection and the system proxy.
    func shutdown() {
        engine?.disconnectAllDevices()
        engine?.stop()
    }

    func stop() {
        engine?.stop()
        port = nil
        status = "Stopped"
    }

    func clear() {
        revision &+= 1
        transactions.removeAll()
        indexById.removeAll()
        selection = nil
    }

    /// Position of each transaction id in `transactions`, so a later event (same id) replaces the row.
    private var indexById: [UInt64: Int] = [:]

    private func append(_ tx: FfiTransaction) {
        revision &+= 1
        if let i = indexById[tx.id], i < transactions.count, transactions[i].id == tx.id {
            transactions[i] = tx
            return
        }
        indexById[tx.id] = transactions.count
        transactions.append(tx)
        if transactions.count > 50_000 {
            transactions.removeFirst(transactions.count - 50_000)
            indexById = Dictionary(uniqueKeysWithValues: transactions.enumerated().map { ($1.id, $0) })
        }
    }
}

extension FfiTransaction: @retroactive Identifiable {}

/// Receives events on Rust engine threads; forwards them without blocking.
private final class Listener: TrafficListener, @unchecked Sendable {
    private let handler: @Sendable (FfiTransaction) -> Void
    init(_ handler: @escaping @Sendable (FfiTransaction) -> Void) { self.handler = handler }
    func onTransaction(transaction: FfiTransaction) { handler(transaction) }
}
