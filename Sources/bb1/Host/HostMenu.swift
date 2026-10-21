import AppKit
import BB1Core

/// Host mode: owns the HostController and fills in the host part of the menu bar menu.
final class HostRole: NSObject {
    let host: HostController
    var onChange: (() -> Void)?
    /// Opens the dashboard on the Host tab (set by the app).
    var showDashboard: (() -> Void)?

    private let pairingPrompt = PairingPrompt()

    init(options: HostOptions) {
        host = HostController(options: options)
        super.init()
        host.onChange = { [weak self] in self?.onChange?() }
        host.onPairingCode = { [weak self] device, code in self?.pairingPrompt.show(device: device, code: code) }
        host.onPairingFinished = { [weak self] _ in self?.pairingPrompt.dismiss() }
        pairingPrompt.onCancel = { [weak self] in self?.host.cancelPairing() }
    }

    func start() { host.start() }

    func stop() {
        pairingPrompt.dismiss()
        host.stop()
    }

    var hasClient: Bool { host.status().clientName != nil }

    var hasProblem: Bool {
        let s = host.status()
        return s.listenerError != nil || s.lastError != nil || !HostController.hasAccessibility
            || (!host.options.pipeline.synthetic && !HostController.hasScreenRecording)
    }

    var isActive: Bool { hasClient }

    func showStatusWindow() { showDashboard?() }

    /// The host part of the menu bar menu: status and the two things you actually do from there.
    func addItems(to menu: NSMenu) {
        let s = host.status()
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
        if let error = s.listenerError ?? s.lastError {
            info("⚠️ \(error)")
        } else if let name = s.clientName {
            info("Streaming to \(name)")
        } else {
            info("Hosting · waiting for a client")
        }
        if hasProblem && s.listenerError == nil && s.lastError == nil {
            action("Permissions Needed…", "exclamationmark.triangle", #selector(showStatus))
        }
        if s.clientName != nil { action("Disconnect Client", "xmark.circle", #selector(disconnect)) }
    }

    /// For Settings ▸ Test Recovery.
    func faultMenuItem() -> NSMenuItem {
        let faults = NSMenuItem(title: "Test Recovery", action: nil, keyEquivalent: "")
        let faultMenu = NSMenu()
        faultMenu.autoenablesItems = false
        for (i, fault) in StreamPipeline.Fault.allCases.enumerated() {
            let item = NSMenuItem(title: "Simulate: \(fault.rawValue)", action: #selector(injectFault(_:)), keyEquivalent: "")
            item.target = self
            item.tag = i
            item.isEnabled = hasClient
            faultMenu.addItem(item)
        }
        faults.submenu = faultMenu
        return faults
    }

    /// IPv4 addresses of this Mac, for typing into the client when Bonjour doesn't find it.
    static func addresses() -> [(String, String)] {
        var result: [(String, String)] = []
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        for ptr in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let ifa = ptr.pointee
            guard let addr = ifa.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
                  ifa.ifa_flags & UInt32(IFF_UP) != 0, ifa.ifa_flags & UInt32(IFF_LOOPBACK) == 0 else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST)
            let address = String(cString: host)
            if address.hasPrefix("169.254.") { continue }
            result.append((String(cString: ifa.ifa_name), address))
        }
        return result
    }

    @objc private func disconnect() { host.disconnectClient() }
    @objc private func showStatus() { showStatusWindow() }

    @objc private func injectFault(_ sender: NSMenuItem) {
        host.pipeline.inject(StreamPipeline.Fault.allCases[sender.tag])
    }

    @objc func copyKey() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(host.key.display, forType: .string)
    }

    @objc private func openScreenRecording() {
        CGRequestScreenCaptureAccess()
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!)
    }

    @objc private func openAccessibility() {
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!)
    }
}

/// Host status for the dashboard's Host tab: everything from the menu, plus the latest log lines.
final class HostPanel: NSObject {
    let view = NSView()
    private let text = NSTextView()
    private let tailscaleBox = NSButton()
    private let pairingBox = NSButton()
    private weak var role: HostRole?
    private var timer: Timer?

