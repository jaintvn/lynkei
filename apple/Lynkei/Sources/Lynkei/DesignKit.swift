import LynkeiKit
import SwiftUI

// Small building blocks from the design canvas, shared by every screen.

/// The 30×18 on/off switch used in rule lists.
struct LynkeiSwitch: View {
    @Binding var isOn: Bool
    var body: some View {
        Button { isOn.toggle() } label: {
            Capsule().fill(isOn ? Theme.accent : Theme.line2)
                .frame(width: 30, height: 18)
                .overlay(alignment: isOn ? .trailing : .leading) {
                    Circle().fill(Color.white).frame(width: 14, height: 14).padding(2)
                }
                .animation(.easeOut(duration: 0.12), value: isOn)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(isOn ? "On" : "Off")
    }
}

/// A row of pills in a rounded tray (the Dashboard/Traffic/Endpoints/Rules switcher, sort orders, tabs).
struct PillGroup<T: Hashable>: View {
    let items: [T]
    let title: (T) -> String
    @Binding var selection: T
    var height: CGFloat = 26
    var fontSize: CGFloat = 12

    var body: some View {
        HStack(spacing: 2) {
            ForEach(items, id: \.self) { item in
                let on = item == selection
                Button { selection = item } label: {
                    Text(title(item))
                        .font(.system(size: fontSize, weight: .medium))
                        .lineLimit(1)
                        .padding(.horizontal, 11)
                        .frame(height: height)
                        .foregroundStyle(on ? Theme.fg : Theme.muted)
                        .background(RoundedRectangle(cornerRadius: 6).fill(on ? Theme.panel : .clear))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: 9).fill(Theme.raised))
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.line))
    }
}

/// "DEVICES", "APPS"… small uppercase section label.
struct SectionLabel: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 11, weight: .semibold))
            .tracking(0.6)
            .foregroundStyle(Theme.faint)
    }
}

extension View {
    /// The rounded panel the Dashboard and Rules screens use.
    func card(padding: CGFloat = 16) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(RoundedRectangle(cornerRadius: 14).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.line))
    }
}

struct AccentButtonStyle: ButtonStyle {
    var height: CGFloat = 28
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.onAccent)
            .padding(.horizontal, 12)
            .frame(height: height)
            .background(RoundedRectangle(cornerRadius: height > 30 ? 9 : 7).fill(Theme.accent.opacity(configuration.isPressed ? 0.8 : 1)))
    }
}

/// A dashed "filter" or "add" chip.
struct DashedChip: View {
    let label: String
    var chevron = true
    var body: some View {
        HStack(spacing: 5) {
            Text(label)
            if chevron { Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold)) }
        }
        .font(.system(size: 12))
        .foregroundStyle(Theme.muted)
        .padding(.horizontal, 9)
        .frame(height: 26)
        .overlay(RoundedRectangle(cornerRadius: 13).strokeBorder(Theme.line2, style: StrokeStyle(lineWidth: 1, dash: [3, 2])))
    }
}

/// Line chart without axes, for the KPI cards and the Endpoints table.
struct Sparkline: View {
    let values: [Double]
    var color: Color
    var lineWidth: CGFloat = 1.6

    var body: some View {
        GeometryReader { geo in
            let pts = values.isEmpty ? [0, 0] : values
            let mx = pts.max() ?? 1, mn = pts.min() ?? 0
            let span = max(mx - mn, 0.0001)
            Path { p in
                for (i, v) in pts.enumerated() {
                    let x = geo.size.width * CGFloat(i) / CGFloat(max(pts.count - 1, 1))
                    let y = geo.size.height - 3 - (geo.size.height - 6) * CGFloat((v - mn) / span)
                    i == 0 ? p.move(to: CGPoint(x: x, y: y)) : p.addLine(to: CGPoint(x: x, y: y))
                }
            }
            .stroke(color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round, lineJoin: .round))
        }
    }
}

/// Horizontal bar on a track (error rate, p95, top hosts).
struct MeterBar: View {
    let fraction: Double
    var color: Color
    var height: CGFloat = 6
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.line)
                Capsule().fill(color).frame(width: max(height, geo.size.width * min(1, max(0, fraction))))
            }
        }
        .frame(height: height)
    }
}

