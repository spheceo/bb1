import AppKit
import ApplicationServices
import BB1Core

/// Sends every key to the host while the stream window is focused, including system shortcuts
/// like Cmd+Tab and Cmd+Space. Those need an event tap, which needs Accessibility permission for
/// this app. Without it, normal keys still work through an in-app monitor.
///
/// Two kinds of chord stay on this Mac: ⌘⎋ gives control back, and Globe-Control chords (macOS window
/// tiling, e.g. 🌐⌃← puts the stream window on the left half) arrange windows. Everything else goes to the host.
final class KeyboardCapture {
    static let escapeKey: UInt16 = 53

    private let isActive: () -> Bool
    /// False in Mouse-only / View-only mode: keys stay on this Mac (shortcuts still work).
    var forwardKeys: () -> Bool = { true }
    private let onKey: (KeyMsg) -> Void
    /// Caps Lock turned on/off on this Mac (it isn't sent as a key; the host matches the state).
    var onCapsLock: ((Bool) -> Void)?
    private let onGiveBack: () -> Void

    private var tap: CFMachPort?
    private var tapSource: CFRunLoopSource?
    private var monitor: Any?
    private var retryTimer: Timer?
    private var swallowedKeyUps = Set<UInt16>()
    /// Keys whose key-down went to this Mac (window tiling), so their key-up does too.
    private var localKeyUps = Set<UInt16>()
    private var heldModifiers = Set<UInt16>()
    private(set) var hasSystemShortcuts = false

    init(isActive: @escaping () -> Bool, onKey: @escaping (KeyMsg) -> Void, onGiveBack: @escaping () -> Void) {
        self.isActive = isActive
        self.onKey = onKey
        self.onGiveBack = onGiveBack
    }

