import AppKit

/// Building blocks for the dashboard: flat, solid greys and white, like the app icon. No gradients or shadows.
enum Style {
    static func graphite(_ v: CGFloat) -> NSColor { NSColor(srgbRed: v, green: v * 1.03, blue: v * 1.12, alpha: 1) }
    static let background = graphite(0.035)
    static let cardFill = graphite(0.07)
    static let cardHover = graphite(0.09)
    static let cardBorder = NSColor.white.withAlphaComponent(0.06)
    static let divider = NSColor.white.withAlphaComponent(0.06)
    static let ink = graphite(0.07)
    static let online = NSColor(srgbRed: 0.18, green: 0.85, blue: 0.53, alpha: 1)
    static let offline = NSColor.white.withAlphaComponent(0.25)

    static func label(_ text: String, size: CGFloat, weight: NSFont.Weight = .regular, color: NSColor = .labelColor) -> NSTextField {
        let l = NSTextField(labelWithString: text)
        l.font = .systemFont(ofSize: size, weight: weight)
        l.textColor = color
        l.lineBreakMode = .byTruncatingTail
        return l
    }

    static func wrapping(_ text: String, size: CGFloat, color: NSColor = .secondaryLabelColor) -> NSTextField {
        let l = NSTextField(wrappingLabelWithString: text)
        l.font = .systemFont(ofSize: size)
        l.textColor = color
        return l
    }
}

/// A rounded button: `.primary` is solid white like the mark, `.secondary` is a faint outline.
final class PillButton: NSButton {
    enum Kind { case primary, secondary, link }
    var kind: Kind { didSet { restyle() } }
    private var hovering = false { didSet { restyle() } }
    private var area: NSTrackingArea?

    init(_ title: String, kind: Kind = .primary, target: AnyObject?, action: Selector?) {
        self.kind = kind
        super.init(frame: .zero)
        self.title = title
        self.target = target
        self.action = action
        isBordered = false
        wantsLayer = true
        layer?.cornerRadius = 8
        focusRingType = .none
        restyle()
    }

    required init?(coder: NSCoder) { fatalError() }

    override var title: String { didSet { restyle() } }
    override var isEnabled: Bool { didSet { restyle() } }

    override var intrinsicContentSize: NSSize {
        let s = super.intrinsicContentSize
        return kind == .link ? s : NSSize(width: s.width + 28, height: 32)
    }

    override func layout() {
        super.layout()
        if kind != .link { layer?.cornerRadius = bounds.height / 2 }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
    }

    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
    override func resetCursorRects() { if isEnabled { addCursorRect(bounds, cursor: .pointingHand) } }

    private func restyle() {
        let color: NSColor
        switch kind {
        case .primary:
            layer?.backgroundColor = isEnabled
                ? NSColor(white: hovering ? 0.84 : 0.95, alpha: 1).cgColor
                : NSColor.white.withAlphaComponent(0.06).cgColor
            layer?.borderWidth = isEnabled ? 0 : 1
            layer?.borderColor = NSColor.white.withAlphaComponent(0.06).cgColor
            color = isEnabled ? Style.ink : .tertiaryLabelColor
        case .secondary:
            layer?.backgroundColor = NSColor.white.withAlphaComponent(hovering && isEnabled ? 0.1 : 0.05).cgColor
            layer?.borderWidth = 1
            layer?.borderColor = NSColor.white.withAlphaComponent(isEnabled ? 0.16 : 0.08).cgColor
            color = isEnabled ? .labelColor : .tertiaryLabelColor
        case .link:
            layer?.backgroundColor = .clear
            layer?.borderWidth = 0
            color = NSColor.white.withAlphaComponent(hovering ? 0.7 : 0.95)
        }
        attributedTitle = NSAttributedString(string: title, attributes: [
            .foregroundColor: color,
            .font: NSFont.systemFont(ofSize: kind == .link ? 13 : 13.5, weight: kind == .link ? .semibold : .semibold),
        ])
    }
}

/// A flat rounded card that lightens on hover.
class CardView: NSView {
    private var area: NSTrackingArea?

    init(radius: CGFloat = 12) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = radius
        layer?.backgroundColor = Style.cardFill.cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor
    }

    override convenience init(frame: NSRect) { self.init() }

    required init?(coder: NSCoder) { fatalError() }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
    }

    override func mouseEntered(with event: NSEvent) { layer?.backgroundColor = Style.cardHover.cgColor }
    override func mouseExited(with event: NSEvent) { layer?.backgroundColor = Style.cardFill.cgColor }
}

