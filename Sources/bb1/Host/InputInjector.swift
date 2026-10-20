import BB1Core
import CoreGraphics
import Foundation
import IOKit.hidsystem

/// Replays the client's mouse and keyboard on this Mac. Needs Accessibility permission.
final class InputInjector {
    private let source = CGEventSource(stateID: .hidSystemState)
    private var keysDown = Set<UInt16>()
    private var buttonsDown = Set<Int>()
    private var lastPoint = CGPoint.zero
    private var scrollRemainder = (x: 0.0, y: 0.0)
    private var capsLockOn = false
    var invertScroll = false

    // Virtual key code → (generic flag, device-specific left/right bit)
    private static let modifiers: [UInt16: (CGEventFlags, UInt64)] = [
        55: (.maskCommand, 0x08), 54: (.maskCommand, 0x10),
        56: (.maskShift, 0x02), 60: (.maskShift, 0x04),
        58: (.maskAlternate, 0x20), 61: (.maskAlternate, 0x40),
        59: (.maskControl, 0x01), 62: (.maskControl, 0x2000),
        63: (.maskSecondaryFn, 0),
    ]
    private static let capsLock: UInt16 = 57

    private var flags: CGEventFlags {
        var raw: UInt64 = capsLockOn ? CGEventFlags.maskAlphaShift.rawValue : 0
        for key in keysDown {
            if let (flag, device) = InputInjector.modifiers[key] { raw |= flag.rawValue | device }
        }
        return CGEventFlags(rawValue: raw)
    }

    private func point(_ x: Double, _ y: Double) -> CGPoint {
        let bounds = CGDisplayBounds(CGMainDisplayID())
        let cx = min(max(x, 0), 1), cy = min(max(y, 0), 1)
        return CGPoint(x: bounds.minX + cx * (bounds.width - 1), y: bounds.minY + cy * (bounds.height - 1))
    }

