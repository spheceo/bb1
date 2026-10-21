import AppKit
import AVFoundation
import BB1Core

/// A line in the overlay card (top left of the stream).
enum OverlayLine {
    case notice(String)
    case warning(String)
    case stat(String)
    case hint(String)
}

/// A dark, padded card with text, drawn over the stream so it reads on any picture.
/// One layout manager both measures and draws the text, so the card always fits it exactly.
final class OverlayCard: CALayer {
    private let storage = NSTextStorage()
    private let layout = NSLayoutManager()
    private let container = NSTextContainer()
    static let padding = CGSize(width: 14, height: 10)

    override init() {
        super.init()
        backgroundColor = NSColor(srgbRed: 0.075, green: 0.078, blue: 0.086, alpha: 0.94).cgColor
        borderColor = NSColor.white.withAlphaComponent(0.1).cgColor
        borderWidth = 1
        cornerRadius = 10
        shadowColor = NSColor.black.cgColor
        shadowOpacity = 0.45
        shadowRadius = 14
        shadowOffset = CGSize(width: 0, height: -4)
        needsDisplayOnBoundsChange = true
        isHidden = true
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
    }

    override init(layer: Any) { super.init(layer: layer) }
    required init?(coder: NSCoder) { fatalError() }

    /// Shows the text (or hides the card) and returns the card's size for at most `maxWidth`.
    @discardableResult
    func set(_ string: NSAttributedString?, maxWidth: CGFloat) -> CGSize {
        guard let string, string.length > 0 else { isHidden = true; return .zero }
        container.size = CGSize(width: max(40, maxWidth - 2 * OverlayCard.padding.width), height: .greatestFiniteMagnitude)
        storage.setAttributedString(string)
        layout.ensureLayout(for: container)
        let used = layout.usedRect(for: container)
        isHidden = false
        setNeedsDisplay()
        return CGSize(width: ceil(used.width) + 2 * OverlayCard.padding.width,
                      height: ceil(used.height) + 2 * OverlayCard.padding.height)
    }

    override func draw(in ctx: CGContext) {
        guard storage.length > 0 else { return }
        ctx.saveGState()
        // The text system draws top-down.
        ctx.translateBy(x: 0, y: bounds.height)
        ctx.scaleBy(x: 1, y: -1)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: true)
        layout.drawGlyphs(forGlyphRange: layout.glyphRange(for: container),
                          at: CGPoint(x: OverlayCard.padding.width, y: OverlayCard.padding.height))
        NSGraphicsContext.restoreGraphicsState()
        ctx.restoreGState()
    }
}

/// The remote screen plus mouse input. Keyboard input is handled by `KeyboardCapture`.
final class VideoView: NSView {
    let displayLayer = AVSampleBufferDisplayLayer()
    private let overlay = OverlayCard()
    private let banner = OverlayCard()
    private var overlayText: NSAttributedString?
    private var bannerText: NSAttributedString?
    private let blankCursor = NSCursor(image: NSImage(size: NSSize(width: 1, height: 1)), hotSpot: .zero)

