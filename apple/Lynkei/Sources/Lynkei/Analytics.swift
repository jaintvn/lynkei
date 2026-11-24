import Foundation
import LynkeiKit

/// Numbers for the Dashboard and Endpoints screens, computed from the captured calls.
enum Analytics {
    /// `/users/8231/orders/ab12cd34ef` → `/users/{id}/orders/{id}`. Query strings are dropped.
    static func foldPath(_ raw: String) -> String {
        let path = raw.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? raw
        let parts = path.split(separator: "/", omittingEmptySubsequences: false).map { seg -> String in
            isId(String(seg)) ? "{id}" : String(seg)
        }
        return parts.joined(separator: "/")
    }

    private static func isId(_ s: String) -> Bool {
        if s.isEmpty { return false }
        if s.allSatisfy(\.isNumber) { return true }
        if s.range(of: "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$", options: .regularExpression) != nil { return true }
        if s.count >= 8, s.allSatisfy(\.isHexDigit), s.contains(where: \.isNumber) { return true }
        if s.range(of: "^[A-Za-z]{1,4}_[0-9A-Za-z]{3,}$", options: .regularExpression) != nil, s.contains(where: \.isNumber) { return true }
        return false
    }

    /// `v2` from `/v2/products`, if the path starts with a version.
    static func apiVersion(_ path: String) -> String? {
        let first = path.split(separator: "/").first.map(String.init) ?? ""
        return first.range(of: "^v[0-9]+$", options: .regularExpression) != nil ? first : nil
    }

    static func isError(_ t: FfiTransaction) -> Bool { t.state == .failed || (t.status ?? 0) >= 400 }

    static func percentile(_ sorted: [UInt64], _ p: Double) -> UInt64 {
        guard !sorted.isEmpty else { return 0 }
        let i = Int((Double(sorted.count - 1) * p).rounded())
        return sorted[min(max(i, 0), sorted.count - 1)]
    }

    /// Identifies an endpoint for favourites: `GET api.acme.com/v2/products/{id}` (a tunnel: `CONNECT host:443`).
    static func endpointKey(_ t: FfiTransaction) -> String {
        t.state == .tunneled ? "\(t.method) \(t.host):\(t.port)" : "\(t.method) \(t.host)\(foldPath(t.path))\(operationSuffix(t))"
    }

    /// ` · GetProducts` for a GraphQL call, so each operation is its own endpoint (they all share one URL).
    static func operationSuffix(_ t: FfiTransaction) -> String {
        GraphQL.label(t).map { " · \($0)" } ?? ""
    }

    enum Platform { case ios, android, mac }
    nonisolated(unsafe) static var devicePlatforms: [String: Platform] = [:]
    static func platform(ofDevice name: String) -> Platform {
        if let p = devicePlatforms[name] { return p }
        if name == androidDevice { return .android }
        return .mac
    }

    static func nowMs() -> UInt64 { UInt64(Date().timeIntervalSince1970 * 1000) }

    static func ago(_ ms: UInt64, now: UInt64 = nowMs()) -> String {
        let d = now > ms ? (now - ms) / 1000 : 0
        if d < 2 { return "now" }
        if d < 60 { return "\(d) s ago" }
        if d < 3600 { return "\(d / 60) min ago" }
        return "\(d / 3600) h ago"
    }

    /// Short kind of body for the badge in the list: JSON, IMG, GQL, TLS…
    static func typeBadge(_ t: FfiTransaction) -> String {
        if t.state == .tunneled { return "TLS" }
        if !t.websocketMessages.isEmpty { return "WS" }
        let ct = t.responseHeaders.first { $0.name.lowercased() == "content-type" }?.value.lowercased() ?? ""
        if GRPC.isGRPC(t) { return "gRPC" }
        if GraphQL.isGraphQL(t) { return "GQL" }
        if EventDecoder.vendor(t) != nil { return "EVT" }
        if ct.contains("json") { return "JSON" }
        if ct.contains("image") { return "IMG" }
        if ct.contains("html") { return "HTML" }
        if ct.contains("javascript") { return "JS" }
        if ct.contains("css") { return "CSS" }
        if ct.contains("grpc") { return "RPC" }
        if ct.contains("xml") { return "XML" }
        if ct.contains("text") { return "TXT" }
        if ct.contains("font") { return "FONT" }
        return ""
    }

