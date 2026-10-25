import AppKit
import BB1Core

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
    enum Kind { case primary, secondary, link, overlay }
    var kind: Kind { didSet { restyle() } }
    /// Compact size, for badges on cards.
    var small = false { didSet { restyle(); invalidateIntrinsicContentSize() } }
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
        if kind == .link { return s }
        return small ? NSSize(width: s.width + 18, height: 24) : NSSize(width: s.width + 28, height: 32)
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
        case .overlay:
            // On top of a screen picture: dark glass so it reads on any wallpaper.
            layer?.backgroundColor = NSColor.black.withAlphaComponent(hovering ? 0.75 : 0.6).cgColor
            layer?.borderWidth = 1
            layer?.borderColor = NSColor.white.withAlphaComponent(0.18).cgColor
            color = .white
        }
        attributedTitle = NSAttributedString(string: title, attributes: [
            .foregroundColor: color,
            .font: NSFont.systemFont(ofSize: kind == .link ? 13 : small ? 11.5 : 13.5, weight: .semibold),
        ])
    }
}

/// A flat rounded card that lightens on hover.
class CardView: NSView {
    private var area: NSTrackingArea?

    init(radius: CGFloat) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = radius
        layer?.backgroundColor = Style.cardFill.cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor
    }

    // `CardView()` lands here (NSObject's init calls init(frame:)). Must name the radius: a bare
    // `self.init()` would call back into this initializer forever.
    override convenience init(frame: NSRect) { self.init(radius: 12) }

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
    /// Given the width, how many columns and how big each item is (instead of a fixed `itemSize`).
    var adaptive: ((CGFloat) -> (columns: Int, size: NSSize))?
    override var isFlipped: Bool { true }

    func setItems(_ views: [NSView]) {
        // Same views in the same order: leave them be (keeps hover state and avoids flicker).
        guard views.count != subviews.count || zip(views, subviews).contains(where: { $0 !== $1 }) else { return }
        subviews.forEach { $0.removeFromSuperview() }
        views.forEach { addSubview($0) }
        needsLayout = true
        invalidateIntrinsicContentSize()
    }

    private var columns: Int {
        if let adaptive { return adaptive(bounds.width).columns }
        return max(1, Int((bounds.width + spacing) / (itemSize.width + spacing)))
    }

    override func layout() {
        super.layout()
        if let adaptive { itemSize = adaptive(bounds.width).size }
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

/// Drawing helpers for the bb1 mark.
enum ComputerArt {
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

/// A thin progress bar: white on a faint track.
final class ProgressLine: NSView {
    var value: Double = 0 { didSet { needsDisplay = true } }
    override var intrinsicContentSize: NSSize { NSSize(width: NSView.noIntrinsicMetric, height: 3) }

    override func draw(_ dirtyRect: NSRect) {
        let r = bounds.height / 2
        NSColor.white.withAlphaComponent(0.08).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: r, yRadius: r).fill()
        let fill = NSRect(x: 0, y: 0, width: max(bounds.height, bounds.width * CGFloat(min(max(value, 0), 1))), height: bounds.height)
        NSColor.white.withAlphaComponent(0.9).setFill()
        NSBezierPath(roundedRect: fill, xRadius: r, yRadius: r).fill()
    }
}

/// The pill in the "Your Macs" header: offers a newer build, then shows the update's progress inside itself.
final class UpdatePill: NSButton {
    enum Look: Equatable { case offer, busy(Double), done, failed }

    private var look: Look = .offer
    private var text = ""
    private var hovering = false
    private var area: NSTrackingArea?
    private let fill = CALayer()

    init(target: AnyObject, action: Selector) {
        super.init(frame: .zero)
        self.target = target
        self.action = action
        isBordered = false
        focusRingType = .none
        imagePosition = .imageLeading
        imageHugsTitle = true
        wantsLayer = true
        layer?.cornerRadius = 15
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        fill.backgroundColor = NSColor.white.withAlphaComponent(0.14).cgColor
        layer?.insertSublayer(fill, at: 0)
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(_ look: Look, _ text: String, tooltip: String? = nil) {
        isHidden = false
        self.look = look
        self.text = text
        toolTip = tooltip
        restyle()
        invalidateIntrinsicContentSize()
        needsLayout = true
    }

    override var intrinsicContentSize: NSSize {
        let s = super.intrinsicContentSize
        return NSSize(width: s.width + 26, height: 30)
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        if case .busy(let p) = look {
            fill.frame = NSRect(x: 0, y: 0, width: bounds.width * CGFloat(min(max(p, 0), 1)), height: bounds.height)
        } else {
            fill.frame = .zero
        }
        CATransaction.commit()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
    }

    override func mouseEntered(with event: NSEvent) { hovering = true; restyle() }
    override func mouseExited(with event: NSEvent) { hovering = false; restyle() }
    override func resetCursorRects() {
        if case .busy = look { return }
        addCursorRect(bounds, cursor: .pointingHand)
    }

    private func restyle() {
        let fg: NSColor
        let symbol: String?
        switch look {
        case .offer:
            layer?.backgroundColor = NSColor(white: hovering ? 0.84 : 0.95, alpha: 1).cgColor
            layer?.borderColor = NSColor.clear.cgColor
            fg = Style.ink
            symbol = "arrow.down.circle.fill"
        case .busy:
            layer?.backgroundColor = NSColor.white.withAlphaComponent(0.06).cgColor
            layer?.borderColor = NSColor.white.withAlphaComponent(0.1).cgColor
            fg = .white
            symbol = nil
        case .done:
            layer?.backgroundColor = NSColor.white.withAlphaComponent(hovering ? 0.1 : 0.06).cgColor
            layer?.borderColor = NSColor.white.withAlphaComponent(0.1).cgColor
            fg = NSColor.white.withAlphaComponent(0.85)
            symbol = "checkmark"
        case .failed:
            layer?.backgroundColor = NSColor.white.withAlphaComponent(hovering ? 0.1 : 0.06).cgColor
            layer?.borderColor = NSColor.white.withAlphaComponent(0.18).cgColor
            fg = .white
            symbol = "exclamationmark.circle"
        }
        image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 12.5, weight: .semibold)) }
        contentTintColor = fg
        attributedTitle = NSAttributedString(string: text, attributes: [
            .foregroundColor: fg, .font: NSFont.systemFont(ofSize: 12.5, weight: .semibold),
        ])
    }
}

