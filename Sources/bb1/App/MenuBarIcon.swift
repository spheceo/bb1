import AppKit

/// The "Half" mark as a menu bar template image: a rounded screen, left half solid.
enum MenuBarIcon {
    static let image: NSImage = {
        let size = NSSize(width: 18, height: 18)
        let image = NSImage(size: size, flipped: false) { _ in
            let screen = NSRect(x: 1.75, y: 3.75, width: 14.5, height: 10.5)
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
}
