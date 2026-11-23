import Foundation
import Observation
import RelayKit

/// Detects devices and runs "Connect" for the Connect-a-device screen.
@MainActor
@Observable
final class DeviceStore {
    private(set) var devices: [FfiDevice] = []
    private(set) var results: [String: FfiConnectResult] = [:]
    private(set) var busy: Set<String> = []
    private(set) var errors: [String: String] = [:]
    private(set) var wifiURL: String = ""
    private(set) var scanning = false

    private let engine: () -> RelayEngine?
    private var timer: Task<Void, Never>?

    init(engine: @escaping () -> RelayEngine?) {
        self.engine = engine
    }

    /// Re-detects devices every few seconds while the screen is open.
    func startWatching() {
        guard timer == nil else { return }
        timer = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    func stopWatching() {
        timer?.cancel()
        timer = nil
    }

    func refresh() async {
        guard let engine = engine() else { return }
        scanning = true
        let (list, url) = await Task.detached(priority: .utility) { (engine.listDevices(), engine.wifiSetupUrl()) }.value
        devices = list
        wifiURL = url
        scanning = false
    }

    func connect(_ device: FfiDevice) async {
        guard let engine = engine(), !busy.contains(device.id) else { return }
        busy.insert(device.id)
        errors[device.id] = nil
        let id = device.id
        let outcome: Result<FfiConnectResult, Error> = await Task.detached {
            Result { try engine.connectDevice(id: id) }
        }.value
        switch outcome {
        case .success(let r): results[id] = r
        case .failure(let e): errors[id] = "\(e)"
        }
        busy.remove(id)
        await refresh()
    }

    func disconnect(_ device: FfiDevice) async {
        guard let engine = engine(), !busy.contains(device.id) else { return }
        busy.insert(device.id)
        let id = device.id
        _ = await Task.detached { Result { try engine.disconnectDevice(id: id) } }.value
        results[id] = nil
        busy.remove(id)
        await refresh()
    }

    func isConnected(_ d: FfiDevice) -> Bool { d.connected || results[d.id] != nil }
}