extension String {
    /// "Update to …" → "update to …", for use mid-sentence.
    var lowercasedFirst: String { prefix(1).lowercased() + dropFirst() }
}

// MARK: - Your Macs

/// One of your other Macs: its screen (live, or the last one seen), its name and how it's doing.
/// Click anywhere to connect; hover for the button, the menu and the outline.
final class MacTile: NSView {
    struct Model {
        var name: String
        var status: String
        var live: Bool
        var image: CGImage?
        /// Offline: the picture goes grey and dark.
        var dim: Bool
        /// "Connect" / "Pair", or nil when there's nothing to do.
        var action: String?
        var progress: Double?
        /// "Update" (this Mac runs an older bb1).
        var badge: String?
        var tooltip: String?
    }

    static let footerHeight: CGFloat = 64
    var onClick: (() -> Void)?
    var onBadge: (() -> Void)?
    var makeMenu: (() -> NSMenu?)?

    private let screen = NSView()
    private let placeholder = NSImageView()
    private let name = Style.label("", size: 14.5, weight: .semibold)
    private let status = NSTextField(labelWithString: "")
    private let progress = ProgressLine()
    private lazy var actionButton = PillButton("Connect", kind: .primary, target: self, action: #selector(clicked))
    private lazy var badgeButton = PillButton("Update", kind: .overlay, target: self, action: #selector(badgeClicked))
    private lazy var menuButton = GlassCircleButton(symbol: "ellipsis", tooltip: "More") { [weak self] in self?.menuClicked() }
    private var model: Model?
    private var hovering = false { didSet { restyle() } }
    private var area: NSTrackingArea?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 16
        layer?.masksToBounds = true
        layer?.backgroundColor = Style.cardFill.cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor

        screen.wantsLayer = true
        screen.layer?.backgroundColor = Style.graphite(0.055).cgColor
        screen.layer?.contentsGravity = .resizeAspectFill
        screen.layer?.masksToBounds = true
        placeholder.image = MacTile.markImage
        placeholder.imageScaling = .scaleNone
        status.font = .systemFont(ofSize: 12)
        status.lineBreakMode = .byTruncatingTail
        status.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        badgeButton.small = true
        actionButton.small = true

        for v in [screen, placeholder, name, status, progress, actionButton, badgeButton, menuButton] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        NSLayoutConstraint.activate([
            screen.leadingAnchor.constraint(equalTo: leadingAnchor),
            screen.trailingAnchor.constraint(equalTo: trailingAnchor),
            screen.topAnchor.constraint(equalTo: topAnchor),
            screen.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -MacTile.footerHeight),
            placeholder.centerXAnchor.constraint(equalTo: screen.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: screen.centerYAnchor),
            progress.leadingAnchor.constraint(equalTo: leadingAnchor),
            progress.trailingAnchor.constraint(equalTo: trailingAnchor),
            progress.topAnchor.constraint(equalTo: screen.bottomAnchor),
            progress.heightAnchor.constraint(equalToConstant: 3),
            name.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 18),
            name.topAnchor.constraint(equalTo: screen.bottomAnchor, constant: 13),
            name.trailingAnchor.constraint(lessThanOrEqualTo: actionButton.leadingAnchor, constant: -10),
            status.leadingAnchor.constraint(equalTo: name.leadingAnchor),
            status.topAnchor.constraint(equalTo: name.bottomAnchor, constant: 3),
            status.trailingAnchor.constraint(lessThanOrEqualTo: actionButton.leadingAnchor, constant: -10),
            actionButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16),
            actionButton.centerYAnchor.constraint(equalTo: bottomAnchor, constant: -MacTile.footerHeight / 2),
            badgeButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            badgeButton.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            menuButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            menuButton.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            menuButton.widthAnchor.constraint(equalToConstant: GlassCircleButton.size),
            menuButton.heightAnchor.constraint(equalToConstant: GlassCircleButton.size),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(_ m: Model) {
        model = m
        name.stringValue = m.name
        let line = NSMutableAttributedString()
        if m.live {
            line.append(NSAttributedString(string: "● ", attributes: [.foregroundColor: Style.online, .font: NSFont.systemFont(ofSize: 9)]))
        }
        line.append(NSAttributedString(string: m.status, attributes: [
            .foregroundColor: NSColor.white.withAlphaComponent(m.live ? 0.6 : 0.4), .font: NSFont.systemFont(ofSize: 12)]))
        status.attributedStringValue = line
        let picture = m.dim ? m.image.map(dimmed) : m.image
        if screen.layer?.contents as! CGImage? !== picture { screen.layer?.contents = picture }
        placeholder.isHidden = m.image != nil
        progress.isHidden = m.progress == nil
        progress.value = m.progress ?? 0
        if let action = m.action, actionButton.title != action { actionButton.title = action }
        if let badge = m.badge, badgeButton.title != badge { badgeButton.title = badge }
        toolTip = m.tooltip
        restyle()
    }

    private static let imaging = CIContext()
    private var dimmedCache: (source: CGImage, result: CGImage)?

    /// The last picture of an offline Mac: grey and dark, made once per picture.
    private func dimmed(_ image: CGImage) -> CGImage {
        if let c = dimmedCache, c.source === image { return c.result }
        let grey = CIImage(cgImage: image)
            .applyingFilter("CIColorControls", parameters: [kCIInputSaturationKey: 0])
            .applyingFilter("CIColorMatrix", parameters: [
                "inputRVector": CIVector(x: 0.42, y: 0, z: 0, w: 0),
                "inputGVector": CIVector(x: 0, y: 0.42, z: 0, w: 0),
                "inputBVector": CIVector(x: 0, y: 0, z: 0.42, w: 0),
            ])
        let result = MacTile.imaging.createCGImage(grey, from: grey.extent) ?? image
        dimmedCache = (image, result)
        return result
    }

    private func restyle() {
        guard let m = model else { return }
        let canAct = m.action != nil
        layer?.borderColor = (hovering && canAct ? NSColor.white.withAlphaComponent(0.75) : Style.cardBorder).cgColor
        layer?.borderWidth = hovering && canAct ? 1.5 : 1
        layer?.backgroundColor = (hovering ? Style.cardHover : Style.cardFill).cgColor
        actionButton.isHidden = !(hovering && canAct)
        badgeButton.isHidden = m.badge == nil
        menuButton.isHidden = !hovering || makeMenu?() == nil
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
        // A tile that moved out from under the pointer gets no mouseExited: check where the pointer really is.
        if let window {
            let inside = bounds.contains(convert(window.mouseLocationOutsideOfEventStream, from: nil))
            if inside != hovering { hovering = inside }
        }
    }

    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
    override func resetCursorRects() { if model?.action != nil { addCursorRect(bounds, cursor: .pointingHand) } }
    override func mouseUp(with event: NSEvent) {
        if bounds.contains(convert(event.locationInWindow, from: nil)) { clicked() }
    }
    override func menu(for event: NSEvent) -> NSMenu? { makeMenu?() }

    @objc private func clicked() { if model?.action != nil { onClick?() } }
    @objc private func badgeClicked() { onBadge?() }
    private func menuClicked() {
        guard let menu = makeMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: menuButton.bounds.height + 4), in: menuButton)
    }

    static let markImage = NSImage(size: NSSize(width: 44, height: 35), flipped: false) { rect in
        ComputerArt.drawMark(in: rect.insetBy(dx: 1, dy: 1), opacity: 0.16)
        return true
    }
}

