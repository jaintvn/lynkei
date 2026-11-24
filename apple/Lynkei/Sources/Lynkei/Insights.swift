import Foundation
import LynkeiKit

// Readers for calls that carry more than their URL says: GraphQL operations, gRPC methods and
// analytics events. All of them work from the headers and the body text Lynkei already captured.

private func header(_ headers: [FfiHeader], _ name: String) -> String? {
    headers.first { $0.name.lowercased() == name }?.value
}

private func jsonObject(_ text: String?) -> Any? {
    guard let text, let data = text.data(using: .utf8) else { return nil }
    return try? JSONSerialization.jsonObject(with: data)
}

private func compact(_ v: Any) -> String {
    if let s = v as? String { return s }
    if let n = v as? NSNumber { return n === kCFBooleanTrue ? "true" : (n === kCFBooleanFalse ? "false" : n.stringValue) }
    if v is NSNull { return "null" }
    if JSONSerialization.isValidJSONObject(v), let d = try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys]),
       let s = String(data: d, encoding: .utf8) { return s }
    return "\(v)"
}

private func properties(_ any: Any?) -> [(String, String)] {
    guard let d = any as? [String: Any] else { return [] }
    return d.keys.sorted().map { ($0, compact(d[$0]!)) }
}

private func formPairs(_ s: String) -> [(String, String)] {
    s.split(separator: "&").map { part in
        let kv = part.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
        let dec = { (x: Substring) in String(x).replacingOccurrences(of: "+", with: " ").removingPercentEncoding ?? String(x) }
        return (dec(kv[0]), kv.count > 1 ? dec(kv[1]) : "")
    }
}

private func queryItems(_ tx: FfiTransaction) -> [(String, String)] {
    guard let q = tx.path.split(separator: "?", maxSplits: 1).dropFirst().first else { return [] }
    return formPairs(String(q))
}

// MARK: - GraphQL

struct GraphQLOperation: Identifiable {
    let id = UUID()
    var name: String?
    var kind: String  // query, mutation, subscription
    var query: String?
    var variables: String?
    var persistedHash: String?
}

