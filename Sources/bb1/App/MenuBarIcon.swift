import AppKit

/// The "Half" mark as a menu bar template image: a rounded screen, left half solid.
enum MenuBarIcon {
    private static let screen = NSRect(x: 1.75, y: 3.75, width: 14.5, height: 10.5)

    static let image: NSImage = {
        let size = NSSize(width: 18, height: 18)
        let image = NSImage(size: size, flipped: false) { _ in
            let path = NSBezierPath(roundedRect: screen, xRadius: 3, yRadius: 3)
            path.lineWidth = 1.5
            NSColor.black.setStroke()
            path.stroke()
            NSGraphicsContext.saveGraphicsState()
            NSBezierPath(rect: NSRect(x: 0, y: 0, width: screen.midX, height: 18)).addClip()
            NSColor.black.setFill()
            path.fill()
            NSGraphicsContext.restoreGraphicsState()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "bb1"
        return image
    }()

    /// Updating: the screen fills up from the left like a progress bar, with a gap inside the outline
    /// so it never looks like the resting mark (whose left half is solid right up to the edge).
    static func progress(_ value: Double) -> NSImage {
        let p = min(max(value, 0), 1)
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { _ in
            let outline = NSBezierPath(roundedRect: screen, xRadius: 3, yRadius: 3)
            outline.lineWidth = 1.5
            NSColor.black.setStroke()
            outline.stroke()
            let inner = screen.insetBy(dx: 2.5, dy: 2.5)
            let bar = NSRect(x: inner.minX, y: inner.minY, width: max(1.5, inner.width * p), height: inner.height)
            NSColor.black.setFill()
            NSBezierPath(roundedRect: bar, xRadius: 1, yRadius: 1).fill()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "bb1 updating, \(Int(p * 100))%"
        return image
    }
}
