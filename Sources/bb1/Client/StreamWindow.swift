import AppKit
import AVFoundation
import BB1Core

/// The remote screen plus mouse input. Keyboard input is handled by `KeyboardCapture`.
final class VideoView: NSView {
    let displayLayer = AVSampleBufferDisplayLayer()
    private let overlay = CATextLayer()
    private let banner = CATextLayer()
    private var overlayLines = 1
    private let blankCursor = NSCursor(image: NSImage(size: NSSize(width: 1, height: 1)), hotSpot: .zero)

    var streamSize = CGSize(width: 16, height: 9)
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
    var onInput: ((ControlMessage) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        layer = CALayer()
        wantsLayer = true
        layer!.backgroundColor = NSColor.black.cgColor
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = NSColor.black.cgColor
        layer!.addSublayer(displayLayer)

        for text in [overlay, banner] {
            text.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .medium)
            text.fontSize = 12
            text.foregroundColor = NSColor.white.cgColor
            text.backgroundColor = NSColor.black.withAlphaComponent(0.6).cgColor
            text.cornerRadius = 6
            text.isWrapped = true
            text.isHidden = true
            layer!.addSublayer(text)
        }
        banner.alignmentMode = .center
        banner.fontSize = 14
    }

    required init?(coder: NSCoder) { fatalError() }

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override var isOpaque: Bool { true }

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
        let width = min(bounds.width - 20, 620)
        let height = CGFloat(overlayLines) * 15 + 10
        overlay.frame = CGRect(x: 10, y: bounds.height - 10 - height, width: width, height: height)
        let bw = min(bounds.width - 40, 520)
        banner.frame = CGRect(x: (bounds.width - bw) / 2, y: bounds.height / 2 - 30, width: bw, height: 60)
        CATransaction.commit()
    }

    func setOverlay(_ text: String?) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        overlay.string = text
        overlay.isHidden = text == nil
        CATransaction.commit()
        if let text {
            // Rough line count at ~88 monospaced characters per line.
            overlayLines = text.split(separator: "\n").reduce(0) { $0 + max(1, ($1.count + 87) / 88) }
            relayout()
        }
    }

    func setBanner(_ text: String?) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        banner.string = text.map { "\n\($0)" }
        banner.isHidden = text == nil
        CATransaction.commit()
    }

    // MARK: Cursor

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: shownCursor)
    }

    private func updateCursor() {
        guard let window, window.isKeyWindow else { return }
        let p = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        if bounds.contains(p) { shownCursor.set() }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect, .cursorUpdate],
                                       owner: self, userInfo: nil))
    }

    override func cursorUpdate(with event: NSEvent) {
        shownCursor.set()
    }

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

    override func mouseMoved(with event: NSEvent) { sendMove(event) }
    override func mouseDragged(with event: NSEvent) { sendMove(event) }
    override func rightMouseDragged(with event: NSEvent) { sendMove(event) }
    override func otherMouseDragged(with event: NSEvent) { sendMove(event) }
    override func mouseDown(with event: NSEvent) {
        if !inputEnabled() { onClickToControl?() }
        sendButton(0, true, event)
    }
    override func mouseUp(with event: NSEvent) { sendButton(0, false, event) }
    override func rightMouseDown(with event: NSEvent) { sendButton(1, true, event) }
    override func rightMouseUp(with event: NSEvent) { sendButton(1, false, event) }
    override func otherMouseDown(with event: NSEvent) { sendButton(event.buttonNumber, true, event) }
    override func otherMouseUp(with event: NSEvent) { sendButton(event.buttonNumber, false, event) }

    override func scrollWheel(with event: NSEvent) {
        guard inputEnabled() else { return }
        let sign = invertScroll ? -1.0 : 1.0
        onInput?(.scroll(ScrollMsg(dx: event.scrollingDeltaX * sign, dy: event.scrollingDeltaY * sign,
                                   precise: event.hasPreciseScrollingDeltas,
                                   phase: event.phase.rawValue, momentum: event.momentumPhase.rawValue)))
    }
}