    /// Returns true if the pointer actually moved (used by the capture watchdog).
    @discardableResult
    func move(_ m: MouseMoveMsg) -> Bool {
        let p = point(m.x, m.y)
        let moved = abs(p.x - lastPoint.x) >= 1 || abs(p.y - lastPoint.y) >= 1
        lastPoint = p
        let type: CGEventType, button: CGMouseButton
        if buttonsDown.contains(0) { (type, button) = (.leftMouseDragged, .left) }
        else if buttonsDown.contains(1) { (type, button) = (.rightMouseDragged, .right) }
        else if !buttonsDown.isEmpty { (type, button) = (.otherMouseDragged, .center) }
        else { (type, button) = (.mouseMoved, .left) }
        guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: button) else { return false }
        e.flags = flags
        e.post(tap: .cghidEventTap)
        return moved
    }

    func button(_ b: MouseButtonMsg) {
        let p = point(b.x, b.y)
        lastPoint = p
        let type: CGEventType, button: CGMouseButton
        switch b.button {
        case 0: (type, button) = (b.down ? .leftMouseDown : .leftMouseUp, .left)
        case 1: (type, button) = (b.down ? .rightMouseDown : .rightMouseUp, .right)
        default: (type, button) = (b.down ? .otherMouseDown : .otherMouseUp, .center)
        }
        guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: button) else { return }
        e.setIntegerValueField(.mouseEventClickState, value: Int64(max(1, b.clicks)))
        if b.button >= 2 { e.setIntegerValueField(.mouseEventButtonNumber, value: Int64(b.button)) }
        e.flags = flags
        e.post(tap: .cghidEventTap)
        if b.down { buttonsDown.insert(b.button) } else { buttonsDown.remove(b.button) }
    }

    func scroll(_ s: ScrollMsg) {
        let sign = invertScroll ? -1.0 : 1.0
        scrollRemainder.x += s.dx * sign
        scrollRemainder.y += s.dy * sign
        let dx = Int32(scrollRemainder.x.rounded(.towardZero))
        let dy = Int32(scrollRemainder.y.rounded(.towardZero))
        scrollRemainder.x -= Double(dx)
        scrollRemainder.y -= Double(dy)
        let phase = InputInjector.scrollPhase(s.phase)
        let momentum = InputInjector.momentumPhase(s.momentum)
        // Phase-only events (began/ended with zero delta) still matter for trackpad scrolling.
        guard dx != 0 || dy != 0 || phase != 0 || momentum != 0 else { return }
        guard let e = CGEvent(scrollWheelEvent2Source: source, units: s.precise ? .pixel : .line,
                              wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0) else { return }
        e.setIntegerValueField(.scrollWheelEventIsContinuous, value: s.precise ? 1 : 0)
        e.setIntegerValueField(.scrollWheelEventScrollPhase, value: phase)
        e.setIntegerValueField(.scrollWheelEventMomentumPhase, value: momentum)
        e.location = lastPoint
        e.flags = flags
        e.post(tap: .cghidEventTap)
    }

    /// Match the client's Caps Lock: set this Mac's real Caps Lock (so the light and every app agree),
    /// and mark typed letters as capitals while it's on.
    func setCapsLock(_ on: Bool) {
        guard on != capsLockOn || on != InputInjector.systemCapsLock() else { return }
        capsLockOn = on
        let service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching(kIOHIDSystemClass))
        guard service != 0 else { return }
        defer { IOObjectRelease(service) }
        var connect: io_connect_t = 0
        guard IOServiceOpen(service, mach_task_self_, UInt32(kIOHIDParamConnectType), &connect) == KERN_SUCCESS else {
            Log.info("input: couldn't open the keyboard driver to set Caps Lock")
            return
        }
        defer { IOServiceClose(connect) }
        let result = IOHIDSetModifierLockState(connect, Int32(kIOHIDCapsLockState), on)
        Log.info("input: Caps Lock \(on ? "on" : "off")\(result == KERN_SUCCESS ? "" : " (couldn't set the system state: \(result))")")
    }

    private static func systemCapsLock() -> Bool {
        CGEventSource.flagsState(.hidSystemState).contains(.maskAlphaShift)
    }

    func key(_ k: KeyMsg) {
        if k.code == InputInjector.capsLock { return } // handled by setCapsLock
        let isModifier = InputInjector.modifiers[k.code] != nil
        if k.down { keysDown.insert(k.code) } else { keysDown.remove(k.code) }
        guard let e = CGEvent(keyboardEventSource: source, virtualKey: k.code, keyDown: k.down) else { return }
        if isModifier { e.type = .flagsChanged }
        if k.isRepeat { e.setIntegerValueField(.keyboardEventAutorepeat, value: 1) }
        e.flags = flags
        e.post(tap: .cghidEventTap)
    }

    /// Lift every key and button we think is held, so nothing stays stuck after a disconnect.
    func releaseAll() {
        let held = keysDown.sorted { InputInjector.modifiers[$0] == nil && InputInjector.modifiers[$1] != nil }
        for code in held { key(KeyMsg(code: code, down: false, isRepeat: false)) }
        let bounds = CGDisplayBounds(CGMainDisplayID())
        for b in buttonsDown {
            let x = bounds.width > 1 ? (lastPoint.x - bounds.minX) / (bounds.width - 1) : 0
            let y = bounds.height > 1 ? (lastPoint.y - bounds.minY) / (bounds.height - 1) : 0
            button(MouseButtonMsg(button: b, down: false, clicks: 1, x: x, y: y))
        }
        keysDown.removeAll()
        buttonsDown.removeAll()
        scrollRemainder = (0, 0)
    }

    // NSEvent.Phase raw → CGScrollPhase raw
    private static func scrollPhase(_ p: UInt) -> Int64 {
        switch p {
        case 1: return 1      // began
        case 4: return 2      // changed
        case 8: return 4      // ended
        case 16: return 8     // cancelled
        case 32: return 128   // mayBegin
        default: return 0
        }
    }

    // NSEvent.Phase raw (momentum) → CGMomentumScrollPhase raw
    private static func momentumPhase(_ p: UInt) -> Int64 {
        switch p {
        case 1: return 1      // begin
        case 4: return 2      // continue
        case 8, 16: return 3  // end
        default: return 0
        }
    }
}
