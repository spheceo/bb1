// Draws the DMG window background (flat graphite like the app, the bb1 mark, an arrow between the
// two icons) at 1x and 2x. Run: swift scripts/make-dmg-background.swift <out-dir>
// The window's content is 640×400 pt; dmg-settings.py puts bb1.app at (170, 236) and Applications at (470, 236).
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")
let size = NSSize(width: 640, height: 400)

func graphite(_ v: CGFloat) -> NSColor { NSColor(srgbRed: v, green: v * 1.03, blue: v * 1.12, alpha: 1) }

/// The "Half" mark: rounded screen outline, left half solid.
func drawMark(in rect: NSRect, color: NSColor) {
    let line = rect.width * 7.5 / 76
    let r = rect.insetBy(dx: line / 2, dy: line / 2)
    let path = NSBezierPath(roundedRect: r, xRadius: rect.width * 14 / 76, yRadius: rect.width * 14 / 76)
    path.lineWidth = line
    color.setStroke()
    color.setFill()
    path.stroke()
    NSGraphicsContext.saveGraphicsState()
    NSBezierPath(rect: NSRect(x: rect.minX - line, y: rect.minY - line, width: rect.width / 2 + line, height: rect.height + 2 * line)).addClip()
    path.fill()
    path.stroke()
    NSGraphicsContext.restoreGraphicsState()
}

func text(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor, centerX: CGFloat, top: CGFloat) {
    let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color]
    let str = NSAttributedString(string: s, attributes: attrs)
    let w = str.size()
    str.draw(at: NSPoint(x: centerX - w.width / 2, y: 400 - top - w.height))
}

func render(scale: CGFloat) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale),
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = size
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

    graphite(0.035).setFill()
    NSRect(origin: .zero, size: size).fill()

    // Positions are in points from the top (like dmg-settings.py); AppKit's y runs up, hence 400 - y.
    // Header: the mark and a short instruction.
    drawMark(in: NSRect(x: 320 - 15, y: 400 - 40 - 24, width: 30, height: 24), color: .white)
    text("Install bb1", size: 20, weight: .bold, color: .white, centerX: 320, top: 76)
    text("Drag bb1 into your Applications folder.", size: 13, weight: .regular,
         color: NSColor.white.withAlphaComponent(0.55), centerX: 320, top: 106)

    for x in [170.0, 470.0] {
        // A tile behind each icon…
        let tile = NSRect(x: x - 86, y: 400 - 344, width: 172, height: 194)
        graphite(0.075).setFill()
        NSBezierPath(roundedRect: tile, xRadius: 24, yRadius: 24).fill()
        NSColor.white.withAlphaComponent(0.06).setStroke()
        let edge = NSBezierPath(roundedRect: tile.insetBy(dx: 0.5, dy: 0.5), xRadius: 24, yRadius: 24)
        edge.lineWidth = 1
        edge.stroke()
        // …and a light tag where Finder writes the name (Finder draws it in dark text).
        let tag = NSRect(x: x - 58, y: 400 - 324, width: 116, height: 22)
        NSColor(white: 0.9, alpha: 1).setFill()
        NSBezierPath(roundedRect: tag, xRadius: 11, yRadius: 11).fill()
    }

    // The Applications folder's own icon under the Applications shortcut: Finder on macOS 26 often
    // leaves a shortcut's icon blank in disk image windows, and this keeps the target visible either way.
    let folder = NSWorkspace.shared.icon(forFile: "/Applications")
    folder.size = NSSize(width: 112, height: 112)
    folder.draw(in: NSRect(x: 470 - 56, y: 400 - 236 - 56, width: 112, height: 112))

    // Arrow between them, level with the icons.
    let y = 400 - 236.0
    let arrow = NSBezierPath()
    arrow.move(to: NSPoint(x: 282, y: y))
    arrow.line(to: NSPoint(x: 356, y: y))
    arrow.move(to: NSPoint(x: 344, y: y + 10))
    arrow.line(to: NSPoint(x: 358, y: y))
    arrow.line(to: NSPoint(x: 344, y: y - 10))
    arrow.lineWidth = 2.5
    arrow.lineCapStyle = .round
    arrow.lineJoinStyle = .round
    NSColor.white.withAlphaComponent(0.4).setStroke()
    arrow.stroke()

    text("Then open bb1 from Applications. If macOS won't open it the first time:", size: 11.5, weight: .regular,
         color: NSColor.white.withAlphaComponent(0.4), centerX: 320, top: 358)
    text("System Settings → Privacy & Security → Open Anyway.", size: 11.5, weight: .medium,
         color: NSColor.white.withAlphaComponent(0.6), centerX: 320, top: 375)

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

try! FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
try! render(scale: 1).write(to: out.appendingPathComponent("background.png"))
try! render(scale: 2).write(to: out.appendingPathComponent("background@2x.png"))
print("wrote \(out.path)/background.png and background@2x.png")