/// One stream window and its connection, plus the stream options (menus and ⌃⌥⇧ shortcuts).
final class StreamWindowController: NSObject, NSWindowDelegate {
    let window: NSWindow
    let view: VideoView
    let connection: ClientConnection
    private let target: ClientTarget
    private let renderer: FrameRenderer
    private var keyboard: KeyboardCapture!
    private var settings = ClientSettings.load()
    private var connected = false
    private var showStats = false
    private var lastStats: ClientStats?
    private var notice: String?
    private var config: StreamConfig?
    private var windowedFrame: NSRect?
    private var wasCapturing = false
    var onClosed: ((String?) -> Void)?
    var onPairedHost: (() -> Void)?
    /// Status text, connected, latest stats (for the menu bar).
    var onUpdate: ((String, Bool, ClientStats?) -> Void)?
    private var endReason: String?

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
        connection.decoder.onFrame = { pixelBuffer, captureUs in
            renderer.enqueue(pixelBuffer)
            connection.markDisplayed(captureUs: captureUs)
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
            self?.window.makeKey()
            NSApp.activate(ignoringOtherApps: true)
        }
        keyboard = KeyboardCapture(
            isActive: { [weak self] in self?.isCapturing ?? false },
            onKey: { connection.send(.key($0)) },
            onLocalCommand: { [weak self] code in self?.localCommand(code) })
        keyboard.forwardKeys = { [weak self] in self?.settings.input.keyboard ?? false }
        keyboard.onCapsLock = { connection.send(.capsLock($0)) }
        connection.setSound(settings.sound)

        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.updateCaptureState()
            }
        }
        updateTitle()
    }

    /// bb1 is in front, this window is focused, and we're connected: input goes to the host.
    private var isCapturing: Bool { connected && NSApp.isActive && window.isKeyWindow }

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
        let now = isCapturing
        if now != wasCapturing {
            wasCapturing = now
            if now {
                Log.info("client: input → host (\(settings.input.title))")
                // Caps Lock may have changed while bb1 wasn't in front.
                connection.send(.capsLock(NSEvent.modifierFlags.contains(.capsLock)))
            } else {
                let why = !connected ? "not connected" : !NSApp.isActive ? "bb1 is in the background" : "window not focused"
                Log.info("client: input released (\(why))")
                connection.send(.releaseAll)
                keyboard.reset()
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

    /// Give the keyboard and mouse back to this Mac (like clicking away in Parsec). Click the picture to take control again.
    @objc func releaseInput() {
        connection.send(.releaseAll)
        keyboard.reset()
        NSApp.deactivate()
        Log.info("client: input released (⌃⌥⇧R)")
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

    private func localCommand(_ code: UInt16) {
        switch code {
        case KeyboardCapture.fullscreenKey: toggleFullscreen()
        case KeyboardCapture.statsKey: toggleStats()
        case KeyboardCapture.cursorKey: toggleCursor()
        case KeyboardCapture.disconnectKey: disconnect()
        case KeyboardCapture.invertScrollKey: toggleScroll()
        case KeyboardCapture.releaseKey: releaseInput()
        case KeyboardCapture.muteKey: toggleSound()
        default: break
        }
    }

    /// The stream options, for the app's menu bar and the bb1 icon's menu.
    func menuItems() -> [NSMenuItem] {
        var items: [NSMenuItem] = []
        func item(_ title: String, _ action: Selector, key: String = "", on: Bool = false, enabled: Bool = true) -> NSMenuItem {
            let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
            if !key.isEmpty { i.keyEquivalentModifierMask = [.control, .option, .shift] }
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
        items.append(item("Fullscreen", #selector(toggleFullscreen), key: "f", on: fullscreen))
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
        items.append(item("Release Input", #selector(releaseInput), key: "r"))
        items.append(item("Flip Scroll Direction", #selector(toggleScroll), key: "i", on: settings.invertScroll))
        items.append(item("Use This Mac's Cursor (instant)", #selector(toggleCursor), key: "c", on: !settings.cursorInVideo))
        items.append(.separator())
        let soundAvailable = connection.hostHasSound
        items.append(item(soundAvailable ? "Mute Sound" : "Mute Sound (host needs updating)", #selector(toggleSound), key: "m",
                          on: !settings.sound, enabled: soundAvailable))
        items.append(item("Show Stats", #selector(toggleStats), key: "s", on: showStats))
        items.append(.separator())
        items.append(item("Disconnect", #selector(disconnect), key: "d"))
        return items
    }

    private func refreshOverlay() {
        guard showStats || notice != nil else { view.setOverlay(nil); return }
        var lines: [String] = []
        if let notice { lines.append(notice.hasPrefix("The host") ? "⚠️ \(notice)" : notice) }
        if showStats, let s = lastStats {
            lines.append(s.summary)
            lines.append("system shortcuts: \(keyboard.hasSystemShortcuts ? "sent to host" : "stay local (allow Accessibility)") · input: \(settings.input.title) · sound: \(settings.sound ? "on" : "muted")")
            lines.append("⌃⌥⇧ F fullscreen · R release · M mute · S stats · C cursor · I scroll · D disconnect")
        }
        view.setOverlay(lines.joined(separator: "\n"))
    }

    func windowDidBecomeKey(_ notification: Notification) { updateCaptureState() }

    func windowDidResignKey(_ notification: Notification) { updateCaptureState() }

    func windowWillClose(_ notification: Notification) {
        NotificationCenter.default.removeObserver(self)
        keyboard.stop()
        connection.stop()
        onClosed?(endReason)
        onClosed = nil
    }
}