/// The coloured letter badge for a rule kind (R, M, B, T…).
enum RuleStyle {
    static func letter(_ kind: String) -> String {
        switch kind {
        case "rewrite": return "R"
        case "map local", "map remote": return "M"
        case "breakpoint": return "B"
        case "throttle": return "T"
        case "block": return "X"
        case "dns spoof": return "D"
        default: return "•"
        }
    }
    static func color(_ kind: String) -> Color {
        switch kind {
        case "rewrite": return Theme.accent
        case "map local", "map remote": return Theme.s2xx
        case "breakpoint": return Theme.paused
        case "throttle": return Theme.s4xx
        case "block": return Theme.s5xx
        default: return Theme.s3xx
        }
    }
    /// Section title in the Rules list.
    static func group(_ kind: String) -> String {
        switch kind {
        case "rewrite": return "Modify"
        case "map local": return "Mock"
        case "map remote": return "Redirect"
        case "breakpoint": return "Pause"
        case "throttle": return "Slow network"
        case "block": return "Block"
        case "dns spoof": return "Reroute host"
        default: return kind.capitalized
        }
    }
    static let order = ["rewrite", "map local", "map remote", "breakpoint", "throttle", "block", "dns spoof"]
}

struct RuleLetter: View {
    let kind: String
    var size: CGFloat = 18
    var body: some View {
        let c = RuleStyle.color(kind)
        Text(RuleStyle.letter(kind))
            .font(.system(size: size > 20 ? 10 : 10, weight: .bold))
            .foregroundStyle(c)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size > 20 ? 6 : 5).fill(c.opacity(0.16)))
    }
}

/// Method colour: GET muted, everything else the accent, CONNECT magenta.
func methodColor(_ m: String) -> Color {
    switch m {
    case "GET": return Theme.muted
    case "CONNECT": return Theme.failed
    case "PUT", "PATCH": return Theme.s4xx
    default: return Theme.accent
    }
}

/// Lays children out left to right and wraps to a new line when they run out of width
/// (the filter chips, the "Add" chips).
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(subviews, width: proposal.width ?? .infinity).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let result = arrange(subviews, width: bounds.width)
        for (i, p) in result.points.enumerated() {
            subviews[i].place(at: CGPoint(x: bounds.minX + p.x, y: bounds.minY + p.y), proposal: .unspecified)
        }
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> (points: [CGPoint], size: CGSize) {
        var points: [CGPoint] = []
        var x: CGFloat = 0, y: CGFloat = 0, rowH: CGFloat = 0, maxX: CGFloat = 0
        for s in subviews {
            let size = s.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width { x = 0; y += rowH + spacing; rowH = 0 }
            points.append(CGPoint(x: x, y: y))
            x += size.width + spacing
            rowH = max(rowH, size.height)
            maxX = max(maxX, x - spacing)
        }
        return (points, CGSize(width: maxX, height: y + rowH))
    }
}

/// iOS (Apple logo), Android (robot head) or Mac icon for a device.
struct PlatformIcon: View {
    let platform: Analytics.Platform
    var size: CGFloat = 13
    var color: Color = Theme.muted

    var body: some View {
        switch platform {
        case .ios: Image(systemName: "apple.logo").font(.system(size: size)).foregroundStyle(color).frame(width: size + 3, height: size + 3)
        case .mac: Image(systemName: "laptopcomputer").font(.system(size: size)).foregroundStyle(color).frame(width: size + 3, height: size + 3)
        case .android:
            AndroidHead().fill(Color(hex: 0x3DDC84), style: FillStyle(eoFill: true))
                .frame(width: size + 1, height: size - 1).frame(width: size + 3, height: size + 3)
        }
    }
}