enum GraphQL {
    /// The operations in a call: one, or several when the app batches them.
    static func operations(_ tx: FfiTransaction) -> [GraphQLOperation] {
        var raw: [[String: Any]] = []
        if tx.method == "GET" {
            let q = queryItems(tx)
            var d: [String: Any] = [:]
            for (k, v) in q where ["query", "operationName", "variables"].contains(k) { d[k] = v }
            if let ext = q.first(where: { $0.0 == "extensions" })?.1, let e = jsonObject(ext) { d["extensions"] = e }
            if !d.isEmpty { raw = [d] }
        } else if let body = jsonObject(tx.requestBodyPreview) {
            if let a = body as? [[String: Any]] { raw = a } else if let o = body as? [String: Any] { raw = [o] }
        }
        return raw.compactMap { d in
            let query = d["query"] as? String
            let hash = ((d["extensions"] as? [String: Any])?["persistedQuery"] as? [String: Any])?["sha256Hash"] as? String
            guard query != nil || hash != nil else { return nil }
            let name = (d["operationName"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? query.flatMap(nameIn)
            var vars: String?
            if let v = d["variables"] {
                if let s = v as? String, !s.isEmpty { vars = s } else if !(v is NSNull), !(v as? [String: Any]).map({ $0.isEmpty }).orFalse { vars = pretty(v) }
            }
            return GraphQLOperation(name: name, kind: kind(of: query), query: query, variables: vars, persistedHash: hash)
        }
    }

    static func isGraphQL(_ tx: FfiTransaction) -> Bool {
        guard tx.state != .tunneled else { return false }
        if tx.path.lowercased().contains("graphql") { return true }
        guard tx.method == "POST", tx.requestBodyPreview?.contains("\"query\"") == true else { return false }
        return !operations(tx).isEmpty
    }

    /// `GetProducts`, or `GetCart + AddItem` for a batch. Nil for anonymous or non-GraphQL calls.
    static func label(_ tx: FfiTransaction) -> String? {
        guard isGraphQL(tx) else { return nil }
        let names = operations(tx).map { $0.name ?? "anonymous \($0.kind)" }
        return names.isEmpty ? nil : names.joined(separator: " + ")
    }

    private static func nameIn(_ query: String) -> String? {
        let re = try? NSRegularExpression(pattern: #"^\s*(?:query|mutation|subscription)\s+([A-Za-z_][A-Za-z0-9_]*)"#)
        let range = NSRange(query.startIndex..., in: query)
        guard let m = re?.firstMatch(in: query, range: range), let r = Range(m.range(at: 1), in: query) else { return nil }
        return String(query[r])
    }

    private static func kind(of query: String?) -> String {
        let q = (query ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if q.hasPrefix("mutation") { return "mutation" }
        if q.hasPrefix("subscription") { return "subscription" }
        return "query"
    }

    private static func pretty(_ v: Any) -> String {
        guard JSONSerialization.isValidJSONObject(v),
              let d = try? JSONSerialization.data(withJSONObject: v, options: [.prettyPrinted, .sortedKeys]) else { return compact(v) }
        return String(data: d, encoding: .utf8) ?? compact(v)
    }
}

private extension Optional where Wrapped == Bool {
    var orFalse: Bool { self ?? false }
}

// MARK: - gRPC

enum GRPC {
    static func isGRPC(_ tx: FfiTransaction) -> Bool {
        (header(tx.requestHeaders, "content-type") ?? "").lowercased().contains("grpc")
    }

    /// `/acme.shop.v1.Catalog/ListProducts` → (service, method).
    static func method(_ tx: FfiTransaction) -> (service: String, method: String)? {
        guard isGRPC(tx) else { return nil }
        let parts = tx.path.split(separator: "?").first.map(String.init)?.split(separator: "/").map(String.init) ?? []
        guard parts.count >= 2 else { return nil }
        return (parts[parts.count - 2], parts[parts.count - 1])
    }

    static func label(_ tx: FfiTransaction) -> String? {
        method(tx).map { "\($0.service)/\($0.method)" }
    }

    /// The call's result: `OK` or the code and message from the trailers when Lynkei saw them.
    static func result(_ tx: FfiTransaction) -> String? {
        guard isGRPC(tx) else { return nil }
        guard let code = header(tx.responseHeaders, "grpc-status") else { return nil }
        let names = ["OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND", "ALREADY_EXISTS", "PERMISSION_DENIED",
                     "RESOURCE_EXHAUSTED", "FAILED_PRECONDITION", "ABORTED", "OUT_OF_RANGE", "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE",
                     "DATA_LOSS", "UNAUTHENTICATED"]
        let name = Int(code).flatMap { $0 < names.count ? names[$0] : nil } ?? code
        if let msg = header(tx.responseHeaders, "grpc-message"), !msg.isEmpty { return "\(name) — \(msg.removingPercentEncoding ?? msg)" }
        return name
    }
}

// MARK: - Analytics events

struct TrackedEvent: Identifiable {
    let id = UUID()
    var vendor: String
    var name: String
    var user: String?
    var properties: [(String, String)]
}

enum EventDecoder {
    /// The analytics events a call carries, when it is a call to a tracking service Lynkei knows.
    static func events(_ tx: FfiTransaction) -> [TrackedEvent] {
        guard tx.state != .tunneled else { return [] }
        let host = tx.host.lowercased()
        let path = tx.path.split(separator: "?").first.map(String.init) ?? tx.path
        if host.contains("google-analytics.com") || host.contains("analytics.google.com") { return ga4(tx) }
        if host.contains("segment.") || host.contains("segment.io") || path.hasSuffix("/v1/batch") || path.hasSuffix("/v1/track") { return segment(tx) }
        if host.contains("amplitude.com") { return amplitude(tx) }
        if host.contains("mixpanel.com") { return mixpanel(tx) }
        if host.contains("appsflyer.com") { return appsflyer(tx) }
        return []
    }

    static func vendor(_ tx: FfiTransaction) -> String? { events(tx).first?.vendor }

    private static func ga4(_ tx: FfiTransaction) -> [TrackedEvent] {
        var lines: [[(String, String)]] = []
        if queryItems(tx).contains(where: { $0.0 == "en" }) { lines.append(queryItems(tx)) }
        for l in (tx.requestBodyPreview ?? "").split(whereSeparator: \.isNewline) { lines.append(formPairs(String(l))) }
        let common = queryItems(tx)
        return lines.compactMap { pairs in
            guard let en = pairs.first(where: { $0.0 == "en" })?.1 else { return nil }
            let props = pairs.filter { $0.0.hasPrefix("ep.") || $0.0.hasPrefix("epn.") }.map { (String($0.0.drop { $0 != "." }.dropFirst()), $0.1) }
            let user = (common + pairs).first { $0.0 == "uid" }?.1
            return TrackedEvent(vendor: "Google Analytics", name: en, user: user, properties: props)
        }
    }

    private static func segment(_ tx: FfiTransaction) -> [TrackedEvent] {
        guard let body = jsonObject(tx.requestBodyPreview) as? [String: Any] else { return [] }
        let list = (body["batch"] as? [[String: Any]]) ?? [body]
        return list.compactMap { e in
            guard let type = e["type"] as? String else { return nil }
            let name = (e["event"] as? String) ?? (e["name"] as? String) ?? type
            var props = properties(e["properties"])
            if type == "identify" { props = properties(e["traits"]) }
            let user = (e["userId"] as? String) ?? (e["anonymousId"] as? String)
            return TrackedEvent(vendor: "Segment", name: type == "track" ? name : "\(type): \(name)", user: user, properties: props)
        }
    }

    private static func amplitude(_ tx: FfiTransaction) -> [TrackedEvent] {
        var list: [[String: Any]] = []
        if let body = jsonObject(tx.requestBodyPreview) as? [String: Any] {
            list = body["events"] as? [[String: Any]] ?? []
        } else if let e = formPairs(tx.requestBodyPreview ?? "").first(where: { $0.0 == "e" })?.1,
                  let arr = jsonObject(e) as? [[String: Any]] { list = arr }
        return list.compactMap { e in
            guard let name = e["event_type"] as? String else { return nil }
            return TrackedEvent(vendor: "Amplitude", name: name, user: (e["user_id"] as? String) ?? (e["device_id"] as? String),
                                properties: properties(e["event_properties"]))
        }
    }

    private static func mixpanel(_ tx: FfiTransaction) -> [TrackedEvent] {
        var payload: Any? = jsonObject(tx.requestBodyPreview)
        if payload == nil {
            let data = (formPairs(tx.requestBodyPreview ?? "") + queryItems(tx)).first { $0.0 == "data" }?.1
            if let data {
                payload = jsonObject(data)
                if payload == nil, let raw = Data(base64Encoded: data, options: .ignoreUnknownCharacters) { payload = try? JSONSerialization.jsonObject(with: raw) }
            }
        }
        let list = (payload as? [[String: Any]]) ?? (payload as? [String: Any]).map { [$0] } ?? []
        return list.compactMap { e in
            guard let name = e["event"] as? String else { return nil }
            let props = e["properties"] as? [String: Any]
            return TrackedEvent(vendor: "Mixpanel", name: name, user: props?["distinct_id"] as? String, properties: properties(props))
        }
    }

    private static func appsflyer(_ tx: FfiTransaction) -> [TrackedEvent] {
        guard let body = jsonObject(tx.requestBodyPreview) as? [String: Any], let name = body["eventName"] as? String else { return [] }
        var props: [(String, String)] = []
        if let v = body["eventValue"] as? String, let parsed = jsonObject(v) { props = properties(parsed) }
        else if let v = body["eventValue"] { props = properties(v) }
        return [TrackedEvent(vendor: "AppsFlyer", name: name, user: body["appsflyer_id"] as? String, properties: props)]
    }
}
