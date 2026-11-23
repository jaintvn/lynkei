import RelayKit
import SwiftUI

/// "Android over Wi-Fi": a QR code that opens Relay's Android setup page on the phone, a live check
/// that the phone's traffic arrives, and adb wireless pairing for people who want the full Connect flow without a cable.
struct AndroidWifiCard: View {
    let url: String
    @Environment(TrafficStore.self) private var traffic
    @State private var showPair = false
    @State private var pairAddress = ""
    @State private var pairCode = ""
    @State private var connectAddress = ""
    @State private var pairMessage: String?
    @State private var working = false

    /// Calls that came from another device on the network and look like an Android app (not an iPhone, not this Mac).
    private var phoneCalls: [FfiTransaction] {
        traffic.transactions.suffix(3000).filter { t in
            let ip = t.clientAddr.split(separator: ":").first.map(String.init) ?? ""
            guard !ip.isEmpty, !ip.hasPrefix("127."), ip != "[", !t.clientAddr.hasPrefix("[::1]") else { return false }
            let ua = (t.requestHeaders.first { $0.name.lowercased() == "user-agent" }?.value ?? "").lowercased()
            if ua.contains("iphone") || ua.contains("cfnetwork") || ua.contains("darwin") || ua.contains("macintosh") { return false }
            return ua.contains("android") || ua.contains("okhttp") || ua.contains("dalvik") || ua.isEmpty
        }
    }

    var body: some View {
        let calls = phoneCalls
        let seen = !calls.isEmpty
        let decrypted = calls.contains { $0.decrypted }
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "wifi")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text("ANDROID OVER WI-FI").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text("Your Android phone").font(.system(size: 15, weight: .semibold))
                    Text("Same network as this Mac, no cable").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            HStack(alignment: .center, spacing: 14) {
                if let qr = qrImage(for: url), !url.isEmpty {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 112, height: 112)
                        .padding(6)
                        .background(RoundedRectangle(cornerRadius: 8).fill(.white))
                        .accessibilityLabel("QR code that opens the Relay Android setup page")
                } else {
                    Text("This Mac has no network address. Connect to Wi-Fi first.").font(.system(size: 12)).foregroundStyle(Theme.muted)
                        .frame(width: 112 + 12, height: 112 + 12)
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Scan with the phone's camera").font(.system(size: 12.5, weight: .semibold))
                    Text("Opens a page with the certificate and the exact Wi-Fi proxy settings to enter.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                    Text(url).font(.system(size: 10.5, design: .monospaced)).foregroundStyle(Theme.faint).textSelection(.enabled)
                }
            }
            .padding(12)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            VStack(alignment: .leading, spacing: 8) {
                row("Phone's traffic reaches Relay", on: seen, detail: seen ? calls.last.map { $0.clientAddr.split(separator: ":").first.map(String.init) ?? "" } : nil)
                row("Certificate trusted (a call was read)", on: decrypted, detail: nil)
            }
            if seen && !decrypted {
                Text("Traffic arrives but is not readable yet. Install the certificate on the phone; apps that ignore user certificates stay hidden.")
                    .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
            }
            DisclosureGroup(isExpanded: $showPair) { pairing } label: {
                Text("No cable? Pair with adb instead").font(.system(size: 12, weight: .medium))
            }
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
    }

    private func row(_ title: String, on: Bool, detail: String?) -> some View {
        HStack(spacing: 8) {
            Image(systemName: on ? "checkmark.circle.fill" : "circle").foregroundStyle(on ? Theme.s2xx : Theme.faint)
            Text(title).font(.system(size: 12.5))
            if let detail, !detail.isEmpty { Text(detail).font(.system(size: 11.5, design: .monospaced)).foregroundStyle(Theme.faint) }
        }
    }

    private var pairing: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("On the phone: Settings → Developer options → Wireless debugging → Pair device with pairing code. Type what it shows:")
                .font(.system(size: 11.5)).foregroundStyle(Theme.muted)
            HStack {
                TextField("Pairing address  192.168.1.9:37123", text: $pairAddress).textFieldStyle(.roundedBorder)
                TextField("Code", text: $pairCode).textFieldStyle(.roundedBorder).frame(width: 90)
            }
            TextField("Connect address from the Wireless debugging screen (optional)  192.168.1.9:41234", text: $connectAddress).textFieldStyle(.roundedBorder)
            HStack {
                Button(working ? "Working…" : "Pair and connect") { pair() }
                    .buttonStyle(AccentButtonStyle(height: 28)).font(.system(size: 12, weight: .medium))
                    .disabled(working || pairAddress.isEmpty || pairCode.isEmpty || AdbLogs.path == nil)
                if AdbLogs.path == nil {
                    Text("adb was not found. Install it with: brew install android-platform-tools").font(.system(size: 11.5)).foregroundStyle(Theme.s4xx)
                }
            }
            if let m = pairMessage { Text(m).font(.system(size: 11.5)).foregroundStyle(Theme.muted).textSelection(.enabled) }
        }
        .padding(.top, 6)
    }

    private func pair() {
        working = true
        pairMessage = nil
        let (addr, code, connect) = (pairAddress.trimmingCharacters(in: .whitespaces), pairCode.trimmingCharacters(in: .whitespaces), connectAddress.trimmingCharacters(in: .whitespaces))
        Task.detached {
            var out = AdbLogs.run(["pair", addr, code]).trimmingCharacters(in: .whitespacesAndNewlines)
            if !connect.isEmpty { out += "\n" + AdbLogs.run(["connect", connect]).trimmingCharacters(in: .whitespacesAndNewlines) }
            let text = out.isEmpty ? "adb gave no answer. Check the address and code." : out
            await MainActor.run {
                pairMessage = text + (connect.isEmpty && text.contains("Successfully") ? "\nThe phone should appear in this list in a few seconds." : "")
                working = false
            }
        }
    }
}
