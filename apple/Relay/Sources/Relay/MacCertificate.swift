import AppKit
import Observation
import SwiftUI

/// Trusting the Relay certificate on this Mac, like Charles' "Install Charles Root Certificate":
/// one button that adds it to your login keychain as a trusted root.
@MainActor
@Observable
final class MacCertificate {
    static let shared = MacCertificate()

    /// nil until checked.
    private(set) var trusted: Bool?
    private(set) var busy = false
    private(set) var message: String?
    /// Called after the certificate was installed (the app uses it to retry hosts Relay had given up on).
    var onInstalled: (() -> Void)?

    var path: String { TrafficStore.dataDirectory.appendingPathComponent("relay-ca.pem").path }
    var exists: Bool { FileManager.default.fileExists(atPath: path) }

    func refresh() {
        guard exists else { trusted = nil; return }
        let p = path
        Task.detached {
            let ok = Self.run(["verify-cert", "-c", p, "-p", "basic"]).status == 0
            await MainActor.run { MacCertificate.shared.trusted = ok }
        }
    }

    /// Adds the certificate to the login keychain and marks it trusted. macOS asks for your password.
    func install() {
        guard exists, !busy else {
            if !exists { message = "Relay has not created its certificate yet. Start the proxy first." }
            return
        }
        busy = true
        message = nil
        let p = path
        let keychain = NSHomeDirectory() + "/Library/Keychains/login.keychain-db"
        Task.detached {
            let r = Self.run(["add-trusted-cert", "-r", "trustRoot", "-k", keychain, p])
            await MainActor.run {
                let me = MacCertificate.shared
                me.busy = false
                if r.status == 0 {
                    me.message = "Relay's certificate is now trusted on this Mac."
                    me.onInstalled?()
                } else {
                    let why = r.output.trimmingCharacters(in: .whitespacesAndNewlines)
                    me.message = why.isEmpty ? "macOS did not accept the certificate (cancelled, or blocked by your organisation)." : why
                }
                me.refresh()
            }
        }
    }

    func revealFile() {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    private nonisolated static func run(_ args: [String]) -> (status: Int32, output: String) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/security")
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        guard (try? p.run()) != nil else { return (-1, "") }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return (p.terminationStatus, String(data: data, encoding: .utf8) ?? "")
    }
}

/// "This Mac" card on the Connect screen: certificate status and the install button.
struct MacCertificateCard: View {
    private let cert = MacCertificate.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "laptopcomputer")
                    .font(.system(size: 18))
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
                VStack(alignment: .leading, spacing: 2) {
                    Text("THIS MAC").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.faint)
                    Text("Relay certificate").font(.system(size: 15, weight: .semibold))
                    Text("Needed to read HTTPS from this Mac's apps and browsers").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            HStack(spacing: 8) {
                Image(systemName: cert.trusted == true ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
                    .foregroundStyle(cert.trusted == true ? Theme.s2xx : Theme.s4xx)
                Text(cert.trusted == true ? "Trusted on this Mac" : (cert.trusted == false ? "Not trusted yet" : "Checking…"))
                    .font(.system(size: 13, weight: .medium))
                Spacer()
                if cert.trusted != true {
                    Button(cert.busy ? "Installing…" : "Install") { cert.install() }
                        .buttonStyle(AccentButtonStyle(height: 28)).font(.system(size: 12, weight: .medium)).disabled(cert.busy)
                }
            }
            .padding(12)
            .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
            MacProxyRow()
            if let m = cert.message { Text(m).font(.system(size: 12)).foregroundStyle(Theme.muted) }
            HStack {
                Text("Adds it to your login keychain. macOS asks for your password.").font(.system(size: 11.5)).foregroundStyle(Theme.faint)
                Spacer()
                Button("Show file") { cert.revealFile() }.buttonStyle(.plain).font(.system(size: 11.5)).foregroundStyle(Theme.accent)
            }
        }
        .padding(18)
        .background(RoundedRectangle(cornerRadius: 14).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
        .onAppear { cert.refresh() }
    }
}

/// "Mac proxy" status inside the This Mac card.
struct MacProxyRow: View {
    @Environment(TrafficStore.self) private var traffic
    private let proxy = MacProxyStatus.shared

    var body: some View {
        let (ok, title, detail) = describe()
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill").foregroundStyle(ok ? Theme.s2xx : Theme.s4xx)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: 12.5, weight: .medium))
                Text(detail).font(.system(size: 11.5)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.line))
        .task {
            while !Task.isCancelled {
                proxy.refresh(relayPort: traffic.port)
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func describe() -> (Bool, String, String) {
        switch proxy.state {
        case .relay(let port):
            return (true, "Mac proxy is on (127.0.0.1:\(port))", "iOS Simulators and this Mac's apps send their traffic through Relay.")
        case .off:
            return (false, "Mac proxy is off", "iOS Simulators and Mac apps are not going through Relay. Connect a simulator to turn it on. If it turns itself off, a VPN or company profile is resetting it.")
        case .other(let who):
            return (false, "Mac proxy points elsewhere (\(who))", "Another tool or a company profile set the proxy, so traffic skips Relay. Connect a simulator to switch it to Relay.")
        case .pac(let url):
            return (false, "A proxy auto-config script is on", "\(url.isEmpty ? "A script" : url) can send traffic around Relay. This usually comes from a company profile or VPN; ask IT, or turn it off in System Settings → Network → Details → Proxies.")
        case .unknown:
            return (false, "Mac proxy: checking…", "")
        }
    }
}
