import AppKit
import BB1Core

struct ClientOptions {
    var host: String?
    var key: String?
    var tailscale = false
    var pair = false
    var headless = false
    var seconds: Double = 10

    static let usage = """
    usage: bb1 client [--host ADDRESS[:PORT]] [--key KEY | --tailscale | --pair] [--headless [--seconds N]]
      --host           connect straight away (signs in with a remembered pairing if there is one)
      --key KEY        sign in with an old pairing key
      --tailscale      sign in with Tailscale (the address must be a Tailscale address)
      --pair           pair with a 6-digit code (asked in the terminal with --headless)
      --headless       no window: decode and print stats, then exit (for testing)
      --seconds N      how long --headless runs (default 10)
    """

    static func parse(_ args: [String]) -> ClientOptions {
        var o = ClientOptions()
        var i = 1
        func next() -> String {
            i += 1
            guard i < args.count else { print(usage); exit(2) }
            return args[i]
        }
        while i < args.count {
            switch args[i] {
            case "--host": o.host = next()
            case "--key": o.key = next()
            case "--tailscale": o.tailscale = true
            case "--pair": o.pair = true
            case "--headless": o.headless = true
            case "--seconds": o.seconds = Double(next()) ?? 10
            case "-h", "--help": print(usage); exit(0)
            default:
                if args[i].hasPrefix("-psn") || args[i].hasPrefix("-NS") { i += 1; continue }
                print("unknown option \(args[i])\n\(usage)"); exit(2)
            }
            i += 1
        }
        return o
    }

