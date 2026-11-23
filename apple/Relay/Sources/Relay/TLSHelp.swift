import RelayKit
import SwiftUI

/// Why an HTTPS call shows no request or response, and what to do about it.
enum TLSIssue {
    /// The app refused Relay's certificate (it pins its server certificate, or does not trust the Relay CA).
    case rejected
    /// Decryption is switched off in Relay.
    case off
    /// The host is on Relay's passthrough list.
    case listed
    /// The TLS handshake with the app failed for another reason.
    case handshake

    static func of(_ tx: FfiTransaction) -> TLSIssue? {
        guard let e = tx.error else { return nil }
        if e.hasPrefix("Not decrypted: the app rejected") { return .rejected }
        if e.hasPrefix("Not decrypted: this host is on") { return .listed }
        if e.hasPrefix("Not decrypted: SSL decryption") { return .off }
        if e.contains("TLS handshake with the client failed") {
            let rejected = ["CertificateUnknown", "UnknownCA", "BadCertificate", "CertificateExpired", "UnsupportedCertificate"]
            return rejected.contains { e.contains($0) } ? .rejected : .handshake
        }
        return nil
    }
}

struct TLSAdvice {
    var title: String
    var cause: String
    var steps: [String]
    /// "retry" (try decrypting again) or "enable" (turn SSL decryption on).
    var action: String?
    var actionTitle: String?

    /// `certInstalled`: other calls from the same device were decrypted, so the Relay CA is already trusted there.
    static func make(_ issue: TLSIssue, platform: Analytics.Platform, certInstalled: Bool, host: String?) -> TLSAdvice {
        let h = host.map { " (\($0))" } ?? ""
        switch issue {
        case .off:
            return TLSAdvice(title: "Relay can't read this HTTPS call", cause: "SSL decryption is switched off, so Relay only passes HTTPS through.",
                             steps: ["Turn on SSL in the toolbar (the SSL switch)."], action: "enable", actionTitle: "Turn on SSL decryption")
        case .listed:
            return TLSAdvice(title: "Relay can't read this HTTPS call", cause: "This host is on Relay's passthrough list\(h), so it is never decrypted.",
                             steps: ["Remove the host from the passthrough list if you want to see its calls."])
        case .handshake:
            return TLSAdvice(title: "The HTTPS connection could not be set up",
                             cause: "The app and Relay could not finish the TLS handshake. This is usually a network hiccup or an app that closed the connection.",
                             steps: ["Run the call again.", "If it keeps failing, check that the device trusts the Relay certificate."])
        case .rejected:
            switch platform {
            case .android:
                if certInstalled {
                    return TLSAdvice(
                        title: "This app refuses to be inspected\(h)",
                        cause: "Relay's certificate is installed on the phone (other apps and hosts are readable), but this app rejected it for this host. The app pins its server certificate, so it only accepts its own.",
                        steps: ["Use a debug or test build of the app with certificate pinning switched off.",
                                "Or test on a rooted phone or emulator with a tool that bypasses pinning (for example Frida).",
                                "Calls to this host stay hidden; everything else is still captured."],
                        action: "retry", actionTitle: "Try decrypting again")
                }
                return TLSAdvice(
                    title: "The phone doesn't trust Relay's certificate yet\(h)",
                    cause: "The app rejected Relay's certificate. Android apps ignore certificates you install yourself unless the app opts in, and the certificate may not be installed on the phone at all.",
                    steps: ["Install it: Settings → Security → Install a certificate → CA certificate → pick relay-ca.crt from Downloads (Samsung: Security and privacy → More security settings → Install from device storage).",
                            "Apps built for Android 7 or newer only accept it if it is a debug build with a network security config that trusts user certificates.",
                            "An emulator without Google Play, or a rooted device, lets Relay install it as a system certificate.",
                            "Then press Try decrypting again."],
                    action: "retry", actionTitle: "Try decrypting again")
            case .ios:
                return TLSAdvice(
                    title: "The app rejected Relay's certificate\(h)",
                    cause: "The device doesn't trust the Relay CA, or the app pins its server certificate.",
                    steps: ["On the iPhone: Settings → General → About → Certificate Trust Settings, and switch on Relay's certificate (after installing the profile from the setup page).",
                            "A simulator trusts it automatically after Connect.",
                            "If other apps work but this one doesn't, it pins certificates: use a build with pinning off.",
                            "Then press Try decrypting again."],
                    action: "retry", actionTitle: "Try decrypting again")
            case .mac:
                return TLSAdvice(
                    title: "The app rejected Relay's certificate\(h)",
                    cause: "This program doesn't trust the Relay CA, or it pins its server certificate.",
                    steps: ["Trust the Relay CA: in Terminal run  security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db \"$HOME/Library/Application Support/Relay/relay-ca.pem\"  (or open that file and set Always Trust in Keychain Access).",
                            "Apps that use their own certificate list (Firefox, Java, Node) need the CA added to that list.",
                            "Then press Try decrypting again."],
                    action: "retry", actionTitle: "Try decrypting again")
            }
        }
    }
}

