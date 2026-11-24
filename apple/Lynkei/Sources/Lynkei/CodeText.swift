import AppKit
import SwiftUI

/// A monospaced text area that can show a whole body (even megabytes), be edited, and highlight
/// search matches. Built on NSTextView so large text stays fast and ⌘F also works.
struct CodeText: NSViewRepresentable {
    @Binding var text: String
    var editable = false
    var query = ""
    /// Which match is the current one (it is scrolled into view and drawn stronger).
    var current = 0
    var onMatches: (Int) -> Void = { _ in }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        let tv = scroll.documentView as! NSTextView
        tv.delegate = context.coordinator
        tv.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        tv.textColor = NSColor(Theme.fg)
        tv.insertionPointColor = NSColor(Theme.accent)
        tv.backgroundColor = .clear
        tv.drawsBackground = false
        tv.isRichText = false
        tv.allowsUndo = true
        tv.usesFindBar = true
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticTextReplacementEnabled = false
        tv.isAutomaticSpellingCorrectionEnabled = false
        tv.isGrammarCheckingEnabled = false
        tv.textContainerInset = NSSize(width: 6, height: 8)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        let tv = scroll.documentView as! NSTextView
        tv.isEditable = editable
        tv.isSelectable = true
        if tv.string != text {
            context.coordinator.updating = true
            tv.string = text
            tv.textColor = NSColor(Theme.fg)
            context.coordinator.updating = false
        }
        context.coordinator.highlight(tv)
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: CodeText
        var updating = false
        private var lastKey = ""
        private var lastCount = -1

        init(_ parent: CodeText) { self.parent = parent }

        func textDidChange(_ notification: Notification) {
            guard !updating, let tv = notification.object as? NSTextView else { return }
            parent.text = tv.string
        }

        private static let tokens = try! NSRegularExpression(
            pattern: "\"(?:[^\"\\\\]|\\\\.)*\"|-?\\d+(?:\\.\\d+)?(?:[eE][+-]?\\d+)?|\\btrue\\b|\\bfalse\\b|\\bnull\\b")
        private var colouredFor = ""

        /// JSON colours (keys, strings, numbers, literals) for bodies up to a megabyte.
        private func colorize(_ tv: NSTextView, _ lm: NSLayoutManager, _ full: NSRange) {
            let s = tv.string
            let trimmed = s.drop { $0.isWhitespace }
            guard full.length < 1_000_000, trimmed.first == "{" || trimmed.first == "[" else {
                lm.removeTemporaryAttribute(.foregroundColor, forCharacterRange: full)
                colouredFor = ""
                return
            }
            let sig = "\(full.length)|\(s.hashValue)"
            guard sig != colouredFor else { return }
            colouredFor = sig
            lm.removeTemporaryAttribute(.foregroundColor, forCharacterRange: full)
            let ns = s as NSString
            for m in Self.tokens.matches(in: s, range: full) {
                let r = m.range
                let first = ns.character(at: r.location)
                var color: NSColor
                if first == 34 {  // a quote: a key when a colon follows
                    var j = r.location + r.length
                    while j < ns.length, [32, 10, 9, 13].contains(ns.character(at: j)) { j += 1 }
                    color = (j < ns.length && ns.character(at: j) == 58) ? NSColor(Theme.key) : NSColor(Theme.str)
                } else {
                    color = NSColor(Theme.num)
                }
                lm.addTemporaryAttribute(.foregroundColor, value: color, forCharacterRange: r)
            }
        }

        func highlight(_ tv: NSTextView) {
            guard let lm = tv.layoutManager else { return }
            let full = NSRange(location: 0, length: (tv.string as NSString).length)
            lm.removeTemporaryAttribute(.backgroundColor, forCharacterRange: full)
            colorize(tv, lm, full)
            let q = parent.query
            var ranges: [NSRange] = []
            if !q.isEmpty {
                let ns = tv.string as NSString
                var from = 0
                while from < ns.length, ranges.count < 20_000 {
                    let r = ns.range(of: q, options: [.caseInsensitive], range: NSRange(location: from, length: ns.length - from))
                    if r.location == NSNotFound { break }
                    ranges.append(r)
                    from = r.location + max(r.length, 1)
                }
            }
            let cur = ranges.isEmpty ? 0 : ((parent.current % ranges.count) + ranges.count) % ranges.count
            for (i, r) in ranges.enumerated() {
                let strong = i == cur
                lm.addTemporaryAttribute(.backgroundColor, value: strong ? NSColor.systemOrange.withAlphaComponent(0.75) : NSColor.systemYellow.withAlphaComponent(0.3), forCharacterRange: r)
            }
            let key = "\(q)|\(cur)|\(ranges.count)"
            if key != lastKey, !ranges.isEmpty { tv.scrollRangeToVisible(ranges[cur]) }
            lastKey = key
            if ranges.count != lastCount {
                lastCount = ranges.count
                let n = ranges.count
                let report = parent.onMatches
                DispatchQueue.main.async { report(n) }
            }
        }
    }
}
