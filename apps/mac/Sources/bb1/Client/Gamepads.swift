import AppKit
import BB1Core
import CoreHaptics
import GameController

/// This Mac's game controllers → the host (GameController framework), as GamepadMsg in the Xbox
/// layout (docs/08-gaming-host.md). Up to four, numbered in the order they connected. State goes out
/// when it changes, polled at 250 Hz, and only while this connection's stream window is key: in the
/// background the pads are sent centred with nothing pressed. Rumble from the host plays through
/// the controller's haptics where it has them (DualSense, DualShock 4, Xbox), best effort.
/// Main thread only.
final class Gamepads {
    private weak var connection: ClientConnection?
    private var timer: DispatchSourceTimer?
    /// Pad number → controller.
    private var slots: [GCController?] = Array(repeating: nil, count: 4)
    /// What the host last got, per pad (nil: nothing this session).
    private var sent: [GamepadMsg?] = Array(repeating: nil, count: 4)
    private var haptics: [Int: (engine: CHHapticEngine, player: CHHapticPatternPlayer?)] = [:]

    init(connection: ClientConnection) {
        self.connection = connection
    }

    /// A session started: send every controller's state again (the host has none of them yet).
    func start() {
        sent = Array(repeating: nil, count: 4)
        guard timer == nil else { return }
        let t = DispatchSource.makeTimerSource(queue: .main)
        t.schedule(deadline: .now(), repeating: .milliseconds(4), leeway: .milliseconds(1))
        t.setEventHandler { [weak self] in self?.poll() }
        t.resume()
        timer = t
    }

    /// The session ended (or is reconnecting).
    func stop() {
        timer?.cancel()
        timer = nil
        for pad in haptics.keys { stopRumble(pad) }
        haptics.removeAll()
    }

    func rumble(_ r: RumbleMsg) {
        guard (0..<4).contains(r.pad), let controller = slots[r.pad] else { return }
        stopRumble(r.pad)
        guard r.large > 0 || r.small > 0 else { return }
        guard let engine = engine(r.pad, controller) else { return }
        // One continuous effect: strength from the stronger motor, sharper when the small (fast) one leads.
        let intensity = Float(max(r.large, r.small)) / 255
        let sharpness: Float = r.small > r.large ? 0.8 : 0.3
        let event = CHHapticEvent(eventType: .hapticContinuous, parameters: [
            CHHapticEventParameter(parameterID: .hapticIntensity, value: min(1, intensity)),
            CHHapticEventParameter(parameterID: .hapticSharpness, value: sharpness),
        ], relativeTime: 0, duration: 30) // until the host says otherwise
        do {
            let player = try engine.makePlayer(with: CHHapticPattern(events: [event], parameters: []))
            try player.start(atTime: CHHapticTimeImmediate)
            haptics[r.pad]?.player = player
        } catch {}
    }

    private func engine(_ pad: Int, _ controller: GCController) -> CHHapticEngine? {
        if let e = haptics[pad]?.engine { return e }
        guard let e = controller.haptics?.createEngine(withLocality: .default) else { return nil }
        e.isAutoShutdownEnabled = false
        do { try e.start() } catch { return nil }
        haptics[pad] = (e, nil)
        return e
    }

    private func stopRumble(_ pad: Int) {
        try? haptics[pad]?.player?.stop(atTime: CHHapticTimeImmediate)
        haptics[pad]?.player = nil
    }

    private func poll() {
        guard let connection else { stop(); return }
        let present = GCController.controllers().filter { $0.extendedGamepad != nil }
        // Unplugged ones free their pad number; new ones take the lowest free.
        for (pad, c) in slots.enumerated() {
            guard let c, !present.contains(where: { $0 === c }) else { continue }
            Log.info("controllers: \(c.vendorName ?? "a controller") (pad \(pad)) disconnected")
            slots[pad] = nil
            stopRumble(pad)
            haptics[pad] = nil
            if sent[pad] != nil {
                connection.send(.gamepad(GamepadMsg(pad: pad, connected: false, buttons: 0, lt: 0, rt: 0, lx: 0, ly: 0, rx: 0, ry: 0, name: nil)))
            }
            sent[pad] = nil
        }
        for c in present where !slots.contains(where: { $0 === c }) {
            guard let pad = slots.firstIndex(where: { $0 == nil }) else { break }
            slots[pad] = c
            Log.info("controllers: \(c.vendorName ?? "a controller") is pad \(pad)")
        }

        // NSApp is nil in the headless client (tests): no window, so nothing counts as in front.
        guard let app = NSApp else { return }
        let front = app.isActive && (app.keyWindow?.delegate as? StreamWindowController)?.connection === connection
        for (pad, c) in slots.enumerated() {
            guard let c, let g = c.extendedGamepad else { continue }
            var state = front ? Self.state(g, pad: pad) : Self.neutral(pad)
            if let last = sent[pad], Self.same(last, state) { continue }
            if sent[pad] == nil { state.name = c.vendorName }
            connection.send(.gamepad(state))
            sent[pad] = state
        }
    }

    // MARK: Mapping (GameController → Xbox layout)

    static func stick(_ v: Float) -> Int { Int((max(-1, min(1, v)) * 32767).rounded()) }
    static func trigger(_ v: Float) -> Int { Int((max(0, min(1, v)) * 255).rounded()) }

    static func neutral(_ pad: Int) -> GamepadMsg {
        GamepadMsg(pad: pad, connected: true, buttons: 0, lt: 0, rt: 0, lx: 0, ly: 0, rx: 0, ry: 0, name: nil)
    }

    static func same(_ a: GamepadMsg, _ b: GamepadMsg) -> Bool {
        var a = a, b = b
        a.name = nil; b.name = nil
        return a == b
    }

    /// GameController's axes are −1…1 with up positive, like ours.
    static func state(_ g: GCExtendedGamepad, pad: Int) -> GamepadMsg {
        var buttons = 0
        let map: [(GCControllerButtonInput?, Int)] = [
            (g.buttonA, 0x1000), (g.buttonB, 0x2000), (g.buttonX, 0x4000), (g.buttonY, 0x8000),
            (g.leftShoulder, 0x0100), (g.rightShoulder, 0x0200),
            (g.leftThumbstickButton, 0x0040), (g.rightThumbstickButton, 0x0080),
            (g.buttonMenu, 0x0010), (g.buttonOptions, 0x0020), (g.buttonHome, 0x0400),
            (g.dpad.up, 0x0001), (g.dpad.down, 0x0002), (g.dpad.left, 0x0004), (g.dpad.right, 0x0008),
        ]
        for (button, bit) in map where button?.isPressed == true { buttons |= bit }
        return GamepadMsg(pad: pad, connected: true, buttons: buttons,
                          lt: trigger(g.leftTrigger.value), rt: trigger(g.rightTrigger.value),
                          lx: stick(g.leftThumbstick.xAxis.value), ly: stick(g.leftThumbstick.yAxis.value),
                          rx: stick(g.rightThumbstick.xAxis.value), ry: stick(g.rightThumbstick.yAxis.value), name: nil)
    }
}
