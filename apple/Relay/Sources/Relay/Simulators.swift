import Foundation

/// iOS Simulator helpers built on `xcrun simctl`.
struct BootedSimulator: Identifiable, Hashable {
    let id: String  // UDID
    let name: String
    let runtime: String
}

enum Simulators {
    static func booted() -> [BootedSimulator] {
        guard let data = try? run(["simctl", "list", "devices", "booted", "-j"]),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let devices = json["devices"] as? [String: [[String: Any]]]
        else { return [] }
        return devices.flatMap { runtimeKey, list in
            list.compactMap { d -> BootedSimulator? in
                guard let udid = d["udid"] as? String, let name = d["name"] as? String else { return nil }
                let runtime = runtimeKey.components(separatedBy: ".").last?
                    .replacingOccurrences(of: "-", with: " ")
                    .replacingOccurrences(of: "iOS ", with: "iOS ") ?? runtimeKey
                return BootedSimulator(id: udid, name: name, runtime: runtime)
            }
        }
    }

    /// Adds the Relay CA to the simulator's trusted roots. Idempotent.
    static func installCA(udid: String, certPath: String) throws {
        _ = try run(["simctl", "keychain", udid, "add-root-cert", certPath])
    }

    @discardableResult
    private static func run(_ args: [String]) throws -> Data {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
        p.arguments = args
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        try p.run()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            let msg = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            throw NSError(domain: "simctl", code: Int(p.terminationStatus), userInfo: [NSLocalizedDescriptionKey: msg])
        }
        return data
    }
}