    var streamSize = CGSize(width: 16, height: 9) { didSet { relayout() } }
    var hideLocalCursor = true {
        didSet { if hideLocalCursor != oldValue { window?.invalidateCursorRects(for: self); updateCursor() } }
    }
    /// The host's current cursor shape, when this Mac's cursor is used over the stream.
    var remoteCursor: NSCursor? {
        didSet { window?.invalidateCursorRects(for: self); updateCursor() }
    }
    private var shownCursor: NSCursor { hideLocalCursor ? blankCursor : (remoteCursor ?? .arrow) }
    var invertScroll = false
    var scale = ScaleMode.fit { didSet { relayout() } }
    var inputEnabled: () -> Bool = { false }
    /// Called when the user clicks the picture while bb1 isn't taking input (to take control again).
    var onClickToControl: (() -> Void)?
    /// The pointer moved onto (true) or off (false) the picture.
    var onPointerOverPicture: ((Bool) -> Void)?
    var onInput: ((ControlMessage) -> Void)?
    /// Mouse buttons held down that started on the picture (a drag keeps control until it ends).
    private var buttonsHeld = 0
    private var swallowMouseUp = false
    private var lastPointerOver = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        layer = CALayer()
        wantsLayer = true
        layer!.backgroundColor = NSColor.black.cgColor
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = NSColor.black.cgColor
        layer!.addSublayer(displayLayer)
        layer!.addSublayer(overlay)
        layer!.addSublayer(banner)
    }

    required init?(coder: NSCoder) { fatalError() }

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override var isOpaque: Bool { true }
    /// Keys typed while this Mac has control (pointer off the picture): no beep. Menu shortcuts still work.
    override func keyDown(with event: NSEvent) {}

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        let scale = window?.backingScaleFactor ?? 2
        overlay.contentsScale = scale
        banner.contentsScale = scale
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        relayout()
    }

    private func relayout() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        displayLayer.frame = videoRect
        displayLayer.videoGravity = .resize
        let size = overlay.set(overlayText, maxWidth: min(bounds.width - 32, 600))
        overlay.frame = CGRect(x: 16, y: bounds.height - 16 - size.height, width: size.width, height: size.height)
        let b = banner.set(bannerText, maxWidth: min(bounds.width - 40, 440))
        banner.frame = CGRect(x: ((bounds.width - b.width) / 2).rounded(), y: ((bounds.height - b.height) / 2).rounded(),
                              width: b.width, height: b.height)
        CATransaction.commit()
        window?.invalidateCursorRects(for: self)
    }

    func setOverlay(_ lines: [OverlayLine]?) {
        overlayText = lines.flatMap { $0.isEmpty ? nil : VideoView.format($0) }
        relayout()
    }

    func setBanner(_ text: String?) {
        bannerText = text.map {
            let style = NSMutableParagraphStyle()
            style.alignment = .center
            return NSAttributedString(string: $0, attributes: [.font: NSFont.systemFont(ofSize: 14, weight: .medium),
                                                                .foregroundColor: NSColor.white, .paragraphStyle: style])
        }
        relayout()
    }

    private static func format(_ lines: [OverlayLine]) -> NSAttributedString {
        let out = NSMutableAttributedString()
        let spaced = NSMutableParagraphStyle()
        spaced.paragraphSpacing = 4
        spaced.lineSpacing = 1
        for (i, line) in lines.enumerated() {
            let font: NSFont, color: NSColor, text: String
            switch line {
            case .notice(let t): text = t; font = .systemFont(ofSize: 13, weight: .medium); color = .white
            case .warning(let t): text = t; font = .systemFont(ofSize: 13, weight: .semibold)
                color = NSColor(srgbRed: 1, green: 0.8, blue: 0.4, alpha: 1)
            case .stat(let t):
                // Wrap between items ("60 fps · RTT 7 ms"), never inside one.
                text = t.components(separatedBy: " · ").map { $0.replacingOccurrences(of: " ", with: "\u{00A0}") }.joined(separator: " · ")
                font = .monospacedSystemFont(ofSize: 11.5, weight: .regular); color = NSColor.white.withAlphaComponent(0.88)
            case .hint(let t): text = t; font = .systemFont(ofSize: 11.5, weight: .regular); color = NSColor.white.withAlphaComponent(0.55)
            }
            out.append(NSAttributedString(string: (i == 0 ? "" : "\n") + text,
                                          attributes: [.font: font, .foregroundColor: color, .paragraphStyle: spaced]))
        }
        return out
    }

    // MARK: Cursor

    override func resetCursorRects() {
        addCursorRect(videoRect, cursor: shownCursor)
    }

    private func updateCursor() {
        guard let window, window.isKeyWindow else { return }
        if pointerOverPicture { shownCursor.set() }
    }

    /// The pointer is on the picture (not the black bars, the title bar or outside the window).
    var pointerOverPicture: Bool {
        guard let window else { return false }
        return videoRect.contains(convert(window.mouseLocationOutsideOfEventStream, from: nil))
    }

    /// Input goes to the host while the pointer is on the picture, or while a drag that started there lasts.
    var controlsHost: Bool { buttonsHeld > 0 || pointerOverPicture }

    private func pointerMoved() {
        let over = pointerOverPicture
        guard over != lastPointerOver else { return }
        lastPointerOver = over
        // Mid-drag the host keeps the pointer; the change is reported when the button comes up.
        if buttonsHeld == 0 { onPointerOverPicture?(over) }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect, .cursorUpdate],
                                       owner: self, userInfo: nil))
    }

    override func cursorUpdate(with event: NSEvent) {
        if pointerOverPicture { shownCursor.set() } else { NSCursor.arrow.set() }
    }

    override func mouseEntered(with event: NSEvent) { pointerMoved() }
    override func mouseExited(with event: NSEvent) { pointerMoved() }

    // MARK: Mouse

    /// Where the picture is drawn inside the view, for the current scale mode.
    var videoRect: CGRect {
        switch scale {
        case .fit:
            return AVMakeRect(aspectRatio: streamSize, insideRect: bounds)
        case .stretch:
            return bounds
        case .actual:
            let s = window?.backingScaleFactor ?? 2
            let size = CGSize(width: streamSize.width / s, height: streamSize.height / s)
            return CGRect(x: ((bounds.width - size.width) / 2).rounded(), y: ((bounds.height - size.height) / 2).rounded(),
                          width: size.width, height: size.height)
        }
    }

    private func position(_ event: NSEvent) -> (Double, Double) {
        let p = convert(event.locationInWindow, from: nil)
        let rect = videoRect
        guard rect.width > 0, rect.height > 0 else { return (0, 0) }
        let x = (p.x - rect.minX) / rect.width
        let y = 1 - (p.y - rect.minY) / rect.height
        return (min(max(x, 0), 1), min(max(y, 0), 1))
    }

    private func sendMove(_ event: NSEvent) {
        guard inputEnabled() else { return }
        let (x, y) = position(event)
        onInput?(.mouseMove(MouseMoveMsg(x: x, y: y)))
    }

    private func sendButton(_ button: Int, _ down: Bool, _ event: NSEvent) {
        guard inputEnabled() else { return }
        let (x, y) = position(event)
        onInput?(.mouseButton(MouseButtonMsg(button: button, down: down, clicks: event.clickCount, x: x, y: y)))
    }

    override func mouseMoved(with event: NSEvent) {
        pointerMoved()
        if pointerOverPicture { sendMove(event) }
    }
    override func mouseDragged(with event: NSEvent) { sendMove(event) }
    override func rightMouseDragged(with event: NSEvent) { sendMove(event) }
    override func otherMouseDragged(with event: NSEvent) { sendMove(event) }

    /// A press on the picture. Off the picture (the black bars) it does nothing: that's this Mac's side.
    private func press(_ button: Int, _ event: NSEvent) {
        guard pointerOverPicture else { return }
        if !inputEnabled() {
            // This click only takes control back; it isn't passed on to the host.
            swallowMouseUp = true
            onClickToControl?()
            return
        }
        buttonsHeld += 1
        sendButton(button, true, event)
    }

    private func release(_ button: Int, _ event: NSEvent) {
        if swallowMouseUp { swallowMouseUp = false; return }
        guard buttonsHeld > 0 else { return }
        buttonsHeld -= 1
        sendButton(button, false, event)
        if buttonsHeld == 0 {
            // The drag ended off the picture: hand control back now.
            lastPointerOver = pointerOverPicture
            if !lastPointerOver { onPointerOverPicture?(false) }
        }
    }

    override func mouseDown(with event: NSEvent) { press(0, event) }
    override func mouseUp(with event: NSEvent) { release(0, event) }
    override func rightMouseDown(with event: NSEvent) { press(1, event) }
    override func rightMouseUp(with event: NSEvent) { release(1, event) }
    override func otherMouseDown(with event: NSEvent) { press(event.buttonNumber, event) }
    override func otherMouseUp(with event: NSEvent) { release(event.buttonNumber, event) }

    /// Re-read where the pointer is (it isn't tracked while the window isn't focused).
    func syncPointer() { if buttonsHeld == 0 { lastPointerOver = pointerOverPicture } }

    /// Control left this view (window lost focus, released): forget held buttons.
    func resetButtons() {
        buttonsHeld = 0
        swallowMouseUp = false
    }

    override func scrollWheel(with event: NSEvent) {
        guard inputEnabled(), pointerOverPicture else { return }
        let sign = invertScroll ? -1.0 : 1.0
        onInput?(.scroll(ScrollMsg(dx: event.scrollingDeltaX * sign, dy: event.scrollingDeltaY * sign,
                                   precise: event.hasPreciseScrollingDeltas,
                                   phase: event.phase.rawValue, momentum: event.momentumPhase.rawValue)))
    }
}

