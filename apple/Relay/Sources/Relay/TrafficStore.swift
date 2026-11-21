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
    var search: String = ""

    private var engine: RelayEngine?

    var isRunning: Bool { port != nil }

    var filtered: [FfiTransaction] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return transactions }
        return transactions.filter { $0.url.lowercased().contains(q) || $0.method.lowercased() == q }
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
            errorMessage = nil
        } catch {
            errorMessage = "\(error)"
            status = "Failed to start"
        }
    }

    func stop() {
        engine?.stop()
        port = nil
        status = "Stopped"
    }

    func clear() {
        transactions.removeAll()
        indexById.removeAll()
        selection = nil
    }

    /// Position of each transaction id in `transactions`, so a later event (same id) replaces the row.
    private var indexById: [UInt64: Int] = [:]

    private func append(_ tx: FfiTransaction) {
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
