import Foundation

/// A step in a JSON path: an object key or an array index.
enum PathPart: Hashable {
    case key(String)
    case index(Int)
}

/// One value in a JSON document (anything that is not an object or a non-empty array).
struct JSONField: Identifiable {
    enum Kind: String { case string, number, bool, null, empty }
    var id: String { path }
    let path: String
    let parts: [PathPart]
    let kind: Kind
    /// What an editor shows: a string without quotes, everything else as JSON text.
    let display: String
    /// The value as JSON text (strings keep their quotes), as the rewrite rules store it.
    let json: String
}

/// A parsed JSON document with helpers to list every field, change one, and print it.
struct JSONDoc {
    private(set) var root: Any

    init?(text: String) {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed, .mutableContainers]) else { return nil }
        root = obj
    }

    /// Indented JSON, `"key": value` (keys sorted, as Foundation does not keep their order).
    var pretty: String {
        guard let d = try? JSONSerialization.data(withJSONObject: root, options: [.sortedKeys, .withoutEscapingSlashes, .fragmentsAllowed]) else { return "" }
        return JSONFormat.pretty(String(data: d, encoding: .utf8) ?? "")
    }

    static func fragment(_ v: Any) -> String {
        guard let d = try? JSONSerialization.data(withJSONObject: v, options: [.fragmentsAllowed, .withoutEscapingSlashes]) else { return "null" }
        return String(data: d, encoding: .utf8) ?? "null"
    }

    private static func pathString(_ parts: [PathPart]) -> String {
        parts.reduce("$") { acc, p in
            switch p {
            case .index(let i): return acc + "[\(i)]"
            case .key(let k):
                return k.range(of: "^[A-Za-z0-9_-]+$", options: .regularExpression) != nil ? acc + "." + k : acc + "[\"\(k)\"]"
            }
        }
    }

    /// Every field in document order, up to `limit`.
    func fields(limit: Int = 5000) -> [JSONField] {
        var out: [JSONField] = []
        func add(_ parts: [PathPart], _ kind: JSONField.Kind, _ v: Any, empty: String? = nil) {
            let json = empty ?? Self.fragment(v)
            let display = (v as? String) ?? json
            out.append(JSONField(path: Self.pathString(parts), parts: parts, kind: kind, display: display, json: json))
        }
        func walk(_ v: Any, _ parts: [PathPart]) {
            guard out.count < limit else { return }
            if let d = v as? [String: Any] {
                if d.isEmpty { add(parts, .empty, v, empty: "{}") }
                for k in d.keys.sorted() { walk(d[k]!, parts + [.key(k)]) }
            } else if let a = v as? [Any] {
                if a.isEmpty { add(parts, .empty, v, empty: "[]") }
                for (i, x) in a.enumerated() { walk(x, parts + [.index(i)]) }
            } else if v is String {
                add(parts, .string, v)
            } else if v is NSNull {
                add(parts, .null, v)
            } else if let n = v as? NSNumber {
                add(parts, CFGetTypeID(n) == CFBooleanGetTypeID() ? .bool : .number, v)
            }
        }
        walk(root, [])
        return out
    }

    /// Sets the field at `parts` from what the user typed: a string stays a string; other kinds
    /// are read as JSON (`12`, `true`, `null`) and fall back to text.
    mutating func set(_ field: JSONField, to typed: String) {
        let value: Any
        if field.kind == .string {
            value = typed
        } else if let data = typed.trimmingCharacters(in: .whitespaces).data(using: .utf8),
                  let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) {
            value = parsed
        } else {
            value = typed
        }
        root = Self.setting(root, field.parts[...], value)
    }

    private static func setting(_ node: Any, _ parts: ArraySlice<PathPart>, _ value: Any) -> Any {
        guard let first = parts.first else { return value }
        let rest = parts.dropFirst()
        switch first {
        case .key(let k):
            var d = (node as? [String: Any]) ?? [:]
            d[k] = setting(d[k] ?? NSNull(), rest, value)
            return d
        case .index(let i):
            var a = (node as? [Any]) ?? []
            if i < a.count { a[i] = setting(a[i], rest, value) }
            return a
        }
    }
}

/// What changed between an original body and an edited one, as rewrite-rule changes: one
/// "set JSON field" per changed or added field and one "remove" per deleted field. When the
/// bodies are not both JSON, or too much changed, the whole new body is the change.
func bodyChangeSteps(original: String, edited: String) -> [StepDraft] {
    func whole() -> [StepDraft] {
        var s = StepDraft(kind: "set_body"); s.value = edited; return [s]
    }
    guard original != edited else { return [] }
    guard let a = JSONDoc(text: original), let b = JSONDoc(text: edited) else { return whole() }
    let before = Dictionary(a.fields(limit: 20_000).map { ($0.path, $0) }, uniquingKeysWith: { f, _ in f })
    let after = b.fields(limit: 20_000)
    var steps: [StepDraft] = []
    for f in after where before[f.path]?.json != f.json {
        var s = StepDraft(kind: "set_json"); s.path = f.path; s.value = f.json; steps.append(s)
    }
    let kept = Set(after.map(\.path))
    for (path, _) in before where !kept.contains(path) {
        var s = StepDraft(kind: "remove_json"); s.path = path; steps.append(s)
    }
    steps.sort { $0.path < $1.path }
    return steps.isEmpty || steps.count > 40 ? whole() : steps
}