/// Your Macs with no other Macs yet: what to do next.
final class EmptyState: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        let mark = NSImageView(image: NSImage(size: NSSize(width: 64, height: 51), flipped: false) { rect in
            ComputerArt.drawMark(in: rect.insetBy(dx: 1, dy: 1), opacity: 0.22)
            return true
        })
        let title = Style.label("No other Macs yet", size: 15, weight: .semibold)
        let detail = Style.label("Turn on Share This Mac in bb1 on another Mac signed in to your Tailscale,\nor add one by its address with +.",
                                 size: 12.5, color: NSColor.white.withAlphaComponent(0.45))
        detail.alignment = .center
        detail.lineBreakMode = .byWordWrapping
        detail.maximumNumberOfLines = 0
        let stack = NSStackView(views: [mark, title, detail])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 8
        stack.setCustomSpacing(18, after: mark)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }
}

/// A round button floating on a picture (the ⋯ on a Mac): Liquid Glass on macOS 26, a blurred disc before.
/// A plain view, not an NSButton, so it stays exactly round.
final class GlassCircleButton: NSView {
    static let size: CGFloat = 28
    private let onClick: () -> Void
    private let icon = NSImageView()
    private var area: NSTrackingArea?
    private var hovering = false { didSet { icon.contentTintColor = .white.withAlphaComponent(hovering ? 1 : 0.85) } }