/// One stream window and its connection, plus the stream options (menus and shortcuts).
///
/// Control follows the pointer: on the host's picture, keyboard and mouse go to the host; off it (the
/// title bar, the black bars, outside the window) they stay on this Mac, where the normal shortcuts work.
/// ⌘⎋ hands control back without moving the pointer (the way out of full screen).
final class StreamWindowController: NSObject, NSWindowDelegate {
    let window: NSWindow
    let view: VideoView
    let connection: ClientConnection
    private let target: ClientTarget
    private let renderer: FrameRenderer
    private var keyboard: KeyboardCapture!
    private var settings = ClientSettings.load()
    private var connected = false
    private var showStats = ProcessInfo.processInfo.environment["BB1_SHOW_STATS"] != nil // tests
    private var lastStats: ClientStats?
    private var notice: String?
    private var config: StreamConfig?
    private var windowedFrame: NSRect?
    private var wasCapturing = false
    /// ⌘⎋ gave control back to this Mac; it returns when the pointer comes back onto the picture or you click it.
    private var handedBack = false
    var onClosed: ((String?) -> Void)?
    var onPairedHost: (() -> Void)?
    /// Status text, connected, latest stats (for the menu bar).
    var onUpdate: ((String, Bool, ClientStats?) -> Void)?
    private var endReason: String?
    private var observers: [NSObjectProtocol] = []

