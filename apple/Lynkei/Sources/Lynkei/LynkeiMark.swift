import AppKit
import SwiftUI

/// The Lynkei mark: an "L" made of two linked nodes, with a packet travelling along it.
/// Same shape as the app icon (scripts/make-icon.swift).
struct LynkeiMark: View {
    var size: CGFloat = 18
    var color: Color = Theme.accent
    var packet: Color = Theme.s2xx

    var body: some View {
        Canvas { ctx, sz in
            func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x * sz.width, y: y * sz.height) }
            let w = 0.10 * sz.width, ring = 0.10 * sz.width
            let a = CGPoint(x: 0.26, y: 0.24), b = CGPoint(x: 0.26, y: 0.76), c = CGPoint(x: 0.74, y: 0.76)
            var path = Path()
            path.move(to: p(a.x, a.y + 0.12))
            path.addArc(tangent1End: p(b.x, b.y), tangent2End: p(c.x - 0.12, c.y), radius: 0.13 * sz.width)
            path.addLine(to: p(c.x - 0.12, c.y))
            let style = StrokeStyle(lineWidth: w, lineCap: .round, lineJoin: .round)
            ctx.stroke(path, with: .color(color), style: style)
            for n in [a, c] {
                let q = p(n.x, n.y)
                ctx.stroke(Path(ellipseIn: CGRect(x: q.x - ring, y: q.y - ring, width: ring * 2, height: ring * 2)),
                           with: .color(color), lineWidth: w * 0.9)
            }
            let d = 0.065 * sz.width, q = p(0.50, b.y)
            ctx.fill(Path(ellipseIn: CGRect(x: q.x - d, y: q.y - d, width: d * 2, height: d * 2)), with: .color(packet))
        }
        .frame(width: size, height: size)
    }
}

extension LynkeiMark {
    /// The mark as a monochrome template image for the menu bar (macOS tints it for light and dark bars).
    static func menuBarImage(side: CGFloat = 18) -> NSImage {
        let img = NSImage(size: NSSize(width: side, height: side), flipped: true) { r in
            guard let ctx = NSGraphicsContext.current?.cgContext else { return false }
            func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x * r.width, y: y * r.height) }
            let w = 0.11 * r.width, ring = 0.10 * r.width
            let a = CGPoint(x: 0.26, y: 0.24), b = CGPoint(x: 0.26, y: 0.76), c = CGPoint(x: 0.74, y: 0.76)
            ctx.setStrokeColor(NSColor.black.cgColor); ctx.setFillColor(NSColor.black.cgColor)
            ctx.setLineWidth(w); ctx.setLineCap(.round); ctx.setLineJoin(.round)
            let path = CGMutablePath()
            path.move(to: p(a.x, a.y + 0.12))
            path.addArc(tangent1End: p(b.x, b.y), tangent2End: p(c.x - 0.12, c.y), radius: 0.13 * r.width)
            path.addLine(to: p(c.x - 0.12, c.y))
            ctx.addPath(path); ctx.strokePath()
            for n in [a, c] {
                let q = p(n.x, n.y)
                ctx.setLineWidth(w * 0.9)
                ctx.strokeEllipse(in: CGRect(x: q.x - ring, y: q.y - ring, width: ring * 2, height: ring * 2))
            }
            let d = 0.075 * r.width, q = p(0.50, b.y)
            ctx.fillEllipse(in: CGRect(x: q.x - d, y: q.y - d, width: d * 2, height: d * 2))
            return true
        }
        img.isTemplate = true
        return img
    }
}