    /// Set from the device list so calls forwarded over adb can be named after the phone.
    nonisolated(unsafe) static var androidDevice: String?

    private static let forwarders: Set<String> = ["adb", "emulator", "ssh", "socat"]
    private static let genericAgents = ["okhttp", "dalvik", "cfnetwork", "mozilla", "java", "go-http-client", "python-requests",
                                        "curl", "apache-httpclient", "urlsession", "alamofire", "node", "axios", "dart", "libcurl"]

    /// True when the connection came through a tunnel (adb reverse, an emulator) rather than from an app on this Mac.
    static func isForwarded(_ t: FfiTransaction) -> Bool {
        guard let p = t.clientProcess?.lowercased(), !p.isEmpty else { return false }
        return forwarders.contains(p) || p.hasPrefix("qemu") || p.hasPrefix("adb")
    }

    /// The app's own name from its User-Agent (`AcmeShop/3.4.1 CFNetwork/…` gives `AcmeShop`), unless it is just an HTTP library.
    static func userAgentApp(_ t: FfiTransaction) -> String? {
        guard let ua = t.requestHeaders.first(where: { $0.name.lowercased() == "user-agent" })?.value else { return nil }
        let token = ua.split(whereSeparator: { $0 == "/" || $0 == " " }).first.map(String.init) ?? ""
        guard !token.isEmpty, !genericAgents.contains(token.lowercased()) else { return nil }
        return token.removingPercentEncoding ?? token
    }

    /// The app that made the call: the Mac program, or for a phone the app named in its User-Agent.
    static func appName(_ t: FfiTransaction) -> String {
        let p = t.clientProcess ?? ""
        if !p.isEmpty, !isForwarded(t) { return p }
        if let ua = userAgentApp(t) { return ua }
        if isForwarded(t) { return "\(androidDevice ?? "Android") app" }
        return t.clientAddr.split(separator: ":").first.map(String.init) ?? t.clientAddr
    }

    /// The device the call came from.
    static func deviceLabel(_ t: FfiTransaction) -> String {
        if isForwarded(t) { return androidDevice ?? "Android" }
        return "This Mac"
    }
}

struct EndpointStat: Identifiable {
    var id: String { "\(method) \(path) \(host)" }
    let method: String
    let path: String
    let host: String
    var key = ""
    var calls = 0
    var errors = 0
    var durations: [UInt64] = []
    var sizeTotal: UInt64 = 0
    var lastSeenMs: UInt64 = 0
    var buckets = [Double](repeating: 0, count: 16)
    var tunnel = false

    var errorRate: Double { calls == 0 ? 0 : Double(errors) / Double(calls) }
    var p50: UInt64 { Analytics.percentile(durations.sorted(), 0.5) }
    var p95: UInt64 { Analytics.percentile(durations.sorted(), 0.95) }
    var avgSize: UInt64 { calls == 0 ? 0 : sizeTotal / UInt64(calls) }
}

extension Analytics {
    /// One row per method + folded path + host, over the last 15 minutes of traffic (or all of it, if older).
    static func endpoints(_ txs: [FfiTransaction]) -> [EndpointStat] {
        let now = nowMs()
        let window: UInt64 = 15 * 60 * 1000
        var map: [String: EndpointStat] = [:]
        for t in txs {
            let tunnel = t.state == .tunneled
            let path = tunnel ? "\(t.host):\(t.port)" : foldPath(t.path) + operationSuffix(t)
            let key = "\(t.method) \(path) \(t.host)"
            var s = map[key] ?? EndpointStat(method: t.method, path: path, host: tunnel ? "TLS passthrough" : t.host)
            s.tunnel = tunnel
            s.key = endpointKey(t)
            s.calls += 1
            if isError(t) { s.errors += 1 }
            s.durations.append(t.durationMs)
            s.sizeTotal += t.responseBodySize
            s.lastSeenMs = max(s.lastSeenMs, t.startedAtMs)
            if now > t.startedAtMs, now - t.startedAtMs < window {
                let i = 15 - Int(Double(now - t.startedAtMs) / Double(window) * 16)
                s.buckets[min(15, max(0, i))] += 1
            }
            map[key] = s
        }
        return Array(map.values)
    }
}