    init(target: ClientTarget) {
        self.target = target
        view = VideoView(frame: NSRect(x: 0, y: 0, width: 1280, height: 720))
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 720),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        connection = ClientConnection(target: target)
        renderer = FrameRenderer(layer: view.displayLayer)
        super.init()

        view.invertScroll = settings.invertScroll
        view.scale = settings.scale
        window.contentView = view
        window.delegate = self
        window.acceptsMouseMovedEvents = true
        window.collectionBehavior = [.fullScreenPrimary]
        window.isReleasedWhenClosed = false
        window.backgroundColor = .black
        window.center()
        window.setFrameAutosaveName("bb1.stream")

        let renderer = self.renderer, connection = self.connection
        // Weak: the decoder belongs to the connection, so a strong capture would keep both alive forever.
        connection.decoder.onFrame = { [weak connection] pixelBuffer, captureUs in
            renderer.enqueue(pixelBuffer)
            connection?.markDisplayed(captureUs: captureUs)
        }
        connection.onDisplayStall = { renderer.reset() }
        connection.onStatus = { [weak self] text, connected in
            guard let self else { return }
            self.connected = connected
            self.onUpdate?(text, connected, nil)
            self.view.setBanner(connected ? nil : text)
            if !connected { self.keyboard.reset() }
            if connected {
                self.connection.setSound(self.settings.sound)
                // Tell the host every time we connect, not only when the option changes.
                self.connection.send(.setCursorInVideo(self.settings.cursorInVideo))
            }
            self.updateCaptureState()
        }
        connection.onConfig = { [weak self] c in self?.apply(c) }
        connection.askCode = { [weak self] host, reply in
            guard let self else { reply(nil); return }
            self.askForCode(host, reply)
        }
        connection.onCursor = { [weak self] c in
            guard let image = NSImage(data: c.png) else { return }
            image.size = NSSize(width: c.width, height: c.height)
            self?.view.remoteCursor = NSCursor(image: image, hotSpot: NSPoint(x: c.hotX, y: c.hotY))
        }
        connection.onPaired = { [weak self] known in
            self?.flash("Paired with \(known.name). It'll connect on its own from now on.")
            self?.onPairedHost?()
        }
        connection.onNotice = { [weak self] text in
            // "The host can't…" stays until it's fixed; anything else is just news.
            if text.hasPrefix("The host") {
                self?.notice = text
                self?.refreshOverlay()
            } else {
                self?.flash(text, for: 10)
            }
        }
        connection.onStats = { [weak self] s in
            guard let self else { return }
            self.lastStats = s
            self.refreshOverlay()
            if self.connected { self.onUpdate?("Connected to \(target.label)", true, s) }
        }
        connection.onEnded = { [weak self] reason in
            self?.endReason = reason
            self?.window.close()
        }

