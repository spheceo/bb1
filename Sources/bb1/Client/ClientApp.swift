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

/// Client mode: the connect window, the stream window, and the client part of the menu bar menu.
final class ClientRole: NSObject {
    let options: ClientOptions
    var onChange: (() -> Void)?
    /// Whether bb1 stays in the menu bar after its windows close.
    var keepRunning: () -> Bool = { true }
    private var stream: StreamWindowController?
    /// Show/hide the dashboard (set by the app).
    var showDashboard: ((String?) -> Void)?
    var hideDashboard: (() -> Void)?
    private(set) var statusText = "Not connected"
    private(set) var stats: ClientStats?
    private(set) var connected = false

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


    func showConnect(_ message: String?) {
        if let stream {
            stream.window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        showDashboard?(message)
    }

    private func open(_ target: ClientTarget) {
        hideDashboard?()
        let s = StreamWindowController(target: target)
        s.onPairedHost = { [weak self] in self?.onChange?() }
        s.onUpdate = { [weak self] status, connected, stats in
            self?.statusText = status
            self?.connected = connected
            if let stats { self?.stats = stats }
            self?.onChange?()
        }
        s.onClosed = { [weak self] reason in
            guard let self else { return }
            self.stream = nil
            self.connected = false
            self.statusText = "Not connected"
            self.stats = nil
            self.onChange?()
            DispatchQueue.main.async {
                // Closed because something went wrong: show why. Closed on purpose: stay out of the way.
                // Back to the bb1 window after disconnecting (with the reason if something went wrong).
                self.showConnect(reason)
            }
        }
        stream = s
        s.show()
        onChange?()
    }

    func windowsClosed() {
        if !keepRunning() && stream == nil { NSApp.terminate(nil) }
    }

    /// Fills the app's "Stream" menu (top menu bar) each time it opens.
    func fillStreamMenu(_ menu: NSMenu) {
        menu.removeAllItems()
        if let stream {
            stream.menuItems().forEach(menu.addItem)
        } else {
            let item = NSMenuItem(title: "Connect…", action: #selector(connectMenu), keyEquivalent: "")
            item.target = self
            menu.addItem(item)
        }
    }

    @objc private func connectMenu() { showConnect(nil) }
    @objc private func disconnect() { stream?.window.close() }
    @objc private func showStream() {
        stream?.window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    var isActive: Bool { connected }

    /// The client part of the menu bar menu.
    func addItems(to menu: NSMenu) {
        func info(_ title: String) {
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        }
        func action(_ title: String, _ symbol: String, _ selector: Selector) {
            let item = NSMenuItem(title: title, action: selector, keyEquivalent: "")
            item.target = self
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
            menu.addItem(item)
        }
        if connected, let s = stats {
            var line = statusText + String(format: " · %.0f fps", s.fps)
            if let l = s.latencyMs { line += String(format: " · %.0f ms", l) }
            info(line)
        } else {
            info(stream == nil ? "Not connected" : statusText)
        }
        if let stream {
            let options = NSMenuItem(title: "Stream", action: nil, keyEquivalent: "")
            options.image = NSImage(systemSymbolName: "slider.horizontal.3", accessibilityDescription: nil)
            let sub = NSMenu()
            sub.autoenablesItems = false
            stream.menuItems().forEach(sub.addItem)
            options.submenu = sub
            menu.addItem(options)
            action("Disconnect", "xmark.circle", #selector(disconnect))
        } else {
            action("Connect…", "display", #selector(connectMenu))
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

