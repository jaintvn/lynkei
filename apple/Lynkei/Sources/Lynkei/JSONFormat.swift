import Foundation

/// Lays JSON text out the way people write it: two-space indent, `"key": value`, keys in the order
/// they arrived. Works on the text itself, so nothing is reordered and numbers keep their digits.
enum JSONFormat {
    static func pretty(_ text: String, indent: Int = 2) -> String {
        var out = ""
        out.reserveCapacity(text.utf8.count * 2)
        var depth = 0
        var inString = false
        var escaped = false
        let chars = Array(text.unicodeScalars)
        var i = 0
        func newline() { out += "\n" + String(repeating: " ", count: depth * indent) }
        func nextSignificant(_ from: Int) -> Unicode.Scalar? {
            var j = from
            while j < chars.count { if !" \n\r\t".unicodeScalars.contains(chars[j]) { return chars[j] }; j += 1 }
            return nil
        }
        while i < chars.count {
            let c = chars[i]
            if inString {
                out.unicodeScalars.append(c)
                if escaped { escaped = false } else if c == "\\" { escaped = true } else if c == "\"" { inString = false }
            } else {
                switch c {
                case "\"": inString = true; out.unicodeScalars.append(c)
                case "{", "[":
                    out.unicodeScalars.append(c)
                    if let n = nextSignificant(i + 1), n == (c == "{" ? "}" : "]") {
                        // empty object or array stays on one line
                        out.unicodeScalars.append(n)
                        var j = i + 1
                        while chars[j] != n { j += 1 }
                        i = j
                    } else {
                        depth += 1
                        newline()
                    }
                case "}", "]":
                    depth = max(0, depth - 1)
                    newline()
                    out.unicodeScalars.append(c)
                case ",": out += ","; newline()
                case ":": out += ": "
                case " ", "\n", "\r", "\t": break
                default: out.unicodeScalars.append(c)
                }
            }
            i += 1
        }
        return out
    }

    /// The same JSON on one line.
    static func minify(_ text: String) -> String {
        var out = ""
        var inString = false
        var escaped = false
        for c in text.unicodeScalars {
            if inString {
                out.unicodeScalars.append(c)
                if escaped { escaped = false } else if c == "\\" { escaped = true } else if c == "\"" { inString = false }
            } else if c == "\"" {
                inString = true; out.unicodeScalars.append(c)
            } else if !" \n\r\t".unicodeScalars.contains(c) {
                out.unicodeScalars.append(c)
            }
        }
        return out
    }
}