extension TrafficStore {
    /// True if some call from this device was read in full, so its certificate setup works.
    func deviceDecrypts(_ device: String) -> Bool {
        transactions.suffix(3000).contains { $0.decrypted && Analytics.deviceLabel($0) == device }
    }

    func advice(for tx: FfiTransaction) -> TLSAdvice? {
        if tx.host == "10.0.2.2", tx.state == .failed, TLSIssue.of(tx) == nil {
            return TLSAdvice(
                title: "10.0.2.2 only exists inside the Android emulator",
                cause: "That address is the emulator's name for your Mac. A real phone can't reach it, so Relay's connection timed out.",
                steps: ["Point the app at a server the phone can reach, for example https://httpbin.org.",
                        "Or, for a server on this Mac, run `adb reverse tcp:8099 tcp:8099` and use http://127.0.0.1:8099 in the app."])
        }
        guard let issue = TLSIssue.of(tx) else { return nil }
        let device = Analytics.deviceLabel(tx)
        return TLSAdvice.make(issue, platform: Analytics.platform(ofDevice: device), certInstalled: deviceDecrypts(device), host: tx.host)
    }

    func perform(_ advice: TLSAdvice) {
        switch advice.action {
        case "retry": retryDecrypting()
        case "enable": setDecrypt(true)
        default: break
        }
    }
}

/// Steps and an action button, shown in the inspector and in the Traffic banner.
struct TLSAdviceView: View {
    let advice: TLSAdvice
    @Environment(TrafficStore.self) private var store

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "lock.trianglebadge.exclamationmark").foregroundStyle(Theme.s4xx).font(.system(size: 16))
                VStack(alignment: .leading, spacing: 4) {
                    Text(advice.title).font(.system(size: 12.5, weight: .semibold))
                    Text(advice.cause).font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 5) {
                Text("What to do").font(.system(size: 11)).foregroundStyle(Theme.faint)
                ForEach(Array(advice.steps.enumerated()), id: \.offset) { i, step in
                    HStack(alignment: .top, spacing: 6) {
                        Text("\(i + 1).").foregroundStyle(Theme.faint)
                        Text(step).foregroundStyle(Theme.fg).fixedSize(horizontal: false, vertical: true)
                    }
                    .font(.system(size: 12))
                }
            }
            if let title = advice.actionTitle {
                Button(title) { store.perform(advice) }.buttonStyle(AccentButtonStyle(height: 28)).font(.system(size: 12, weight: .medium))
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Theme.s4xx.opacity(0.10)))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.s4xx.opacity(0.30)))
    }
}

/// One line above the Traffic list when apps refused Relay's certificate recently.
struct TLSBanner: View {
    let rows: [FfiTransaction]
    @Environment(TrafficStore.self) private var store
    @State private var open = false

    var body: some View {
        let cutoff = Analytics.nowMs().saturatingSubtract(15 * 60 * 1000)
        let hit = rows.suffix(2000).filter { $0.startedAtMs >= cutoff && TLSIssue.of($0) == .rejected }
        if let sample = hit.last {
            let hosts = Set(hit.map(\.host))
            let device = Analytics.deviceLabel(sample)
            HStack(spacing: 10) {
                Image(systemName: "lock.trianglebadge.exclamationmark").foregroundStyle(Theme.s4xx)
                Text("\(hosts.count) host\(hosts.count == 1 ? "" : "s") on \(device) can't be read: the app rejected Relay's certificate, so those calls show no request or response.")
                    .font(.system(size: 12)).lineLimit(2)
                Spacer(minLength: 8)
                Button("What to do") { open.toggle() }.buttonStyle(PillButtonStyle()).font(.system(size: 12, weight: .medium))
                    .popover(isPresented: $open, arrowEdge: .bottom) {
                        if let a = store.advice(for: sample) {
                            VStack(alignment: .leading, spacing: 8) {
                                TLSAdviceView(advice: a)
                                Text("Affected: \(hosts.sorted().prefix(8).joined(separator: ", "))\(hosts.count > 8 ? "…" : "")")
                                    .font(.system(size: 11)).foregroundStyle(Theme.faint)
                            }
                            .frame(width: 420).padding(14)
                        }
                    }
            }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(Theme.s4xx.opacity(0.10))
            .overlay(alignment: .bottom) { Rectangle().fill(Theme.s4xx.opacity(0.30)).frame(height: 1) }
        }
    }
}

private extension UInt64 {
    func saturatingSubtract(_ n: UInt64) -> UInt64 { self > n ? self - n : 0 }
}
