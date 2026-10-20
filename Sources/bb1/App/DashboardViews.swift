import AppKit

/// Building blocks for the dashboard: flat, dark, rounded, no default bezels.
enum Style {
    static let cardFill = NSColor.white.withAlphaComponent(0.045)
    static let cardHover = NSColor.white.withAlphaComponent(0.075)
    static let cardBorder = NSColor.white.withAlphaComponent(0.08)
    static let divider = NSColor.white.withAlphaComponent(0.08)
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

/// A flat rounded button: `.primary` is filled with the accent colour, `.secondary` is an outline.
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
                ? NSColor.controlAccentColor.withAlphaComponent(hovering ? 0.85 : 1).cgColor
                : NSColor.white.withAlphaComponent(0.07).cgColor
            layer?.borderWidth = 0
            color = isEnabled ? .white : .tertiaryLabelColor
        case .secondary:
            layer?.backgroundColor = NSColor.white.withAlphaComponent(hovering && isEnabled ? 0.08 : 0).cgColor
            layer?.borderWidth = 1
            layer?.borderColor = NSColor.white.withAlphaComponent(isEnabled ? 0.35 : 0.12).cgColor
            color = isEnabled ? .labelColor : .tertiaryLabelColor
        case .link:
            layer?.backgroundColor = .clear
            layer?.borderWidth = 0
            color = hovering ? NSColor.controlAccentColor.blended(withFraction: 0.25, of: .white)! : .controlAccentColor
        }
        attributedTitle = NSAttributedString(string: title, attributes: [
            .foregroundColor: color,
            .font: NSFont.systemFont(ofSize: kind == .link ? 13 : 13.5, weight: kind == .link ? .semibold : .semibold),
        ])
    }
}

/// A rounded card that lightens on hover.
class CardView: NSView {
    private var area: NSTrackingArea?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.backgroundColor = Style.cardFill.cgColor
        layer?.borderWidth = 1
        layer?.borderColor = Style.cardBorder.cgColor
    }

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
        layer?.backgroundColor = selected ? NSColor.white.withAlphaComponent(0.1).cgColor : .clear
        contentTintColor = selected ? .controlAccentColor : .secondaryLabelColor
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
        layer?.backgroundColor = NSColor.white.withAlphaComponent(0.06).cgColor
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

/// The big picture on a computer card: a Mac outline with the bb1 mark on its screen.
enum ComputerArt {
    static func image(laptop: Bool, online: Bool) -> NSImage {
        let name = laptop ? "laptopcomputer" : "desktopcomputer"
        let config = NSImage.SymbolConfiguration(pointSize: 64, weight: .ultraLight)
            .applying(.init(paletteColors: [online ? NSColor(white: 0.86, alpha: 1) : NSColor.white.withAlphaComponent(0.22)]))
        return NSImage(systemSymbolName: name, accessibilityDescription: nil)?.withSymbolConfiguration(config) ?? NSImage()
    }
}
