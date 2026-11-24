import Foundation
import Observation
import LynkeiKit

/// Bridges lynkei-core events onto the main actor for SwiftUI.
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
    /// Live switches (also on the notch island): HTTPS decryption, and a network preset for everything.
    private(set) var decrypt = true
    private(set) var globalThrottle: String?
    private(set) var throttlePresets: [String] = ["3g", "edge", "lte", "lte-lossy", "high-latency", "offline"]
    var search: String = "" { didSet { revision &+= 1 } }
    /// "Only this device/app" choice, as a search term (`app:AcmeShop`, `client:192.168.0.5`).
    var source: String? { didSet { revision &+= 1 } }
    /// "Only this app / this device" (names as shown in the app, see `Analytics.appName`).
    var appFilter: String? { didSet { revision &+= 1 } }
    var deviceFilter: String? { didSet { revision &+= 1 } }
    /// Favourite apps and endpoints (kept between launches). With any set and Focus on, only they are shown.
    var favApps: Set<String> = Set(UserDefaults.standard.stringArray(forKey: "favApps") ?? []) {
        didSet { UserDefaults.standard.set(Array(favApps), forKey: "favApps"); revision &+= 1 }
    }
    var favEndpoints: Set<String> = Set(UserDefaults.standard.stringArray(forKey: "favEndpoints") ?? []) {
        didSet { UserDefaults.standard.set(Array(favEndpoints), forKey: "favEndpoints"); revision &+= 1 }
    }
    var favHosts: Set<String> = Set(UserDefaults.standard.stringArray(forKey: "favHosts") ?? []) {
        didSet { UserDefaults.standard.set(Array(favHosts), forKey: "favHosts"); revision &+= 1 }
    }
    /// Show only one endpoint (an `Analytics.endpointKey`), chosen from the sidebar or the Endpoints screen.
    var endpointFilter: String? { didSet { revision &+= 1 } }
    var focusMode = true { didSet { revision &+= 1 } }
    var hasFavourites: Bool { !favApps.isEmpty || !favEndpoints.isEmpty || !favHosts.isEmpty }
    func isFavourite(_ t: FfiTransaction) -> Bool {
        favApps.contains(Analytics.appName(t)) || favEndpoints.contains(Analytics.endpointKey(t)) || favHosts.contains(t.host)
    }
    func toggleFavApp(_ name: String) { if favApps.contains(name) { favApps.remove(name) } else { favApps.insert(name) } }
    func toggleFavHost(_ host: String) { if favHosts.contains(host) { favHosts.remove(host) } else { favHosts.insert(host) } }

    /// Lists one endpoint and shows its newest call in the inspector.
    func showEndpoint(_ key: String) {
        endpointFilter = key
        search = ""
        selection = transactions.last { Analytics.endpointKey($0) == key }?.id
        if focusMode && hasFavourites { focusMode = false }
    }

    /// Lists one domain and shows its newest call in the inspector.
    func showHost(_ host: String) {
        endpointFilter = nil
        search = "host:\(host)"
        selection = transactions.last { $0.host == host }?.id
        if focusMode && hasFavourites { focusMode = false }
    }

    func toggleFavEndpoint(_ key: String) { if favEndpoints.contains(key) { favEndpoints.remove(key) } else { favEndpoints.insert(key) } }
    /// Only favourites, if there are any (the island always works this way).
    func favouritesOnly(_ rows: [FfiTransaction]) -> [FfiTransaction] { hasFavourites ? rows.filter(isFavourite) : rows }
    /// The newest calls the island looks at: favourites only when some are set.
    var islandRows: [FfiTransaction] { favouritesOnly(Array(transactions.suffix(3000))) }
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var cache: (revision: Int, rows: [FfiTransaction])?
    @ObservationIgnored private var parsed: (query: String, filter: TrafficFilter?, error: String?)?

    private var engine: LynkeiEngine?

    /// For the Connect-a-device screen (device actions run on the same engine).
    var engineForDevices: LynkeiEngine? { engine }

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

    /// Tries again the hosts Lynkei gave up on because an app rejected its certificate.
    func retryDecrypting() {
        engine?.retryDecrypting()
        ruleMessage = "Lynkei will try decrypting these hosts again. Run the call in the app."
    }

    func setDecrypt(_ on: Bool) {
        engine?.setDecrypt(on: on)
        decrypt = on
    }
    func setGlobalThrottle(_ preset: String?) {
        do {
            try engine?.setGlobalThrottle(preset: preset)
            globalThrottle = preset
        } catch { ruleMessage = "\(error)" }
    }
    private func refreshSwitches() {
        guard let e = engine else { return }
        decrypt = e.decryptEnabled()
        globalThrottle = e.globalThrottle()
        throttlePresets = e.throttlePresets()
    }

    func refreshRules() { if let e = engine { rules = e.listRules() } }
    func rulesFor(_ tx: FfiTransaction) -> [FfiRule] { engine?.rulesFor(request: tx) ?? [] }
    func ruleKind(_ id: String) -> String { rules.first { $0.id == id }?.kind ?? "" }
    func addMapLocal(_ tx: FfiTransaction, file: String, status: UInt16) throws {
        guard let engine else { return }
        _ = try engine.addMapLocalRule(request: tx, file: file, status: status)
        refreshRules()
    }
    /// Creates or updates a rewrite rule (empty id = new). Returns its id.
    func saveRewrite(_ rule: FfiRewriteRule) throws -> String {
        guard let engine else { throw NSError(domain: "Lynkei", code: 1, userInfo: [NSLocalizedDescriptionKey: "Start Lynkei first"]) }
        let id = try engine.saveRewriteRule(rule: rule)
        refreshRules()
        return id
    }
    func rewriteRule(_ id: String) -> FfiRewriteRule? { try? engine?.getRewriteRule(id: id) }

    func addDnsSpoof(host: String, to: String) throws {
        guard let engine else { return }
        _ = try engine.addDnsSpoofRule(host: host, to: to)
        refreshRules()
    }
    func setMapLocalFile(_ id: String, file: String, status: UInt16) throws {
        try engine?.setMapLocalFile(id: id, file: file, status: status)
        refreshRules()
    }
    func rulesYAML() -> String { engine?.rulesYaml() ?? "" }
    func applyRulesYAML(_ yaml: String) -> [String] {
        let problems = engine?.setRules(yaml: yaml) ?? ["Lynkei is not running"]
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
        let scoped = scope(rows)
        cache = (revision, scoped)
        return scoped
    }

    /// Keeps the calls from the chosen app and device (no filter set: all of them).
    func scope(_ rows: [FfiTransaction]) -> [FfiTransaction] {
        guard appFilter != nil || deviceFilter != nil || endpointFilter != nil || (focusMode && hasFavourites) else { return rows }
        return rows.filter { t in
            (!(focusMode && hasFavourites) || isFavourite(t))
                && (endpointFilter == nil || Analytics.endpointKey(t) == endpointFilter)
                && (appFilter == nil || Analytics.appName(t) == appFilter) && (deviceFilter == nil || Analytics.deviceLabel(t) == deviceFilter)
        }
    }

    /// Apps that made requests, with counts.
    var processesSeen: [(name: String, count: Int)] {
        var counts: [String: Int] = [:]
        for t in transactions { counts[Analytics.appName(t), default: 0] += 1 }
        return counts.map { ($0.key, $0.value) }.sorted { $0.count > $1.count }
    }

    var selected: FfiTransaction? {
        guard let selection else { return nil }
        return transactions.first { $0.id == selection }
    }

    static var dataDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Lynkei", isDirectory: true)
    }

    func start(port requested: UInt16 = 8888) {
        do {
            let engine = try self.engine ?? LynkeiEngine(dataDir: Self.dataDirectory.path)
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
            refreshSwitches()
            watchBreakpoints()
            LogStore.shared.start()
            errorMessage = nil
        } catch {
            errorMessage = "\(error)"
            status = "Failed to start"
        }
    }

    /// Called when the app quits: undo every device connection and the system proxy.
    func shutdown() {
        LogStore.shared.stop()
        engine?.disconnectAllDevices()
        engine?.stop()
    }

    func stop() {
        engine?.stop()
        port = nil
        status = "Stopped"
    }

    /// HAR for the requests matching the current search and source filter (all stored history).
    func exportHAR() throws -> String {
        guard let engine else { throw NSError(domain: "Lynkei", code: 1, userInfo: [NSLocalizedDescriptionKey: "Start Lynkei first"]) }
        return try engine.exportHar(query: query)
    }

    /// Shows the exchanges of a HAR file in the list (and keeps them in history). Returns how many.
    func importHAR(_ text: String) throws -> Int {
        guard let engine else { throw NSError(domain: "Lynkei", code: 1, userInfo: [NSLocalizedDescriptionKey: "Start Lynkei first"]) }
        let added = try engine.importHar(har: text)
        for tx in added { append(tx) }
        return added.count
    }

    /// Sends a request through Lynkei (so it shows in the list), `count` times.
    func compose(_ request: FfiComposeRequest, count: Int, parallel: Bool) async throws -> [FfiComposeResult] {
        guard let engine else { throw NSError(domain: "Lynkei", code: 1, userInfo: [NSLocalizedDescriptionKey: "Start Lynkei first"]) }
        return try await Task.detached { try engine.compose(request: request, count: UInt32(count), parallel: parallel) }.value
    }

    /// Resends a captured request exactly as it was, once.
    func repeatNow(_ tx: FfiTransaction) {
        if GRPC.isGRPC(tx) { ruleMessage = "gRPC calls can't be repeated yet: Lynkei shows the messages decoded, not as the original bytes"; return }
        let draft = ComposeDraft(copying: tx)
        Task {
            do {
                let r = try await compose(draft.request, count: 1, parallel: false)
                if let e = r.first?.error { ruleMessage = "Repeat failed: \(e)" }
            } catch { ruleMessage = "Repeat failed: \(error)" }
        }
    }

    func clear() {
        revision &+= 1
        transactions.removeAll()
        indexById.removeAll()
        selection = nil
    }

    /// Position of each transaction id in `transactions`, so a later event (same id) replaces the row.
    private var indexById: [UInt64: Int] = [:]

    func appendImported(_ tx: FfiTransaction) { append(tx) }

    private func append(_ tx: FfiTransaction) {
        revision &+= 1
        if let i = indexById[tx.id], i < transactions.count, transactions[i].id == tx.id {
            transactions[i] = tx
            return
        }
        indexById[tx.id] = transactions.count
        transactions.append(tx)
        // Test hook: LYNKEI_SELECT=<text in a URL> selects the first matching call.
        if selection == nil, let want = ProcessInfo.processInfo.environment["LYNKEI_SELECT"], tx.url.contains(want) { selection = tx.id }
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
