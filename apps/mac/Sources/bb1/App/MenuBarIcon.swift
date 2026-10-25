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

    /// One frame of "just updated": the full bar fades as a checkmark draws itself in the screen, then the
    /// check fades as the mark's solid half comes back. Each value is 0…1.
    static func updated(bar: Double, check: Double, checkOpacity: Double, half: Double) -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { _ in
            let outline = NSBezierPath(roundedRect: screen, xRadius: 3, yRadius: 3)
            outline.lineWidth = 1.5
            NSColor.black.setStroke()
            outline.stroke()
            if bar > 0 {
                NSColor.black.withAlphaComponent(bar).setFill()
                NSBezierPath(roundedRect: screen.insetBy(dx: 2.5, dy: 2.5), xRadius: 1, yRadius: 1).fill()
            }
            if check > 0 && checkOpacity > 0 {
                // The tick, drawn up to `check` of its length.
                let points = [NSPoint(x: 6.1, y: 9.1), NSPoint(x: 8.2, y: 7.0), NSPoint(x: 12.1, y: 10.9)]
                let lengths = zip(points, points.dropFirst()).map { hypot($1.x - $0.x, $1.y - $0.y) }
                var remaining = lengths.reduce(0, +) * min(check, 1)
                let tick = NSBezierPath()
                tick.move(to: points[0])
                for (i, length) in lengths.enumerated() where remaining > 0 {
                    let t = min(1, remaining / length)
                    let a = points[i], b = points[i + 1]
                    tick.line(to: NSPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t))
                    remaining -= length
                }
                tick.lineWidth = 1.6
                tick.lineCapStyle = .round
                tick.lineJoinStyle = .round
                NSColor.black.withAlphaComponent(checkOpacity).setStroke()
                tick.stroke()
            }
            if half > 0 {
                NSGraphicsContext.saveGraphicsState()
                NSBezierPath(rect: NSRect(x: 0, y: 0, width: screen.midX, height: 18)).addClip()
                NSColor.black.withAlphaComponent(half).setFill()
                outline.fill()
                NSGraphicsContext.restoreGraphicsState()
            }
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "bb1 updated"
        return image
    }
}
