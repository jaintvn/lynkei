import SwiftUI

/// iPhones and iPads plugged in by cable. Lynkei can't set an iPhone's proxy over USB (iOS has nothing like `adb reverse`),
/// so this card says so and points to the Wi-Fi setup next to it, instead of leaving the phone silently missing.
struct PhysicalPhone: Hashable {
    let name: String
    let model: String
    let os: String
    /// "wired" or "localNetwork" (paired over Wi-Fi).
    let transport: String
    var isWired: Bool { transport == "wired" }
}

enum UsbIPhones {
    /// iPhones and iPads that Xcode's `devicectl` can see: plugged in by cable, or paired for wireless use.
    static func detect() -> [PhysicalPhone] {
        let file = NSTemporaryDirectory() + "lynkei-devicectl-\(UUID().uuidString).json"
        defer { try? FileManager.default.removeItem(atPath: file) }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
        p.arguments = ["devicectl", "list", "devices", "--json-output", file, "-q"]
        p.standardOutput = Pipe()
        p.standardError = Pipe()
        guard (try? p.run()) != nil else { return [] }
        p.waitUntilExit()
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: file)),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let devices = (root["result"] as? [String: Any])?["devices"] as? [[String: Any]] else { return [] }
        return devices.compactMap { d in
            let hw = d["hardwareProperties"] as? [String: Any] ?? [:]
            let dev = d["deviceProperties"] as? [String: Any] ?? [:]
            let conn = d["connectionProperties"] as? [String: Any] ?? [:]
            guard hw["reality"] as? String == "physical", hw["platform"] as? String == "iOS",
                  (conn["tunnelState"] as? String) == "connected" || (conn["transportType"] as? String) == "wired" else { return nil }
            return PhysicalPhone(name: dev["name"] as? String ?? "iPhone", model: hw["marketingName"] as? String ?? "",
                                 os: dev["osVersionNumber"] as? String ?? "", transport: conn["transportType"] as? String ?? "")
        }
    }
}

struct UsbIPhoneCard: View {
    let phones: [PhysicalPhone]
    let setupURL: String

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "cable.connector")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text(phones.contains { $0.isWired } ? "IPHONE OVER USB" : "IPHONE (PAIRED)").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text(phones.map { $0.model.isEmpty ? $0.name : "\($0.name) · \($0.model)" }.joined(separator: ", ")).font(.system(size: 15, weight: .semibold)).lineLimit(2)
                    Text(phones.first.map { "iOS \($0.os) · \($0.isWired ? "plugged in by cable" : "paired over Wi-Fi")" } ?? "").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("An iPhone can't be set up over the cable").font(.system(size: 12.5, weight: .semibold))
                Text("iOS has no way for Lynkei to set the phone's proxy through USB, as it does for Android. The cable is fine, but the phone has to use Wi-Fi: scan the QR code on the “Your iPhone or iPad” card with its camera and follow the page (install the profile, trust it, set the Wi-Fi proxy).")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                Text("The Mac and the iPhone must be on the same Wi-Fi network.").font(.system(size: 12)).foregroundStyle(Theme.faint)
                if !setupURL.isEmpty {
                    HStack {
                        Text(setupURL).font(.system(size: 10.5, design: .monospaced)).foregroundStyle(Theme.faint).textSelection(.enabled).lineLimit(1)
                        Spacer()
                        Button("Copy link") { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(setupURL, forType: .string) }
                            .buttonStyle(.plain).font(.system(size: 11.5)).foregroundStyle(Theme.accent)
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
    }
}