/// An on/off switch in white and graphite (NSSwitch only comes in the system accent colour).
final class ToggleSwitch: NSControl {
    var isOn = false { didSet { needsDisplay = true } }
    override var intrinsicContentSize: NSSize { NSSize(width: 36, height: 20) }

    override func draw(_ dirtyRect: NSRect) {
        let track = NSRect(x: 0.5, y: (bounds.height - 20) / 2 + 0.5, width: 35, height: 19)
        let path = NSBezierPath(roundedRect: track, xRadius: 9.5, yRadius: 9.5)
        (isOn ? NSColor(white: 0.95, alpha: 1) : NSColor.white.withAlphaComponent(0.1)).setFill()
        path.fill()
        NSColor.white.withAlphaComponent(isOn ? 0 : 0.12).setStroke()
        path.stroke()
        let knob = NSRect(x: isOn ? track.maxX - 17 : track.minX + 2, y: track.minY + 2, width: 15, height: 15)
        (isOn ? Style.ink : NSColor(white: 0.85, alpha: 1)).setFill()
        NSBezierPath(ovalIn: knob).fill()
    }

    override func mouseDown(with event: NSEvent) {
        isOn.toggle()
        sendAction(action, to: target)
    }

    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }
}

/// Lays out fixed-size children left to right, wrapping into rows.
final class FlowView: NSView {
    var itemSize = NSSize(width: 228, height: 262)
    var spacing: CGFloat = 18
    override var isFlipped: Bool { true }

    func setItems(_ views: [NSView]) {
        subviews.forEach { $0.removeFromSuperview() }
        views.forEach { addSubview($0) }
        needsLayout = true
        invalidateIntrinsicContentSize()
    }

    private var columns: Int { max(1, Int((bounds.width + spacing) / (itemSize.width + spacing))) }

    override func layout() {
        super.layout()
        for (i, v) in subviews.enumerated() {
            let row = i / columns, col = i % columns
            v.frame = NSRect(x: CGFloat(col) * (itemSize.width + spacing), y: CGFloat(row) * (itemSize.height + spacing),
                             width: itemSize.width, height: itemSize.height)
        }
        invalidateIntrinsicContentSize()
    }

    override var intrinsicContentSize: NSSize {
        let rows = (subviews.count + columns - 1) / columns
        return NSSize(width: NSView.noIntrinsicMetric, height: max(itemSize.height, CGFloat(rows) * (itemSize.height + spacing) - spacing))
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        needsLayout = true
    }
}

/// One icon in the sidebar.
final class SidebarButton: NSButton {
    var selected = false { didSet { restyle() } }

    init(symbol: String, tooltip: String, target: AnyObject, action: Selector) {
        super.init(frame: .zero)
        isBordered = false
        wantsLayer = true
        layer?.cornerRadius = 12
        image = NSImage(systemSymbolName: symbol, accessibilityDescription: tooltip)?
            .withSymbolConfiguration(.init(pointSize: 19, weight: .medium))
        imagePosition = .imageOnly
        toolTip = tooltip
        self.target = target
        self.action = action
        translatesAutoresizingMaskIntoConstraints = false
        widthAnchor.constraint(equalToConstant: 46).isActive = true
        heightAnchor.constraint(equalToConstant: 46).isActive = true
        restyle()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func restyle() {
        layer?.backgroundColor = selected ? NSColor.white.withAlphaComponent(0.09).cgColor : .clear
        layer?.borderWidth = selected ? 1 : 0
        layer?.borderColor = NSColor.white.withAlphaComponent(0.07).cgColor
        contentTintColor = selected ? .white : NSColor.white.withAlphaComponent(0.4)
    }
}

/// A small rounded pill with a coloured dot and text (account / status).
final class StatusChip: NSView {
    private let dot = NSView()
    private let text = Style.label("", size: 12, weight: .medium, color: .secondaryLabelColor)

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 13
        layer?.backgroundColor = NSColor.white.withAlphaComponent(0.06).cgColor
        dot.wantsLayer = true
        dot.layer?.cornerRadius = 4
        let stack = NSStackView(views: [dot, text])
        stack.spacing = 7
        stack.edgeInsets = NSEdgeInsets(top: 5, left: 11, bottom: 5, right: 12)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            dot.widthAnchor.constraint(equalToConstant: 8), dot.heightAnchor.constraint(equalToConstant: 8),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor), stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor), stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func set(_ string: String, on: Bool) {
        text.stringValue = string
        dot.layer?.backgroundColor = (on ? Style.online : Style.offline).cgColor
    }
}

