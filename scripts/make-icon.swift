#!/usr/bin/env swift
// Draws the Lynkei app icon and writes apple/Lynkei/Resources/AppIcon.icns.
//   swift scripts/make-icon.swift
// The mark (an "L" made of two linked nodes and a packet at the corner) is the same shape as LynkeiMark.swift.
import AppKit

func mark(_ ctx: CGContext, in r: CGRect, color: CGColor, dot: CGColor) {
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: r.minX + x * r.width, y: r.minY + (1 - y) * r.height) }
    let w = 0.085 * r.width, ring = 0.095 * r.width
    let a = (x: 0.26, y: 0.24), b = (x: 0.26, y: 0.76), c = (x: 0.74, y: 0.76)
    ctx.setStrokeColor(color); ctx.setLineWidth(w); ctx.setLineCap(.round); ctx.setLineJoin(.round)
    let path = CGMutablePath()
    path.move(to: p(a.x, a.y + 0.12))
    path.addArc(tangent1End: p(b.x, b.y), tangent2End: p(c.x - 0.12, c.y), radius: 0.13 * r.width)
    path.addLine(to: p(c.x - 0.12, c.y))
    ctx.addPath(path); ctx.strokePath()
    for n in [a, c] {
        let rect = CGRect(x: p(n.x, n.y).x - ring, y: p(n.x, n.y).y - ring, width: ring * 2, height: ring * 2)
        ctx.setLineWidth(w * 0.9); ctx.strokeEllipse(in: rect)
    }
    let d = 0.055 * r.width, q = p(0.50, b.y)
    ctx.setFillColor(dot); ctx.fillEllipse(in: CGRect(x: q.x - d, y: q.y - d, width: d * 2, height: d * 2))
}

func icon(_ px: Int) -> Data {
    let s = CGFloat(px)
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    let g = NSGraphicsContext(bitmapImageRep: rep)!
    NSGraphicsContext.current = g
    let ctx = g.cgContext
    let cs = CGColorSpaceCreateDeviceRGB()
    let body = CGRect(x: s * 0.098, y: s * 0.098, width: s * 0.804, height: s * 0.804)
    let shape = CGPath(roundedRect: body, cornerWidth: body.width * 0.2237, cornerHeight: body.width * 0.2237, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.01), blur: s * 0.025, color: CGColor(gray: 0, alpha: 0.35))
    ctx.addPath(shape); ctx.setFillColor(CGColor(gray: 0.1, alpha: 1)); ctx.fillPath()
    ctx.restoreGState()
    ctx.saveGState(); ctx.addPath(shape); ctx.clip()
    let bg = CGGradient(colorsSpace: cs, colors: [CGColor(red: 0.13, green: 0.15, blue: 0.27, alpha: 1), CGColor(red: 0.05, green: 0.055, blue: 0.09, alpha: 1)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(bg, start: CGPoint(x: body.minX, y: body.maxY), end: CGPoint(x: body.maxX, y: body.minY), options: [])
    ctx.restoreGState()
    // soft top highlight
    ctx.saveGState(); ctx.addPath(shape); ctx.clip()
    let hl = CGGradient(colorsSpace: cs, colors: [CGColor(gray: 1, alpha: 0.10), CGColor(gray: 1, alpha: 0)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(hl, start: CGPoint(x: s / 2, y: body.maxY), end: CGPoint(x: s / 2, y: body.midY), options: [])
    ctx.restoreGState()
    ctx.addPath(shape); ctx.setStrokeColor(CGColor(gray: 1, alpha: 0.10)); ctx.setLineWidth(max(1, s * 0.004)); ctx.strokePath()
    mark(ctx, in: body.insetBy(dx: body.width * 0.04, dy: body.width * 0.04), color: CGColor(red: 0.62, green: 0.67, blue: 1, alpha: 1), dot: CGColor(red: 0.35, green: 0.93, blue: 0.7, alpha: 1))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let root = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent().deletingLastPathComponent()
let set = FileManager.default.temporaryDirectory.appendingPathComponent("lynkei.iconset")
try? FileManager.default.removeItem(at: set)
try FileManager.default.createDirectory(at: set, withIntermediateDirectories: true)
for (name, px) in [("16", 16), ("16@2x", 32), ("32", 32), ("32@2x", 64), ("128", 128), ("128@2x", 256), ("256", 256), ("256@2x", 512), ("512", 512), ("512@2x", 1024)] {
    try icon(px).write(to: set.appendingPathComponent("icon_\(name.replacingOccurrences(of: "@2x", with: "")).png".replacingOccurrences(of: "icon_\(name.replacingOccurrences(of: "@2x", with: ""))", with: name.contains("@") ? "icon_\(name.replacingOccurrences(of: "@2x", with: ""))x\(name.replacingOccurrences(of: "@2x", with: ""))@2x" : "icon_\(name)x\(name)")))
}
let out = root.appendingPathComponent("apple/Lynkei/Resources/AppIcon.icns")
let p = Process(); p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", set.path, "-o", out.path]; try p.run(); p.waitUntilExit()
try icon(1024).write(to: root.appendingPathComponent("apple/Lynkei/Resources/AppIcon-1024.png"))
print("wrote", out.path)