        view.inputEnabled = { [weak self] in (self?.isCapturing ?? false) && (self?.settings.input.mouse ?? false) }
        view.onInput = { connection.send($0) }
        view.onClickToControl = { [weak self] in
            guard let self else { return }
            self.handedBack = false
            self.window.makeKey()
            NSApp.activate(ignoringOtherApps: true)
            self.updateCaptureState()
        }
        view.onPointerOverPicture = { [weak self] over in
            guard let self else { return }
            if over { self.handedBack = false }
            self.updateCaptureState()
        }
        keyboard = KeyboardCapture(
            isActive: { [weak self] in self?.isCapturing ?? false },
            onKey: { connection.send(.key($0)) },
            onGiveBack: { [weak self] in self?.releaseInput() })
        keyboard.forwardKeys = { [weak self] in self?.settings.input.keyboard ?? false }
        keyboard.onCapsLock = { connection.send(.capsLock($0)) }
        connection.setSound(settings.sound)

        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.updateCaptureState()
            })
        }
        updateTitle()
    }

    /// Connected, bb1 in front with this window focused, and the pointer on the picture: input goes to the host.
    private var isCapturing: Bool { connected && NSApp.isActive && window.isKeyWindow && !handedBack && view.controlsHost }

    func show() {
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        view.setBanner("Connecting…")
        keyboard.start()
        connection.start()
    }

    private func apply(_ c: StreamConfig) {
        let first = config == nil
        config = c
        view.streamSize = CGSize(width: c.width, height: c.height)
        if first, window.styleMask.contains(.fullScreen) == false, let screen = window.screen ?? NSScreen.main {
            // Start at 1 host pixel per point if it fits, else as large as fits.
            let visible = screen.visibleFrame.size
            var size = NSSize(width: c.width, height: c.height)
            let scale = min(1, visible.width * 0.9 / size.width, (visible.height - 30) * 0.9 / size.height)
            size = NSSize(width: (size.width * scale).rounded(), height: (size.height * scale).rounded())
            window.setContentSize(size)
            window.center()
        }
    }

    /// First-time pairing: the host shows a 6-digit code; type it here.
    private func askForCode(_ host: String, _ reply: @escaping (String?) -> Void) {
        view.setBanner("Pairing with \(host)…")
        let alert = NSAlert()
        alert.messageText = "Pair with \(host)"
        alert.informativeText = "A 6-digit code is now showing on \(host)'s screen. Type it here. You only do this once."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 160, height: 28))
        field.font = .monospacedDigitSystemFont(ofSize: 18, weight: .medium)
        field.alignment = .center
        field.placeholderString = "000000"
        alert.accessoryView = field
        alert.addButton(withTitle: "Pair")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        alert.beginSheetModal(for: window) { response in
            reply(response == .alertFirstButtonReturn ? field.stringValue : nil)
        }
    }

    // MARK: Input state

    private func updateCaptureState() {
        view.syncPointer()
        let now = isCapturing
        if now != wasCapturing {
            wasCapturing = now
            if now {
                Log.info("client: input → host (\(settings.input.title))")
                // Caps Lock may have changed while bb1 wasn't in front.
                connection.send(.capsLock(NSEvent.modifierFlags.contains(.capsLock)))
            } else {
                let why = !connected ? "not connected" : !NSApp.isActive ? "bb1 is in the background"
                    : !window.isKeyWindow ? "window not focused" : handedBack ? "⌘⎋" : "pointer left the picture"
                Log.info("client: input released (\(why))")
                connection.send(.releaseAll)
                keyboard.reset()
                view.resetButtons()
            }
        }
        updateTitle()
    }

    private func updateTitle() {
        window.title = "bb1 — \(target.label)"
        // Hide this Mac's cursor over the picture only while it's actually driving the host's cursor.
        view.hideLocalCursor = settings.cursorInVideo && settings.input.mouse && isCapturing
    }

    // MARK: Options

    private func save() { settings.save() }

    @objc func toggleFullscreen() { window.toggleFullScreen(nil) }

    /// Window covers the whole screen (still a window); again to go back.
    @objc func toggleFillScreen() {
        guard !window.styleMask.contains(.fullScreen), let screen = window.screen ?? NSScreen.main else { return }
        if let frame = windowedFrame {
            window.setFrame(frame, display: true, animate: true)
            windowedFrame = nil
        } else {
            windowedFrame = window.frame
            window.setFrame(screen.visibleFrame, display: true, animate: true)
        }
    }

    @objc func setScale(_ sender: NSMenuItem) {
        guard let mode = ScaleMode(rawValue: sender.representedObject as? String ?? "") else { return }
        settings.scale = mode
        view.scale = mode
        save()
    }

    @objc func setInput(_ sender: NSMenuItem) {
        guard let mode = InputMode(rawValue: sender.representedObject as? String ?? "") else { return }
        settings.input = mode
        save()
        connection.send(.releaseAll)
        keyboard.reset()
        Log.info("client: input mode \(mode.title)")
        flash("Input: \(mode.title)")
        updateTitle()
    }

    /// ⌘⎋: give the keyboard and mouse back to this Mac without moving the pointer. Moving back onto the
    /// picture (or clicking it) gives control to the host again.
    @objc func releaseInput() {
        guard !handedBack, isCapturing else { return }
        handedBack = true
        updateCaptureState()
        flash("This Mac has control. Click the picture to go back.")
    }

    @objc func toggleSound() {
        settings.sound.toggle()
        save()
        connection.setSound(settings.sound)
        flash(settings.sound ? "Sound on" : "Sound muted")
    }

    @objc func toggleStats() {
        showStats.toggle()
        refreshOverlay()
    }

    @objc func toggleCursor() {
        // The host's cursor drawn in the video vs this Mac's own cursor.
        settings.cursorInVideo.toggle()
        save()
        connection.send(.setCursorInVideo(settings.cursorInVideo))
        updateTitle()
    }

    @objc func toggleScroll() {
        view.invertScroll.toggle()
        settings.invertScroll = view.invertScroll
        save()
        flash("Scroll direction \(view.invertScroll ? "flipped" : "normal")")
    }

    @objc func disconnect() { window.close() }

    private func flash(_ text: String, for seconds: Double = 2) {
        notice = text
        refreshOverlay()
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [weak self] in
            guard let self, self.notice == text else { return }
            self.notice = nil
            self.refreshOverlay()
        }
    }

    /// The stream options, for the app's menu bar and the bb1 icon's menu.
    func menuItems() -> [NSMenuItem] {
        var items: [NSMenuItem] = []
        func item(_ title: String, _ action: Selector, key: String = "", mods: NSEvent.ModifierFlags = .command,
                  on: Bool = false, enabled: Bool = true) -> NSMenuItem {
            let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
            if !key.isEmpty { i.keyEquivalentModifierMask = mods }
            i.target = self
            i.state = on ? .on : .off
            i.isEnabled = enabled
            return i
        }
        func sub(_ title: String, _ children: [NSMenuItem]) -> NSMenuItem {
            let i = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let m = NSMenu()
            m.autoenablesItems = false
            children.forEach(m.addItem)
            i.submenu = m
            return i
        }
        let fullscreen = window.styleMask.contains(.fullScreen)
        items.append(item("Full Screen", #selector(toggleFullscreen), key: "f", mods: [.control, .command], on: fullscreen))
        items.append(item("Fill Screen (Windowed)", #selector(toggleFillScreen), on: windowedFrame != nil, enabled: !fullscreen))
        items.append(sub("Scaling", ScaleMode.allCases.map { mode in
            let i = item(mode.title, #selector(setScale(_:)), on: settings.scale == mode)
            i.representedObject = mode.rawValue
            return i
        }))
        items.append(.separator())
        items.append(sub("Input", InputMode.allCases.map { mode in
            let i = item(mode.title, #selector(setInput(_:)), on: settings.input == mode)
            i.representedObject = mode.rawValue
            return i
        }))
        items.append(item("Give Control Back to This Mac", #selector(releaseInput), key: "\u{1b}", enabled: isCapturing))
        items.append(item("Flip Scroll Direction", #selector(toggleScroll), on: settings.invertScroll))
        items.append(item("Use This Mac's Cursor (instant)", #selector(toggleCursor), on: !settings.cursorInVideo))
        items.append(.separator())
        let soundAvailable = connection.hostHasSound
        items.append(item(soundAvailable ? "Mute Sound" : "Mute Sound (host needs updating)", #selector(toggleSound),
                          on: !settings.sound, enabled: soundAvailable))
        items.append(item("Show Stats", #selector(toggleStats), key: "i", on: showStats))
        items.append(.separator())
        items.append(item("Disconnect", #selector(disconnect)))
        return items
    }

    private func refreshOverlay() {
        guard showStats || notice != nil else { view.setOverlay(nil); return }
        var lines: [OverlayLine] = []
        if let notice { lines.append(notice.hasPrefix("The host") ? .warning(notice) : .notice(notice)) }
        if showStats, let s = lastStats {
            lines.append(.stat(s.summary))
            lines.append(.stat("system shortcuts \(keyboard.hasSystemShortcuts ? "go to the host" : "stay here (allow Accessibility)") · input \(settings.input.title.lowercased()) · sound \(settings.sound ? "on" : "muted")"))
            lines.append(.hint("Off the picture, this Mac has control. ⌘⎋ gives it back · ⌃⌘F full screen · ⌘I stats"))
        }
        view.setOverlay(lines)
    }

    func windowDidBecomeKey(_ notification: Notification) { updateCaptureState() }

    func windowDidResignKey(_ notification: Notification) { updateCaptureState() }

    func windowWillClose(_ notification: Notification) {
        observers.forEach(NotificationCenter.default.removeObserver)
        observers.removeAll()
        keyboard.stop()
        connection.stop()
        // AppKit keeps the last key window alive until another becomes key. Let go of the view now, so
        // the connection, decoder and last frame it reaches go with this window, not some time later.
        window.makeFirstResponder(nil)
        window.contentView = nil
        onClosed?(endReason)
        onClosed = nil
    }
}
