import AppKit
import SwiftUI

/// Tokens from the Claude Design canvas "Lynkei UI — Claude Design" ("Graphite + Electric Indigo").
/// Every colour has the canvas's dark and light value and follows the system appearance.
enum Theme {
    static let bg = dyn(dark: (0x0D0E11, 1), light: (0xF5F6F8, 1))
    static let panel = dyn(dark: (0x13151A, 1), light: (0xFFFFFF, 1))
    static let raised = dyn(dark: (0x1A1D23, 1), light: (0xF1F3F6, 1))
    static let hover = dyn(dark: (0x1F232B, 1), light: (0xEBEEF3, 1))
    static let line = dyn(dark: (0xFFFFFF, 0.07), light: (0x101828, 0.08))
    static let line2 = dyn(dark: (0xFFFFFF, 0.12), light: (0x101828, 0.14))
    static let fg = dyn(dark: (0xE9EBF0, 1), light: (0x14161B, 1))
    static let muted = dyn(dark: (0xA1A7B4, 1), light: (0x4D5563, 1))
    static let faint = dyn(dark: (0x8A91A0, 1), light: (0x5F6775, 1))
    static let accent = dyn(dark: (0x7C8CFF, 1), light: (0x4F5BD5, 1))
    static let selection = dyn(dark: (0x7C8CFF, 0.16), light: (0x4F5BD5, 0.11))
    static let onAccent = dyn(dark: (0x0D0E11, 1), light: (0xFFFFFF, 1))

    static let s2xx = dyn(dark: (0x3DD68C, 1), light: (0x0E7A50, 1))
    static let s3xx = dyn(dark: (0x9AACC6, 1), light: (0x4F6079, 1))
    static let s4xx = dyn(dark: (0xF5B544, 1), light: (0x985E00, 1))
    static let s5xx = dyn(dark: (0xFF6B70, 1), light: (0xC8262E, 1))
    static let failed = dyn(dark: (0xE879F0, 1), light: (0x9C2FA1, 1))
    static let paused = dyn(dark: (0xFF9F43, 1), light: (0xB4540A, 1))

    /// JSON colours: keys, strings, numbers.
    static let key = dyn(dark: (0x9CB4FF, 1), light: (0x3B4BC8, 1))
    static let str = dyn(dark: (0x7EE2B8, 1), light: (0x0E7A50, 1))
    static let num = dyn(dark: (0xF5B544, 1), light: (0x985E00, 1))

    static let mono = Font.system(size: 12, design: .monospaced)
    static let monoSmall = Font.system(size: 11, weight: .semibold, design: .monospaced)

    private static func dyn(dark: (UInt32, Double), light: (UInt32, Double)) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            let (hex, alpha) = isDark ? dark : light
            return NSColor(
                srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
                green: CGFloat((hex >> 8) & 0xFF) / 255,
                blue: CGFloat(hex & 0xFF) / 255,
                alpha: alpha)
        })
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}
