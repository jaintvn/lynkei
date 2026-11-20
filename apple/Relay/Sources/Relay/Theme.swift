import SwiftUI

/// Tokens from the chosen Claude Design canvas ("Graphite + Electric Indigo"). Dark-first.
enum Theme {
    static let bg = Color(hex: 0x0D0E11)
    static let panel = Color(hex: 0x13151A)
    static let raised = Color(hex: 0x1A1D23)
    static let line = Color.white.opacity(0.07)
    static let line2 = Color.white.opacity(0.12)
    static let fg = Color(hex: 0xE9EBF0)
    static let muted = Color(hex: 0xA1A7B4)
    static let faint = Color(hex: 0x8A91A0)
    static let accent = Color(hex: 0x7C8CFF)
    static let selection = Color(hex: 0x7C8CFF).opacity(0.16)

    static let s2xx = Color(hex: 0x3DD68C)
    static let s3xx = Color(hex: 0x9AACC6)
    static let s4xx = Color(hex: 0xF5B544)
    static let s5xx = Color(hex: 0xFF6B70)
    static let failed = Color(hex: 0xE879F0)
    static let paused = Color(hex: 0xFF9F43)

    static let mono = Font.system(size: 12, design: .monospaced)
    static let monoSmall = Font.system(size: 11, weight: .semibold, design: .monospaced)
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
