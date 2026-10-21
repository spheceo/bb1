// Draws the DMG window background at 1x and 2x. Run: swift scripts/make-dmg-background.swift <out-dir>
//
// The window is the bb1 mark: left half light, right half graphite. bb1.app sits on the light half,
// Applications on the dark half, and an arc carries you across the split (dark on light, white on dark).
// Finder writes icon names in near-black over a background picture, so the Applications shortcut is
// named " " (no visible name) and its name is drawn here in white instead.
// Content is 640×400 pt; dmg-settings.py puts bb1.app at (160, 196) and Applications at (480, 196).
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")
let W: CGFloat = 640, H: CGFloat = 400

func graphite(_ v: CGFloat) -> NSColor { NSColor(srgbRed: v, green: v * 1.03, blue: v * 1.12, alpha: 1) }
let light = NSColor(srgbRed: 0.95, green: 0.95, blue: 0.955, alpha: 1)
let dark = graphite(0.035)
let ink = graphite(0.08)

/// Points from the top-left (like Finder's icon positions) → AppKit's bottom-left.
func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: x, y: H - y) }

/// Text centred on x (or left-aligned at x), top edge at y from the top.
func text(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor, x: CGFloat, top: CGFloat, centered: Bool = true) {
    let str = NSAttributedString(string: s, attributes: [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color])
    let w = str.size()
    str.draw(at: NSPoint(x: centered ? x - w.width / 2 : x, y: H - top - w.height))
}

func render(scale: CGFloat) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: W, height: H)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let leftHalf = NSRect(x: 0, y: 0, width: W / 2, height: H)
    let rightHalf = NSRect(x: W / 2, y: 0, width: W / 2, height: H)
    light.setFill(); leftHalf.fill()
    dark.setFill(); rightHalf.fill()

    // The arc from bb1 to Applications, half ink and half white like the mark.
    let arc = NSBezierPath()
    arc.move(to: p(222, 168))
    arc.curve(to: p(412, 168), controlPoint1: p(270, 96), controlPoint2: p(364, 96))
    let tip = p(412, 168)
    arc.move(to: NSPoint(x: tip.x - 13, y: tip.y + 5))
    arc.line(to: tip)
    arc.line(to: NSPoint(x: tip.x - 3, y: tip.y + 13))
    arc.lineWidth = 2.5
    arc.lineCapStyle = .round
    arc.lineJoinStyle = .round
    for (half, color) in [(leftHalf, ink.withAlphaComponent(0.55)), (rightHalf, NSColor.white.withAlphaComponent(0.7))] {
        NSGraphicsContext.saveGraphicsState()
        NSBezierPath(rect: half).addClip()
        color.setStroke()
        arc.stroke()
        NSGraphicsContext.restoreGraphicsState()
    }

    // The Applications folder icon and its name (Finder may leave the shortcut's own icon blank on macOS 26).
    let folder = NSWorkspace.shared.icon(forFile: "/Applications")
    folder.size = NSSize(width: 112, height: 112)
    folder.draw(in: NSRect(x: 480 - 56, y: H - 196 - 56, width: 112, height: 112))
    text("Applications", size: 13, weight: .regular, color: .white, x: 480, top: 262)

    // The one instruction, along the bottom of the light half.
    text("Drag bb1 into Applications", size: 12, weight: .medium, color: ink.withAlphaComponent(0.55), x: 160, top: 346)

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

try! FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
try! render(scale: 1).write(to: out.appendingPathComponent("background.png"))
try! render(scale: 2).write(to: out.appendingPathComponent("background@2x.png"))
print("wrote \(out.path)/background.png and background@2x.png")
