import Foundation
import Observation
import LynkeiKit

/// Detects devices and runs "Connect" for the Connect-a-device screen.
@MainActor
@Observable
final class DeviceStore {
    private(set) var devices: [FfiDevice] = []
    private(set) var results: [String: FfiConnectResult] = [:]
    private(set) var busy: Set<String> = []
    private(set) var errors: [String: String] = [:]
    private(set) var wifiURL: String = ""
    /// What the Android-over-Wi-Fi QR code encodes (empty without a network address).
    private(set) var androidWifiURL: String = ""
    /// The iPhone setup page by IP address (more reliable than the .local name on office networks); falls back to the .local URL.
    var iphoneSetupURL: String { androidWifiURL.isEmpty ? wifiURL : androidWifiURL.replacingOccurrences(of: "/android", with: "/setup") }
    private(set) var scanning = false
    /// Devices the user disconnected on purpose (so the island does not warn about them).
    private(set) var userDisconnected: Set<String> = []

    private let engine: () -> LynkeiEngine?
    private var timer: Task<Void, Never>?

    init(engine: @escaping () -> LynkeiEngine?) {
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
        let (list, url, androidURL) = await Task.detached(priority: .utility) { (engine.listDevices(), engine.wifiSetupUrl(), engine.wifiAndroidUrl()) }.value
        devices = list
        Analytics.devicePlatforms = Dictionary(list.map { ($0.name, $0.kind.hasPrefix("ios") ? Analytics.Platform.ios : .android) }, uniquingKeysWith: { a, _ in a })
        Analytics.androidDevice = list.first { $0.kind.hasPrefix("android") && ($0.connected || results[$0.id] != nil) }?.name
        wifiURL = url
        androidWifiURL = androidURL
        // A screen opened later (or after a restart of the sheet) still shows the checklist.
        for d in list where d.connected && results[d.id] == nil {
            let id = d.id
            let known = await Task.detached { engine.deviceStatus(id: id) }.value
            if let known { results[id] = known }
        }
        scanning = false
    }

    func connect(_ device: FfiDevice) async {
        guard let engine = engine(), !busy.contains(device.id) else { return }
        busy.insert(device.id)
        errors[device.id] = nil
        let id = device.id
        userDisconnected.remove(id)
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
        userDisconnected.insert(id)
        _ = await Task.detached { Result { try engine.disconnectDevice(id: id) } }.value
        results[id] = nil
        busy.remove(id)
        await refresh()
    }

    func isConnected(_ d: FfiDevice) -> Bool { d.connected || results[d.id] != nil }
}
