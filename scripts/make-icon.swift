// Draws the bb1 app icon ("Half": a rounded screen, left half solid) at every macOS size
// and writes Resources/AppIcon.icns. Run: swift scripts/make-icon.swift
import AppKit

let root = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

func draw(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px) / 1024
    // Apple's icon grid: 824 pt tile centred on a 1024 canvas.
    let tile = NSRect(x: 100 * s, y: 100 * s, width: 824 * s, height: 824 * s)
    let tilePath = NSBezierPath(roundedRect: tile, xRadius: 185 * s, yRadius: 185 * s)

    // Soft shadow under the tile, like system icons.
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
    shadow.shadowBlurRadius = 18 * s
    shadow.shadowOffset = NSSize(width: 0, height: -8 * s)
    shadow.set()
    NSColor.black.setFill()
    tilePath.fill()
    NSGraphicsContext.restoreGraphicsState()

    // Near-black tile with a faint top-to-bottom sheen.
    NSGradient(starting: NSColor(srgbRed: 0.16, green: 0.165, blue: 0.18, alpha: 1),
               ending: NSColor(srgbRed: 0.055, green: 0.058, blue: 0.067, alpha: 1))!.draw(in: tilePath, angle: -90)
    NSColor.white.withAlphaComponent(0.08).setStroke()
    tilePath.lineWidth = 2 * s
    tilePath.stroke()

    // The mark, from the 128-unit design grid: screen 76×60 at (26,34), radius 14, stroke 7.
    let u = 824 * s / 128
    func r(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> NSRect {
        NSRect(x: tile.minX + x * u, y: tile.maxY - (y + h) * u, width: w * u, height: h * u)
    }
    let screen = r(26, 34, 76, 60)
    let outline = NSBezierPath(roundedRect: screen, xRadius: 14 * u, yRadius: 14 * u)
    NSColor.white.setStroke()
    outline.lineWidth = 7.5 * u
    outline.stroke()
    NSGraphicsContext.saveGraphicsState()
    NSBezierPath(rect: NSRect(x: screen.minX - 10 * u, y: screen.minY - 10 * u, width: screen.width / 2 + 10 * u, height: screen.height + 20 * u)).addClip()
    NSColor.white.setFill()
    outline.fill()
    outline.stroke()
    NSGraphicsContext.restoreGraphicsState()

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    try! draw(size).write(to: iconset.appendingPathComponent("icon_\(size)x\(size).png"))
    try! draw(size * 2).write(to: iconset.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
}
try! draw(1024).write(to: root.appendingPathComponent("Resources/AppIcon-1024.png"))
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", root.appendingPathComponent("Resources/AppIcon.icns").path]
try! task.run()
task.waitUntilExit()
print(task.terminationStatus == 0 ? "wrote Resources/AppIcon.icns" : "iconutil failed")