    init(role: HostRole) {
        self.role = role
        super.init()
        text.isEditable = false
        text.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        text.textContainerInset = NSSize(width: 8, height: 8)
        text.autoresizingMask = [.width]
        text.isVerticallyResizable = true
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.documentView = text
        scroll.borderType = .bezelBorder

        let settings = AppSettings.load()
        tailscaleBox.title = "Let my Tailscale devices connect without pairing"
        tailscaleBox.setButtonType(.switch)
        tailscaleBox.state = settings.allowTailscale ? .on : .off
        tailscaleBox.target = self
        tailscaleBox.action = #selector(toggleTailscale)
        pairingBox.title = "Allow pairing new Macs with a code"
        pairingBox.setButtonType(.switch)
        pairingBox.state = settings.allowPairing ? .on : .off
        pairingBox.target = self
        pairingBox.action = #selector(togglePairing)
        let forget = NSButton(title: "Forget Paired Macs…", target: self, action: #selector(forgetDevices))
        let logs = NSButton(title: "Open Log", target: self, action: #selector(openLog))
        let toggles = NSStackView(views: [tailscaleBox, pairingBox])
        toggles.orientation = .vertical
        toggles.alignment = .leading
        let buttons = NSStackView(views: [toggles, NSView(), forget, logs])
        buttons.distribution = .fill
        let stack = NSStackView(views: [scroll, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        scroll.translatesAutoresizingMaskIntoConstraints = false
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            stack.topAnchor.constraint(equalTo: view.topAnchor),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
            buttons.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 300),
        ])
    }

    func start() {
        refresh()
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.refresh() }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    @objc private func openLog() {
        if let url = Log.fileURL { NSWorkspace.shared.open(url) }
    }

    @objc private func toggleTailscale() {
        var s = AppSettings.load()
        s.allowTailscale = tailscaleBox.state == .on
        s.save()
    }

    @objc private func togglePairing() {
        var s = AppSettings.load()
        s.allowPairing = pairingBox.state == .on
        s.save()
    }

    @objc private func forgetDevices() {
        let alert = NSAlert()
        alert.messageText = "Forget all paired Macs?"
        alert.informativeText = "They'll need to pair again (or sign in with Tailscale) next time they connect."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        role?.host.forgetPairedDevices()
        refresh()
    }

    private func refresh() {
        guard let role else { return }
        let h = role.host
        let s = h.status()
        var lines: [String] = []
        if let error = s.listenerError { lines.append("⚠️ \(error)") }
        if let name = s.clientName {
            lines.append("Streaming to \(name)")
            if let i = s.stream {
                lines.append("  \(i.width)×\(i.height) \(i.codec.name)\(i.lowLatency ? " low latency" : "") · \(i.fps) fps · \(i.bitrateMbps) Mbps")
            }
        } else {
            lines.append("Waiting for a client on port \(h.options.controlPort)")
        }
        if let error = s.lastError { lines.append("⚠️ \(error)") }
        if !s.recoveries.isEmpty {
            lines.append("Auto-recoveries: " + s.recoveries.map { "\($0.key) ×\($0.value)" }.sorted().joined(separator: ", "))
        }
        lines.append("")
        let devices = PairedDevice.store.load()
        lines.append(devices.isEmpty ? "Paired Macs: none yet" : "Paired Macs:")
        let df = DateFormatter()
        df.dateStyle = .medium
        df.timeStyle = .short
        for d in devices.sorted(by: { $0.name < $1.name }) {
            let seen = d.lastSeen.map { " · last seen \(df.string(from: $0))" } ?? ""
            lines.append("  \(d.name) (via \(d.via == "code" ? "pairing code" : d.via == "tailscale" ? "Tailscale" : "old key")\(seen))")
        }
        lines.append("")
        for (name, address) in HostRole.addresses() {
            lines.append("Address:      \(address)\(name.hasPrefix("utun") ? "  (VPN/Tailscale)" : "")")
        }
        lines.append("Old pairing key (only for older bb1 versions): \(h.key.display)")
        lines.append("Screen Recording: \(HostController.hasScreenRecording ? "✓ allowed" : "⚠️ not allowed")")
        lines.append("Accessibility:    \(HostController.hasAccessibility ? "✓ allowed" : "⚠️ not allowed")")
        lines.append("")
        lines.append("Recent log:")
        lines.append(contentsOf: HostPanel.tail(Log.fileURL, lines: 40))
        let atBottom = (text.enclosingScrollView?.verticalScroller?.floatValue ?? 1) > 0.98
        text.string = lines.joined(separator: "\n")
        if atBottom { text.scrollToEndOfDocument(nil) }
    }

    static func tail(_ url: URL?, lines count: Int) -> [String] {
        guard let url, let handle = try? FileHandle(forReadingFrom: url) else { return [] }
        defer { try? handle.close() }
        let size = (try? handle.seekToEnd()) ?? 0
        try? handle.seek(toOffset: size > 16_000 ? size - 16_000 : 0)
        let data = handle.readDataToEndOfFile()
        return Array(String(decoding: data, as: UTF8.self).split(separator: "\n").suffix(count).map(String.init))
    }
}