/// Your Tailscale account, top right: round picture and name.
final class AccountChip: NSView {
    private let avatar = NSImageView()
    private let name = Style.label("", size: 12.5, weight: .medium, color: .secondaryLabelColor)
    private var pictureURL: URL?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 15
        layer?.backgroundColor = NSColor.white.withAlphaComponent(0.05).cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor
        avatar.wantsLayer = true
        avatar.layer?.cornerRadius = 10
        avatar.layer?.masksToBounds = true
        avatar.imageScaling = .scaleProportionallyUpOrDown
        let stack = NSStackView(views: [avatar, name])
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 5, left: 5, bottom: 5, right: 13)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            avatar.widthAnchor.constraint(equalToConstant: 20), avatar.heightAnchor.constraint(equalToConstant: 20),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor), stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor), stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        toolTip = "Your Tailscale account"
    }

    required init?(coder: NSCoder) { fatalError() }

    func set(name text: String, picture: URL?, connected: Bool) {
        name.stringValue = text
        name.textColor = connected ? .labelColor : .secondaryLabelColor
        if picture != pictureURL {
            pictureURL = picture
            avatar.image = NSImage(systemSymbolName: "person.crop.circle.fill", accessibilityDescription: nil)
            avatar.contentTintColor = .secondaryLabelColor
            if let picture {
                URLSession.shared.dataTask(with: picture) { [weak self] data, _, _ in
                    guard let data, let image = NSImage(data: data) else { return }
                    DispatchQueue.main.async { self?.avatar.image = image }
                }.resume()
            }
        }
        if picture == nil {
            avatar.image = NSImage(systemSymbolName: connected ? "person.crop.circle.fill" : "network.slash", accessibilityDescription: nil)
            avatar.contentTintColor = .secondaryLabelColor
        }
    }
}

/// The big picture on a computer card: a flat Mac with the bb1 mark on its screen.
/// Offline or unshared Macs are drawn dim.
enum ComputerArt {
    static let size = NSSize(width: 128, height: 88)

    static func image(laptop: Bool, online: Bool) -> NSImage {
        NSImage(size: size, flipped: false) { _ in
            let body = NSColor.white.withAlphaComponent(online ? 0.22 : 0.1)
            let screen = laptop ? NSRect(x: 18, y: 14, width: 92, height: 64) : NSRect(x: 12, y: 26, width: 104, height: 60)
            body.setFill()
            if laptop {
                NSBezierPath(roundedRect: NSRect(x: 4, y: 6, width: 120, height: 8), xRadius: 4, yRadius: 4).fill()
            } else {
                NSBezierPath(rect: NSRect(x: 56, y: 10, width: 16, height: 16)).fill()
                NSBezierPath(roundedRect: NSRect(x: 40, y: 6, width: 48, height: 5), xRadius: 2.5, yRadius: 2.5).fill()
            }
            let panel = NSBezierPath(roundedRect: screen.insetBy(dx: 0.75, dy: 0.75), xRadius: 8, yRadius: 8)
            Style.background.setFill()
            panel.fill()
            body.setStroke()
            panel.lineWidth = 1.5
            panel.stroke()

            let w: CGFloat = 34, h = w * 60 / 76
            drawMark(in: NSRect(x: screen.midX - w / 2, y: screen.midY - h / 2, width: w, height: h), opacity: online ? 1 : 0.2)
            return true
        }
    }

    /// The "Half" mark: a rounded screen outline with its left half solid (same proportions as the icon).
    static func drawMark(in rect: NSRect, opacity: CGFloat) {
        // One transparency layer, so the fill and outline don't double up where they overlap.
        let cg = NSGraphicsContext.current!.cgContext
        cg.saveGState()
        cg.setAlpha(opacity)
        cg.beginTransparencyLayer(auxiliaryInfo: nil)
        defer { cg.endTransparencyLayer(); cg.restoreGState() }
        let line = rect.width * 7.5 / 76
        let r = rect.insetBy(dx: line / 2, dy: line / 2)
        let path = NSBezierPath(roundedRect: r, xRadius: rect.width * 14 / 76, yRadius: rect.width * 14 / 76)
        path.lineWidth = line
        NSColor.white.setStroke()
        NSColor.white.setFill()
        path.stroke()
        NSGraphicsContext.saveGraphicsState()
        NSBezierPath(rect: NSRect(x: rect.minX - line, y: rect.minY - line, width: rect.width / 2 + line, height: rect.height + 2 * line)).addClip()
        path.fill()
        path.stroke()
        NSGraphicsContext.restoreGraphicsState()
    }
}