/// The Android robot head: a dome with two antennae and two eyes cut out.
struct AndroidHead: Shape {
    func path(in r: CGRect) -> Path {
        var p = Path()
        let w = r.width, h = r.height
        let c = CGPoint(x: r.midX, y: r.minY + h * 0.95)
        let rad = min(w * 0.5, h * 0.8)
        p.move(to: CGPoint(x: c.x - rad, y: c.y))
        p.addArc(center: c, radius: rad, startAngle: .degrees(180), endAngle: .degrees(0), clockwise: false)
        p.closeSubpath()
        let eye = rad * 0.16
        p.addEllipse(in: CGRect(x: c.x - rad * 0.5 - eye, y: c.y - rad * 0.45 - eye, width: eye * 2, height: eye * 2))
        p.addEllipse(in: CGRect(x: c.x + rad * 0.5 - eye, y: c.y - rad * 0.45 - eye, width: eye * 2, height: eye * 2))
        var a = Path()
        a.move(to: CGPoint(x: c.x - rad * 0.55, y: c.y - rad * 0.8)); a.addLine(to: CGPoint(x: c.x - rad * 0.8, y: r.minY))
        a.move(to: CGPoint(x: c.x + rad * 0.55, y: c.y - rad * 0.8)); a.addLine(to: CGPoint(x: c.x + rad * 0.8, y: r.minY))
        p.addPath(a.strokedPath(StrokeStyle(lineWidth: max(1, w * 0.07), lineCap: .round)))
        return p
    }
}

/// Switches focus mode (only favourites) on and off; shown once something is a favourite.
struct FocusToggle: View {
    @Environment(TrafficStore.self) private var store
    var body: some View {
        if store.hasFavourites {
            Button { store.focusMode.toggle() } label: {
                HStack(spacing: 6) {
                    Image(systemName: store.focusMode ? "star.fill" : "star").font(.system(size: 11))
                        .foregroundStyle(store.focusMode ? Theme.s4xx : Theme.muted)
                    Text("Focus").font(.system(size: 12.5, weight: .medium))
                    Text("\(store.favApps.count + store.favEndpoints.count)")
                        .font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted)
                }
                .padding(.horizontal, 10).frame(height: 30)
                .background(RoundedRectangle(cornerRadius: 8).fill(store.focusMode ? Theme.selection : .clear))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(store.focusMode ? Theme.accent : Theme.line2))
            }
            .buttonStyle(.plain)
            .fixedSize()
            .help(store.focusMode ? "Showing only your favourite apps and endpoints. Click to show everything." : "Showing everything. Click to show only favourites.")
        }
    }
}

/// The star that marks a favourite.
struct StarButton: View {
    let on: Bool
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Image(systemName: on ? "star.fill" : "star").font(.system(size: 11))
                .foregroundStyle(on ? Theme.s4xx : Theme.faint)
                .frame(width: 20, height: 20).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(on ? "Remove from favourites" : "Add to favourites")
    }
}

/// Lays children side by side in columns of equal unit width (`spans` units each), all as tall as the tallest.
struct EqualColumns: Layout {
    var spans: [Int]
    var spacing: CGFloat = 12

    private func unit(_ width: CGFloat) -> CGFloat {
        let total = CGFloat(spans.reduce(0, +))
        let gaps = spacing * CGFloat(max(spans.count - 1, 0))
        return (width - gaps) / max(total, 1)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let w = proposal.width ?? 800
        let u = unit(w)
        var h: CGFloat = 0
        for (i, s) in subviews.enumerated() {
            let sp = CGFloat(i < spans.count ? spans[i] : 1)
            h = max(h, s.sizeThatFits(ProposedViewSize(width: u * sp + spacing * (sp - 1), height: nil)).height)
        }
        return CGSize(width: w, height: h)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let u = unit(bounds.width)
        let h = sizeThatFits(proposal: ProposedViewSize(width: bounds.width, height: nil), subviews: subviews, cache: &cache).height
        var x = bounds.minX
        for (i, s) in subviews.enumerated() {
            let sp = CGFloat(i < spans.count ? spans[i] : 1)
            let w = u * sp + spacing * (sp - 1)
            s.place(at: CGPoint(x: x, y: bounds.minY), anchor: .topLeading, proposal: ProposedViewSize(width: w, height: h))
            x += w + spacing
        }
    }
}