    init(symbol: String, tooltip: String, onClick: @escaping () -> Void) {
        self.onClick = onClick
        super.init(frame: NSRect(x: 0, y: 0, width: GlassCircleButton.size, height: GlassCircleButton.size))
        toolTip = tooltip
        icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tooltip)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .bold))
        icon.contentTintColor = .white.withAlphaComponent(0.85)
        let background: NSView
        if #available(macOS 26.0, *) {
            let glass = NSGlassEffectView()
            glass.cornerRadius = GlassCircleButton.size / 2
            glass.style = .clear
            glass.tintColor = NSColor.black.withAlphaComponent(0.22)
            background = glass
        } else {
            let blur = NSVisualEffectView()
            blur.material = .hudWindow
            blur.blendingMode = .withinWindow
            blur.state = .active
            blur.wantsLayer = true
            blur.layer?.cornerRadius = GlassCircleButton.size / 2
            blur.layer?.masksToBounds = true
            background = blur
        }
        for v in [background, icon] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        NSLayoutConstraint.activate([
            background.leadingAnchor.constraint(equalTo: leadingAnchor),
            background.trailingAnchor.constraint(equalTo: trailingAnchor),
            background.topAnchor.constraint(equalTo: topAnchor),
            background.bottomAnchor.constraint(equalTo: bottomAnchor),
            icon.centerXAnchor.constraint(equalTo: centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
    }

    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }
    override func mouseDown(with event: NSEvent) { onClick() }
    override func mouseUp(with event: NSEvent) {}
    override func resetCursorRects() { addCursorRect(bounds, cursor: .arrow) }
}