    func start() {
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            guard let self, self.isActive() else { return event }
            if self.hasSystemShortcuts { return nil } // the tap already handled it
            if !self.forwardKeys() {
                if event.type == .keyDown, self.isGiveBack(event.keyCode, CGEventFlags(rawValue: UInt64(event.modifierFlags.rawValue))) {
                    if !event.isARepeat { self.onGiveBack() }
                    return nil
                }
                return event
            }
            if event.type == .flagsChanged && event.keyCode == 57 {
                self.onCapsLock?(event.modifierFlags.contains(.capsLock))
                return event
            }
            let kind: CGEventType = event.type == .keyDown ? .keyDown : event.type == .keyUp ? .keyUp : .flagsChanged
            self.route(code: event.keyCode, type: kind,
                       flags: CGEventFlags(rawValue: UInt64(event.modifierFlags.rawValue)),
                       isRepeat: event.type == .keyDown && event.isARepeat)
            return nil
        }
        if AXIsProcessTrusted() {
            installTap()
        } else if ProcessInfo.processInfo.environment["BB1_NO_PROMPT"] == nil {
            Permissions.askForMissing(screen: false, accessibility: true)
            Log.info("client: no Accessibility permission yet; Cmd+Tab and other system shortcuts stay on this Mac")
            retryTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
                guard let self, AXIsProcessTrusted() else { return }
                self.installTap()
            }
        }
    }

    // The event tap holds an unretained pointer to this object: it must be gone before this is.
    deinit { stop() }

    func stop() {
        retryTimer?.invalidate()
        retryTimer = nil
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        if let tap {
            CGEvent.tapEnable(tap: tap, enable: false)
            CFMachPortInvalidate(tap)
        }
        if let tapSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), tapSource, .commonModes) }
        tap = nil
        tapSource = nil
        hasSystemShortcuts = false
    }

    /// Forget held keys, e.g. when the window loses focus (the host releases its side too).
    func reset() {
        heldModifiers.removeAll()
        swallowedKeyUps.removeAll()
        localKeyUps.removeAll()
    }

    private func installTap() {
        guard tap == nil else { return }
        retryTimer?.invalidate()
        retryTimer = nil
        let mask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue) | (1 << CGEventType.flagsChanged.rawValue)
        let callback: CGEventTapCallBack = { _, type, event, refcon in
            let me = Unmanaged<KeyboardCapture>.fromOpaque(refcon!).takeUnretainedValue()
            return me.handleTap(type: type, event: event)
        }
        guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
                                          eventsOfInterest: CGEventMask(mask), callback: callback,
                                          userInfo: Unmanaged.passUnretained(self).toOpaque()) else {
            Log.info("client: couldn't create the keyboard event tap")
            return
        }
        let source = CFMachPortCreateRunLoopSource(nil, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.tap = tap
        tapSource = source
        hasSystemShortcuts = true
        Log.info("client: keyboard tap active; system shortcuts go to the host while the stream window is focused")
    }

    private func handleTap(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return Unmanaged.passUnretained(event)
        }
        guard isActive() else { return Unmanaged.passUnretained(event) }
        let code = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
        if type == .flagsChanged && code == 57 {
            // Caps Lock: let this Mac toggle it as usual, and tell the host the new state.
            onCapsLock?(event.flags.contains(.maskAlphaShift))
            return Unmanaged.passUnretained(event)
        }
        if type == .keyDown, heldModifiers.contains(63), event.flags.contains(.maskControl) {
            localKeyUps.insert(code) // window tiling: arrange the stream window here
            return Unmanaged.passUnretained(event)
        }
        if type == .keyUp, localKeyUps.remove(code) != nil { return Unmanaged.passUnretained(event) }
        if !forwardKeys() {
            // Keys stay on this Mac; only bb1's own shortcuts are taken.
            if type == .keyDown, isGiveBack(code, event.flags) {
                if event.getIntegerValueField(.keyboardEventAutorepeat) == 0 { onGiveBack() }
                return nil
            }
            return Unmanaged.passUnretained(event)
        }
        route(code: code, type: type, flags: event.flags,
              isRepeat: event.getIntegerValueField(.keyboardEventAutorepeat) != 0)
        return nil
    }

    // Left/right device bits, so we can tell which modifier changed.
    private static let modifierBits: [UInt16: UInt64] = [
        55: 0x08, 54: 0x10, 56: 0x02, 60: 0x04, 58: 0x20, 61: 0x40, 59: 0x01, 62: 0x2000,
        63: CGEventFlags.maskSecondaryFn.rawValue,
    ]

    /// ⌘⎋ (Command and Escape, nothing else).
    private func isGiveBack(_ code: UInt16, _ flags: CGEventFlags) -> Bool {
        code == KeyboardCapture.escapeKey && flags.contains(.maskCommand)
            && !flags.contains(.maskControl) && !flags.contains(.maskAlternate) && !flags.contains(.maskShift)
    }

    private func route(code: UInt16, type: CGEventType, flags: CGEventFlags, isRepeat: Bool) {
        switch type {
        case .keyDown:
            if isGiveBack(code, flags) {
                swallowedKeyUps.insert(code)
                if !isRepeat { onGiveBack() }
                return
            }
            onKey(KeyMsg(code: code, down: true, isRepeat: isRepeat))
        case .keyUp:
            if swallowedKeyUps.remove(code) != nil { return }
            onKey(KeyMsg(code: code, down: false, isRepeat: false))
        case .flagsChanged:
            if code == 57 { // caps lock: report the state instead of sending a key
                onCapsLock?(flags.contains(.maskAlphaShift))
                return
            }
            let down: Bool
            if let bit = KeyboardCapture.modifierBits[code] {
                down = flags.rawValue & bit != 0
            } else {
                down = !heldModifiers.contains(code)
            }
            if down { heldModifiers.insert(code) } else { heldModifiers.remove(code) }
            onKey(KeyMsg(code: code, down: down, isRepeat: false))
        default:
            break
        }
    }
}
