import Foundation
import LynkeiKit

/// A phone or tablet Lynkei has seen on the network, found from the traffic it sends.
/// (iPhones on Wi-Fi can't be asked for their details, so Lynkei reads them off their requests.)
struct NetworkClient: Identifiable {
    enum Kind { case apple, android, other }

    let ip: String
    let kind: Kind
    let requests: Int
    let decrypted: Int
    let tunneled: Int
    let refused: Int
    /// Certificate refusals for hosts that are NOT Apple's own (those are pinned and always refuse).
    let refusedOther: Int
    let onlyAppleSeen: Bool
    let lastSeenMs: UInt64
    let firstSeenMs: UInt64

    var id: String { ip }

    var title: String {
        switch kind {
        case .apple: return "iPhone or iPad"
        case .android: return "Android phone"
        case .other: return "Device on your network"
        }
    }

    /// Proxy: requests are arriving. Certificate: decrypted requests prove the device trusts Lynkei.
    var proxyStep: FfiStep {
        FfiStep(state: "done", detail: "\(requests) requests are reaching Lynkei from \(ip)")
    }

    var certificateStep: FfiStep {
        if decrypted > 0 {
            return FfiStep(state: "done", detail: "Lynkei can read this device’s HTTPS (\(decrypted) decrypted requests)")
        }
        if refusedOther > 0 {
            return FfiStep(
                state: "needs_action",
                detail: kind == .apple
                    ? "The device refused Lynkei’s certificate. Install the profile from the QR code, then Settings → General → About → Certificate Trust Settings → turn on Lynkei CA."
                    : "The device refused Lynkei’s certificate. Install it (scan the QR code) and use a browser."
            )
        }
        return FfiStep(
            state: "pending",
            detail: onlyAppleSeen
                ? "Not confirmed yet: so far only Apple’s own services were seen, and those are never decrypted. Open Safari and load any HTTPS site; if this turns green you’re set. If not, install the profile and turn on Lynkei CA in Certificate Trust Settings."
                : "Not confirmed yet. Open a browser on the device and load an HTTPS site."
        )
    }

    var trafficStep: FfiStep {
        FfiStep(state: "done", detail: "Last request \(Self.ago(lastSeenMs))")
    }

    private static func ago(_ ms: UInt64) -> String {
        let secs = max(0, Int(Date().timeIntervalSince1970) - Int(ms / 1000))
        if secs < 5 { return "just now" }
        if secs < 90 { return "\(secs) s ago" }
        return "\(secs / 60) min ago"
    }

    /// Groups recent transactions by client address, skipping this Mac (loopback: simulators and
    /// adb-tunnelled phones, which already have their own cards).
    static func detect(from transactions: [FfiTransaction]) -> [NetworkClient] {
        var buckets: [String: [FfiTransaction]] = [:]
        for t in transactions {
            guard let ip = clientIP(t.clientAddr), !isLocal(ip) else { continue }
            buckets[ip, default: []].append(t)
        }
        return buckets.map { ip, txs in
            NetworkClient(
                ip: ip,
                kind: classify(txs),
                requests: txs.filter { $0.state != .inProgress }.count,
                decrypted: txs.filter { $0.decrypted }.count,
                tunneled: txs.filter { $0.state == .tunneled }.count,
                refused: txs.filter { $0.state == .failed && ($0.error ?? "").contains("handshake") }.count,
                refusedOther: txs.filter { $0.state == .failed && ($0.error ?? "").contains("handshake") && !isAppleHost($0.host) }.count,
                onlyAppleSeen: txs.allSatisfy { isAppleHost($0.host) },
                lastSeenMs: txs.map(\.startedAtMs).max() ?? 0,
                firstSeenMs: txs.map(\.startedAtMs).min() ?? 0
            )
        }
        .sorted { $0.lastSeenMs > $1.lastSeenMs }
    }

    private static func isAppleHost(_ host: String) -> Bool {
        let h = host.lowercased()
        return ["apple.com", "icloud.com", "mzstatic.com", "cdn-apple.com", "itunes.com", "apple-cloudkit.com"].contains { h == $0 || h.hasSuffix("." + $0) }
    }

    private static func clientIP(_ addr: String) -> String? {
        if addr.hasPrefix("[") { return addr.split(separator: "]").first.map { String($0.dropFirst()) } }
        return addr.split(separator: ":").first.map(String.init)
    }

    private static func isLocal(_ ip: String) -> Bool {
        ip == "127.0.0.1" || ip == "::1" || ip.hasPrefix("169.254.") || ip.hasPrefix("fe80")
    }

    private static func classify(_ txs: [FfiTransaction]) -> Kind {
        var apple = 0, android = 0
        for t in txs.prefix(400) {
            let host = t.host.lowercased()
            if isAppleHost(host) { apple += 1 }
            if host.hasSuffix("googleapis.com") || host.hasSuffix("gstatic.com") || host.hasSuffix("android.com") { android += 1 }
            for h in t.requestHeaders where h.name.lowercased() == "user-agent" {
                let ua = h.value
                if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("CFNetwork") || ua.contains("Darwin") { apple += 3 }
                if ua.contains("Android") || ua.contains("okhttp") || ua.contains("Dalvik") { android += 3 }
            }
        }
        if apple == 0 && android == 0 { return .other }
        return apple >= android ? .apple : .android
    }
}