/// A round icon button (the gear in the header): just the symbol, with a soft circle behind it on hover.
/// Drawn by hand so it's exactly round (an NSButton can come out taller than its constraints).
final class IconButton: NSControl {
    private let symbol: NSImage?
    private var hovering = false { didSet { needsDisplay = true } }
    private var area: NSTrackingArea?
    static let size: CGFloat = 30

    init(symbol name: String, tooltip: String, target: AnyObject, action: Selector) {
        symbol = NSImage(systemSymbolName: name, accessibilityDescription: tooltip)?
            .withSymbolConfiguration(.init(pointSize: 14, weight: .medium))
        super.init(frame: NSRect(x: 0, y: 0, width: IconButton.size, height: IconButton.size))
        toolTip = tooltip
        self.target = target
        self.action = action
        translatesAutoresizingMaskIntoConstraints = false
        setContentHuggingPriority(.required, for: .horizontal)
        setContentHuggingPriority(.required, for: .vertical)
    }

    required init?(coder: NSCoder) { fatalError() }

    override var intrinsicContentSize: NSSize { NSSize(width: IconButton.size, height: IconButton.size) }

    override func draw(_ dirtyRect: NSRect) {
        if hovering {
            NSColor.white.withAlphaComponent(0.08).setFill()
            NSBezierPath(ovalIn: bounds).fill()
        }
        guard let symbol else { return }
        let tinted = NSImage(size: symbol.size, flipped: false) { rect in
            symbol.draw(in: rect)
            NSColor.white.withAlphaComponent(self.hovering ? 0.9 : 0.55).set()
            rect.fill(using: .sourceAtop)
            return true
        }
        let s = symbol.size
        tinted.draw(in: NSRect(x: round((bounds.width - s.width) / 2), y: round((bounds.height - s.height) / 2),
                               width: s.width, height: s.height))
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
    override func mouseDown(with event: NSEvent) {}
    override func mouseUp(with event: NSEvent) {
        if bounds.contains(convert(event.locationInWindow, from: nil)) { sendAction(action, to: target) }
    }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }
}

/// Lines things up with the window's close / minimise / zoom buttons.
enum TitleBar {
    /// Centre of the window buttons, in points from the top of `view`, and the x just after the zoom button.
    static func metrics(in view: NSView) -> (centerFromTop: CGFloat, afterButtons: CGFloat)? {
        guard let window = view.window, let close = window.standardWindowButton(.closeButton),
              let zoom = window.standardWindowButton(.zoomButton) else { return nil }
        let c = close.convert(close.bounds, to: view)
        let z = zoom.convert(zoom.bounds, to: view)
        let fromTop = view.isFlipped ? c.midY : view.bounds.maxY - c.midY
        return (fromTop, z.maxX)
    }
}

/// Your Tailscale account picture, round. The name is in the tooltip.
final class AvatarView: NSImageView {
    private var pictureURL: URL?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.masksToBounds = true
        imageScaling = .scaleProportionallyUpOrDown
        translatesAutoresizingMaskIntoConstraints = false
        widthAnchor.constraint(equalToConstant: 28).isActive = true
        heightAnchor.constraint(equalToConstant: 28).isActive = true
    }

    required init?(coder: NSCoder) { fatalError() }

    func set(name: String?, picture: URL?) {
        toolTip = name.map { "Signed in to Tailscale as \($0)" } ?? (Tailscale.isInstalled ? "Tailscale is off" : "Tailscale isn't installed")
        guard picture != pictureURL || image == nil else { return }
        pictureURL = picture
        image = NSImage(systemSymbolName: name == nil ? "network.slash" : "person.crop.circle.fill", accessibilityDescription: nil)
        contentTintColor = NSColor.white.withAlphaComponent(0.5)
        guard let picture else { return }
        URLSession.shared.dataTask(with: picture) { [weak self] data, _, _ in
            guard let data, let img = NSImage(data: data) else { return }
            DispatchQueue.main.async { if self?.pictureURL == picture { self?.image = img } }
        }.resume()
    }
}