    func target() -> ClientTarget? {
        guard let host, let endpoint = parseEndpoint(host) else { return nil }
        let address = host.split(separator: ":").first.map(String.init) ?? host
        let tailscaleIP = Tailscale.isTailscaleIP(address) ? address : nil
        let auth: ControlChannel.ClientAuth
        if let key, let k = PairingKey(string: key) {
            auth = .legacyKey(k)
        } else if tailscale {
            auth = .tailscale
        } else if pair {
            auth = .code
        } else if let known = KnownHost.store.load().first(where: { $0.address == host || $0.tailscaleIP == address || $0.name == host }) {
            auth = .device(secret: known.secret)
        } else if tailscaleIP != nil {
            auth = .tailscale
        } else {
            auth = .code
        }
        return ClientTarget(endpoint: endpoint, label: host, auth: auth, tailscaleIP: tailscaleIP)
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

/// Client mode: the stream windows (one per Mac, side by side if you like), and the client part of the menu bar menu.
final class ClientRole: NSObject {
    let options: ClientOptions
    var onChange: (() -> Void)?
    /// Whether bb1 stays in the menu bar after its windows close.
    var keepRunning: () -> Bool = { true }
    /// Show/hide the dashboard (set by the app).
    var showDashboard: ((String?) -> Void)?
    var hideDashboard: (() -> Void)?

    /// One open stream: its window and what it last reported.
    private final class Stream {
        let controller: StreamWindowController
        var status = "Connecting…"
        var connected = false
        var stats: ClientStats?
        init(_ controller: StreamWindowController) { self.controller = controller }
    }
    /// Oldest first. Every open stream window is owned here: a window whose controller went away would
    /// leave its keyboard hook pointing at freed memory.
    private var streams: [Stream] = []

    init(options: ClientOptions) {
        self.options = options
        super.init()
    }

    func connect(_ target: ClientTarget) { open(target) }

    func start() {
        if let target = options.target() {
            open(target)
        } else {
            showConnect(nil)
        }
    }

    func showConnect(_ message: String?) { showDashboard?(message) }

    private func open(_ target: ClientTarget) {
        hideDashboard?()
        // Already streaming from that Mac: bring its window forward instead of a second stream.
        if let existing = streams.first(where: { $0.controller.target.label == target.label }) {
            existing.controller.window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let c = StreamWindowController(target: target)
        let stream = Stream(c)
        c.onPairedHost = { [weak self] in self?.onChange?() }
        c.onUpdate = { [weak self, weak stream] status, connected, stats in
            guard let stream else { return }
            stream.status = status
            stream.connected = connected
            if let stats { stream.stats = stats }
            self?.onChange?()
        }
        c.onClosed = { [weak self, weak stream] reason in
            guard let self else { return }
            self.streams.removeAll { $0 === stream }
            self.onChange?()
            DispatchQueue.main.async {
                // Closed on purpose: that's it (no window, so the Dock icon goes too).
                // Ended by the host or by a failure: show why in the bb1 window.
                if let reason { self.showConnect(reason) } else { self.windowsClosed() }
            }
        }
        streams.append(stream)
        c.show()
        onChange?()
    }

    func windowsClosed() {
        if !keepRunning() && streams.isEmpty { NSApp.terminate(nil) }
    }

    /// The stream the Stream menu is about: the focused stream window, else the newest.
    private var current: Stream? {
        streams.first { $0.controller.window.isKeyWindow } ?? streams.last
    }

    /// Fills the app's "Stream" menu (top menu bar) each time it opens.
    func fillStreamMenu(_ menu: NSMenu) {
        menu.removeAllItems()
        if let current {
            current.controller.menuItems().forEach(menu.addItem)
        } else {
            let item = NSMenuItem(title: "Connect…", action: #selector(connectMenu), keyEquivalent: "")
            item.target = self
            menu.addItem(item)
        }
    }

    @objc private func connectMenu() { showConnect(nil) }
    @objc private func disconnect(_ sender: NSMenuItem) {
        guard sender.tag < streams.count else { return }
        streams[sender.tag].controller.window.close()
    }

    var isActive: Bool { streams.contains { $0.connected } }

    /// The client part of the menu bar menu: one block per open stream (Open bb1 is how you connect).
    func addItems(to menu: NSMenu) {
        for (i, stream) in streams.enumerated() {
            if i > 0 { menu.addItem(.separator()) }
            var line = stream.status
            if stream.connected, let s = stream.stats {
                line += String(format: " · %.0f fps", s.fps)
                if let l = s.latencyMs { line += String(format: " · %.0f ms", l) }
            }
            let info = NSMenuItem(title: line, action: nil, keyEquivalent: "")
            info.isEnabled = false
            menu.addItem(info)
            let options = NSMenuItem(title: "Stream", action: nil, keyEquivalent: "")
            options.image = NSImage(systemSymbolName: "slider.horizontal.3", accessibilityDescription: nil)
            let sub = NSMenu()
            sub.autoenablesItems = false
            stream.controller.menuItems().forEach(sub.addItem)
            options.submenu = sub
            menu.addItem(options)
            let disconnect = NSMenuItem(title: "Disconnect", action: #selector(disconnect(_:)), keyEquivalent: "")
            disconnect.target = self
            disconnect.tag = i
            disconnect.image = NSImage(systemSymbolName: "xmark.circle", accessibilityDescription: nil)
            menu.addItem(disconnect)
        }
    }
}

/// `--headless`: connect, decode, print stats once a second, exit after N seconds.
final class HeadlessClient {
    let connection: ClientConnection
    let seconds: Double
    private var last: ClientStats?

    init(target: ClientTarget, seconds: Double) {
        connection = ClientConnection(target: target)
        self.seconds = seconds
    }

    func run() {
        let connection = self.connection
        connection.decoder.onFrame = { _, captureUs in connection.markDisplayed(captureUs: captureUs) }
        connection.audio.muted = true // receive sound for the stats, but don't play it
        connection.askCode = { host, reply in
            print("Enter the 6-digit code shown on \(host): ", terminator: "")
            fflush(stdout)
            DispatchQueue.global().async {
                let line = readLine()
                DispatchQueue.main.async { reply(line) }
            }
        }
        connection.onStatus = { text, connected in
            print("status: \(text)")
            if connected { connection.send(.setCursorInVideo(false)) }
        }
        connection.onCursor = { c in print("cursor: \(Int(c.width))×\(Int(c.height)) pt, hot spot (\(Int(c.hotX)), \(Int(c.hotY))), \(c.png.count) bytes") }
        connection.onNotice = { print("notice: \($0)") }
        connection.onStats = { [weak self] s in
            self?.last = s
            print(s.summary)
        }
        connection.onEnded = { reason in
            print("ended: \(reason)")
            exit(1)
        }
        connection.start()
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
            connection.stop()
            let shown = self.last?.counters.displayed ?? 0
            print(shown > 0 ? "OK: \(shown) frames shown" : "FAIL: no frames shown")
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { exit(shown > 0 ? 0 : 1) }
        }
    }
}

