import SwiftUI

/// iPhones and iPads plugged in by cable. Relay can't set an iPhone's proxy over USB (iOS has nothing like `adb reverse`),
/// so this card says so and points to the Wi-Fi setup next to it, instead of leaving the phone silently missing.
enum UsbIPhones {
    static func detect() -> [String] {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/sbin/system_profiler")
        p.arguments = ["SPUSBDataType", "-json"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = Pipe()
        guard (try? p.run()) != nil else { return [] }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard let json = try? JSONSerialization.jsonObject(with: data) else { return [] }
        var found: [String] = []
        func walk(_ x: Any) {
            if let d = x as? [String: Any] {
                if let n = d["_name"] as? String, n.contains("iPhone") || n.contains("iPad") { found.append(n) }
                for v in d.values { walk(v) }
            } else if let a = x as? [Any] {
                a.forEach(walk)
            }
        }
        walk(json)
        return Array(Set(found)).sorted()
    }
}

struct UsbIPhoneCard: View {
    let names: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "cable.connector")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text("IPHONE OVER USB").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text(names.joined(separator: ", ")).font(.system(size: 15, weight: .semibold)).lineLimit(2)
                    Text("Plugged in by cable").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("An iPhone can't be set up over the cable").font(.system(size: 12.5, weight: .semibold))
                Text("iOS has no way for Relay to set the phone's proxy through USB, as it does for Android. The cable is fine, but the phone has to use Wi-Fi: scan the QR code on the “Your iPhone or iPad” card with its camera and follow the page (install the profile, trust it, set the Wi-Fi proxy).")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                Text("The Mac and the iPhone must be on the same Wi-Fi network.").font(.system(size: 12)).foregroundStyle(Theme.faint)
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