// MARK: - Settings

/// A few options side by side; the chosen one is solid white (like the primary buttons).
final class SegmentedPicker: NSControl {
    let options: [String]
    var selected = 0 { didSet { needsDisplay = true } }
    private var hovered: Int?
    private var area: NSTrackingArea?
    private static let font = NSFont.systemFont(ofSize: 12, weight: .semibold)
    private static let pad: CGFloat = 12

    init(_ options: [String], selected: Int, target: AnyObject, action: Selector) {
        self.options = options
        self.selected = selected
        super.init(frame: .zero)
        self.target = target
        self.action = action
        translatesAutoresizingMaskIntoConstraints = false
    }

    required init?(coder: NSCoder) { fatalError() }

    private var widths: [CGFloat] {
        options.map { ceil(($0 as NSString).size(withAttributes: [.font: SegmentedPicker.font]).width) + 2 * SegmentedPicker.pad }
    }

    override var intrinsicContentSize: NSSize { NSSize(width: widths.reduce(0, +) + 4, height: 28) }

    private func rects() -> [NSRect] {
        var x: CGFloat = 2
        return widths.map { w in defer { x += w }; return NSRect(x: x, y: 2, width: w, height: bounds.height - 4) }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.white.withAlphaComponent(0.06).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: bounds.height / 2, yRadius: bounds.height / 2).fill()
        for (i, r) in rects().enumerated() {
            let on = i == selected
            if on || hovered == i {
                (on ? NSColor(white: 0.95, alpha: 1) : NSColor.white.withAlphaComponent(0.06)).setFill()
                NSBezierPath(roundedRect: r, xRadius: r.height / 2, yRadius: r.height / 2).fill()
            }
            let text = NSAttributedString(string: options[i], attributes: [
                .font: SegmentedPicker.font,
                .foregroundColor: on ? Style.ink : NSColor.white.withAlphaComponent(0.6),
            ])
            let s = text.size()
            text.draw(at: NSPoint(x: round(r.midX - s.width / 2), y: round(r.midY - s.height / 2)))
        }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(a)
        area = a
    }

    private func index(at event: NSEvent) -> Int? {
        let p = convert(event.locationInWindow, from: nil)
        return rects().firstIndex { $0.contains(p) }
    }

    override func mouseMoved(with event: NSEvent) { hovered = index(at: event); needsDisplay = true }
    override func mouseExited(with event: NSEvent) { hovered = nil; needsDisplay = true }
    override func mouseDown(with event: NSEvent) {}
    override func mouseUp(with event: NSEvent) {
        guard let i = index(at: event), i != selected else { return }
        selected = i
        sendAction(action, to: target)
    }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }
}

/// One card holding a section's rows, with hairlines between them (no hover: it's not clickable).
final class SettingsGroup: NSView {
    private let stack = NSStackView()

    init(rows: [NSView]) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.backgroundColor = Style.cardFill.cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        for (i, row) in rows.enumerated() {
            if i > 0 {
                let line = NSView()
                line.wantsLayer = true
                line.layer?.backgroundColor = Style.divider.cgColor
                let holder = NSView()
                line.translatesAutoresizingMaskIntoConstraints = false
                holder.addSubview(line)
                NSLayoutConstraint.activate([
                    holder.heightAnchor.constraint(equalToConstant: 1),
                    line.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 16),
                    line.trailingAnchor.constraint(equalTo: holder.trailingAnchor),
                    line.topAnchor.constraint(equalTo: holder.topAnchor),
                    line.bottomAnchor.constraint(equalTo: holder.bottomAnchor),
                ])
                stack.addArrangedSubview(holder)
                holder.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
            }
            stack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
    }

    required init?(coder: NSCoder) { fatalError() }
}
