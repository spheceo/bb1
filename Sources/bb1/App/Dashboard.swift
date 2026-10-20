import AppKit
import BB1Core
import Network

/// What the dashboard needs from the app.
protocol DashboardDelegate: AnyObject {
    var isHosting: Bool { get }
    var hostRole: HostRole? { get }
    var settings: AppSettings { get }
    func setHosting(_ on: Bool)
    func connect(_ target: ClientTarget)
    func updateSettings(_ change: (inout AppSettings) -> Void)
    var startsAtLogin: Bool { get }
    func setStartsAtLogin(_ on: Bool)
    func refreshDock()
}

/// bb1's main window, laid out like Parsec: an icon sidebar and two pages, Computers and Settings.
final class Dashboard: NSObject, NSWindowDelegate {
    enum Page { case computers, settings }

    let window: NSWindow
    weak var delegate: DashboardDelegate?
    let directory = HostDirectory()
    private let content = NSView()
    private var computersButton: SidebarButton!
    private var settingsButton: SidebarButton!
    private var computers: ComputersPage?
    private var settingsPage: SettingsPage?
    private var page: Page = .computers
    private var timer: Timer?
    var onClosed: (() -> Void)?

    init(delegate: DashboardDelegate) {
        self.delegate = delegate
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 980, height: 660),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                          backing: .buffered, defer: false)
        super.init()
        window.title = "bb1"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.appearance = NSAppearance(named: .darkAqua)
        window.backgroundColor = Style.background
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.minSize = NSSize(width: 640, height: 480)
        window.setFrameAutosaveName("bb1.dashboard.v2")

        let sidebar = NSView()
        computersButton = SidebarButton(symbol: "display.2", tooltip: "Computers", target: self, action: #selector(showComputers))
        settingsButton = SidebarButton(symbol: "gearshape", tooltip: "Settings", target: self, action: #selector(showSettings))
        let icons = NSStackView(views: [computersButton, settingsButton])
        icons.orientation = .vertical
        icons.spacing = 10
        let line = NSBox()
        line.boxType = .custom
        line.fillColor = Style.divider
        line.borderWidth = 0

        let root = NSView()
        for v in [sidebar, icons, line, content] { v.translatesAutoresizingMaskIntoConstraints = false; root.addSubview(v) }
        NSLayoutConstraint.activate([
            sidebar.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            sidebar.topAnchor.constraint(equalTo: root.topAnchor),
            sidebar.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            sidebar.widthAnchor.constraint(equalToConstant: 76),
            icons.centerXAnchor.constraint(equalTo: sidebar.centerXAnchor),
            icons.topAnchor.constraint(equalTo: root.topAnchor, constant: 64),
            line.leadingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            line.topAnchor.constraint(equalTo: root.topAnchor),
            line.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            line.widthAnchor.constraint(equalToConstant: 1),
            content.leadingAnchor.constraint(equalTo: line.trailingAnchor),
            content.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            content.topAnchor.constraint(equalTo: root.topAnchor),
            content.bottomAnchor.constraint(equalTo: root.bottomAnchor),
        ])
        window.contentView = root
        window.center()
        directory.onChange = { [weak self] _ in self?.refresh() }
        directory.start()
        display(ProcessInfo.processInfo.environment["BB1_DASHBOARD_PAGE"] == "settings" ? .settings : .computers)
    }

    func show(_ page: Page? = nil, message: String? = nil) {
        display(page ?? self.page)
        if let message { computers?.setMessage(message) }
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(nil)
        NSApp.activate(ignoringOtherApps: true)
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.refresh() }
    }

    func hide() {
        timer?.invalidate()
        timer = nil
        window.orderOut(nil)
    }

    /// Re-read what's shown (host status, discovered Macs). Settings is only rebuilt on demand.
    func refresh() {
        guard window.isVisible, page == .computers else { return }
        computers?.reload()
    }

    @objc private func showComputers() { display(.computers) }
    @objc private func showSettings() { display(.settings) }

    private func display(_ p: Page) {
        page = p
        computersButton.selected = p == .computers
        settingsButton.selected = p == .settings
        content.subviews.forEach { $0.removeFromSuperview() }
        let view: NSView
        switch p {
        case .computers:
            if computers == nil { computers = ComputersPage(dashboard: self) }
            view = computers!
            computers?.reload()
        case .settings:
            if settingsPage == nil { settingsPage = SettingsPage(dashboard: self) }
            view = settingsPage!
            settingsPage?.reload()
        }
        view.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            view.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            view.topAnchor.constraint(equalTo: content.topAnchor),
            view.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
    }

    func windowWillClose(_ notification: Notification) {
        timer?.invalidate()
        timer = nil
        onClosed?()
    }
}

// MARK: - Computers

final class ComputersPage: NSView {
    private weak var dashboard: Dashboard?
    private let flow = FlowView()
    private let account = AccountChip()
    private let message = Style.label("", size: 12, color: .secondaryLabelColor)
    private var entries: [HostDirectory.Entry] = []
    /// Per Mac: connect over the home network instead of Tailscale (Tailscale is the default).
    private var preferHome: [String: Bool] = UserDefaults.standard.dictionary(forKey: "bb1.preferHome") as? [String: Bool] ?? [:] {
        didSet { UserDefaults.standard.set(preferHome, forKey: "bb1.preferHome") }
    }

    init(dashboard: Dashboard) {
        self.dashboard = dashboard
        super.init(frame: .zero)

        let title = Style.label("Your Macs", size: 30, weight: .bold)
        let add = NSButton(image: NSImage(systemSymbolName: "plus", accessibilityDescription: "Connect by address")!
            .withSymbolConfiguration(.init(pointSize: 15, weight: .semibold))!, target: self, action: #selector(connectByAddress))
        add.isBordered = false
        add.contentTintColor = NSColor.white.withAlphaComponent(0.45)
        add.toolTip = "Connect by address"
        let titleRow = NSStackView(views: [title, add])
        titleRow.spacing = 10
        titleRow.alignment = .centerY
        let subtitle = Style.label("Low-latency desktop, with sound and a shared clipboard.", size: 13, color: .secondaryLabelColor)
        let titles = NSStackView(views: [titleRow, subtitle])
        titles.orientation = .vertical
        titles.alignment = .leading
        titles.spacing = 4
        let header = NSStackView(views: [titles, NSView(), account])
        header.alignment = .top
        header.distribution = .fill

        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let doc = FlippedView()
        doc.translatesAutoresizingMaskIntoConstraints = false
        flow.translatesAutoresizingMaskIntoConstraints = false
        flow.itemSize = NSSize(width: 248, height: 262)
        flow.spacing = 22
        doc.addSubview(flow)
        scroll.documentView = doc
        NSLayoutConstraint.activate([
            flow.leadingAnchor.constraint(equalTo: doc.leadingAnchor),
            flow.trailingAnchor.constraint(equalTo: doc.trailingAnchor),
            flow.topAnchor.constraint(equalTo: doc.topAnchor, constant: 6),
            flow.bottomAnchor.constraint(equalTo: doc.bottomAnchor, constant: -28),
            doc.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])

        let column = NSStackView(views: [header, scroll, message])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 26
        column.translatesAutoresizingMaskIntoConstraints = false
        addSubview(column)
        for v in [header, scroll] as [NSView] { v.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true }
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 44),
            column.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -44),
            column.topAnchor.constraint(equalTo: topAnchor, constant: 54),
            column.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -20),
        ])
        scroll.setContentHuggingPriority(.init(1), for: .vertical)
    }

    required init?(coder: NSCoder) { fatalError() }

    func setMessage(_ text: String) { message.stringValue = text }

    func reload() {
        guard let dashboard, let app = dashboard.delegate else { return }
        entries = dashboard.directory.entries
        if let name = dashboard.directory.tailscaleName {
            account.set(name: name, picture: dashboard.directory.tailscalePicture, connected: true)
        } else {
            account.set(name: Tailscale.isInstalled ? "Tailscale off" : "No Tailscale", picture: nil, connected: false)
        }
        var cards: [NSView] = [thisMacCard(app)]
        for (i, e) in entries.enumerated() { cards.append(card(for: e, index: i)) }
        flow.setItems(cards)
    }

    // MARK: Cards

    private func thisMacCard(_ app: DashboardDelegate) -> NSView {
        let hosting = app.isHosting
        var status = ("Not shared", false)
        var problem = false
        if hosting {
            if let name = app.hostRole?.host.status().clientName { status = ("\(name) is connected", true) }
            else { status = ("Sharing", true) }
            if app.hostRole?.hasProblem == true { status = ("Needs permissions", false); problem = true }
        }
        let button: PillButton
        if problem {
            button = PillButton("Allow Access…", kind: .primary, target: self, action: #selector(openSettings))
        } else if hosting {
            button = PillButton("Stop Sharing", kind: .secondary, target: self, action: #selector(toggleSharing))
        } else {
            button = PillButton("Share This Mac", kind: .primary, target: self, action: #selector(toggleSharing))
        }
        return makeCard(name: "This Mac", detail: DeviceIdentity.name, status: status.0, live: status.1,
                        laptop: DeviceIdentity.name.contains("MacBook"), dimmed: !hosting, menu: nil, button: button)
    }

    private func usesTailscale(_ e: HostDirectory.Entry) -> Bool {
        e.tailscaleIP != nil && (e.lan == nil || preferHome[e.name] != true)
    }

    private func card(for e: HostDirectory.Entry, index: Int) -> NSView {
        let target = HostDirectory.target(for: e, preferTailscale: usesTailscale(e))
        let pairing: Bool = { if case .code? = target?.auth { return true }; return false }()
        let status: String
        if !e.online {
            status = "Offline"
        } else if target?.tailscaleIP != nil {
            status = e.known != nil ? "Online · Tailscale" : "On your Tailscale"
        } else {
            status = e.known != nil ? "Online · Home network" : "Not paired yet"
        }
        let button = PillButton(!e.online ? "Offline" : pairing ? "Pair" : "Connect", kind: .primary,
                                target: self, action: #selector(connectCard(_:)))
        button.tag = index
        button.isEnabled = e.online && target != nil

        let menuButton = NSButton(image: NSImage(systemSymbolName: "ellipsis", accessibilityDescription: "More")!
            .withSymbolConfiguration(.init(pointSize: 13, weight: .semibold))!, target: self, action: #selector(showMenu(_:)))
        menuButton.isBordered = false
        menuButton.contentTintColor = NSColor.white.withAlphaComponent(0.45)
        menuButton.tag = index
        let hasMenu = e.known != nil || (e.lan != nil && e.tailscaleIP != nil)
        return makeCard(name: e.name, detail: nil, status: status, live: e.online, laptop: e.name.contains("MacBook"),
                        dimmed: !e.online, menu: hasMenu ? menuButton : nil, button: button)
    }

    private func makeCard(name: String, detail: String?, status: String, live: Bool, laptop: Bool, dimmed: Bool,
                          menu: NSButton?, button: PillButton) -> NSView {
        let card = CardView(radius: 16)
        let art = NSImageView(image: ComputerArt.image(laptop: laptop, online: !dimmed))
        art.imageScaling = .scaleNone
        let title = Style.label(name, size: 15, weight: .semibold)
        title.allowsDefaultTighteningForTruncation = true
        let statusLine = NSTextField(labelWithAttributedString: {
            let s = NSMutableAttributedString(string: "● ", attributes: [
                .foregroundColor: live ? Style.online : Style.offline, .font: NSFont.systemFont(ofSize: 9)])
            s.append(NSAttributedString(string: status, attributes: [
                .foregroundColor: NSColor.secondaryLabelColor, .font: NSFont.systemFont(ofSize: 12)]))
            return s
        }())
        statusLine.lineBreakMode = .byTruncatingTail
        var views: [NSView] = [art, title, statusLine, button]
        var detailLabel: NSTextField?
        if let detail {
            let d = Style.label(detail, size: 12, color: .tertiaryLabelColor)
            views.append(d)
            detailLabel = d
        }
        if let menu { views.append(menu) }
        for v in views { v.translatesAutoresizingMaskIntoConstraints = false; card.addSubview(v) }
        var c: [NSLayoutConstraint] = [
            art.centerXAnchor.constraint(equalTo: card.centerXAnchor),
            art.topAnchor.constraint(equalTo: card.topAnchor, constant: 30),
            art.widthAnchor.constraint(equalToConstant: ComputerArt.size.width),
            art.heightAnchor.constraint(equalToConstant: ComputerArt.size.height),
            title.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 20),
            title.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -20),
            title.topAnchor.constraint(equalTo: art.bottomAnchor, constant: 22),
            statusLine.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            statusLine.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -20),
            button.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 20),
            button.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -20),
            button.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -20),
            button.heightAnchor.constraint(equalToConstant: 34),
        ]
        if let detailLabel {
            c += [detailLabel.leadingAnchor.constraint(equalTo: title.leadingAnchor),
                  detailLabel.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -20),
                  detailLabel.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 2),
                  statusLine.topAnchor.constraint(equalTo: detailLabel.bottomAnchor, constant: 4)]
        } else {
            c.append(statusLine.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 4))
        }
        if let menu {
            c += [menu.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
                  menu.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -12),
                  menu.widthAnchor.constraint(equalToConstant: 26), menu.heightAnchor.constraint(equalToConstant: 22)]
        }
        NSLayoutConstraint.activate(c)
        return card
    }

    // MARK: Actions

    @objc private func showMenu(_ sender: NSButton) {
        guard sender.tag < entries.count else { return }
        let e = entries[sender.tag]
        let menu = NSMenu()
        if e.lan != nil && e.tailscaleIP != nil {
            let ts = NSMenuItem(title: "Connect over Tailscale", action: #selector(useTailscale(_:)), keyEquivalent: "")
            ts.state = usesTailscale(e) ? .on : .off
            let home = NSMenuItem(title: "Connect over Home Network", action: #selector(useHome(_:)), keyEquivalent: "")
            home.state = usesTailscale(e) ? .off : .on
            for item in [ts, home] { item.target = self; item.tag = sender.tag; menu.addItem(item) }
        }
        if e.known != nil {
            if !menu.items.isEmpty { menu.addItem(.separator()) }
            let forget = NSMenuItem(title: "Forget This Mac", action: #selector(forget(_:)), keyEquivalent: "")
            forget.target = self
            forget.tag = sender.tag
            menu.addItem(forget)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height + 4), in: sender)
    }

    @objc private func useTailscale(_ sender: NSMenuItem) {
        guard sender.tag < entries.count else { return }
        preferHome[entries[sender.tag].name] = false
        reload()
    }

    @objc private func useHome(_ sender: NSMenuItem) {
        guard sender.tag < entries.count else { return }
        preferHome[entries[sender.tag].name] = true
        reload()
    }

    @objc private func forget(_ sender: NSMenuItem) {
        guard sender.tag < entries.count, let host = entries[sender.tag].known else { return }
        KnownHost.store.save(KnownHost.store.load().filter { $0.hostId != host.hostId })
        dashboard?.directory.rebuild()
    }

    @objc private func connectCard(_ sender: NSButton) {
        guard sender.tag < entries.count else { return }
        let e = entries[sender.tag]
        guard let target = HostDirectory.target(for: e, preferTailscale: usesTailscale(e)) else { return }
        if case .code = target.auth { setMessage("A 6-digit code will appear on \(e.name)'s screen.") }
        dashboard?.delegate?.connect(target)
    }

    @objc private func connectByAddress() {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "Connect to a Mac"
        alert.informativeText = "Enter its home-network or Tailscale address."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "192.168.1.20 or 100.x.y.z"
        alert.accessoryView = field
        alert.addButton(withTitle: "Connect")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            var options = ClientOptions()
            options.host = field.stringValue.trimmingCharacters(in: .whitespaces)
            guard let target = options.target() else { self.setMessage("That address doesn't look right."); return }
            self.dashboard?.delegate?.connect(target)
        }
    }

    @objc private func toggleSharing() {
        guard let app = dashboard?.delegate else { return }
        app.setHosting(!app.isHosting)
        reload()
    }

    @objc private func openSettings() { dashboard?.show(.settings) }
}

// MARK: - Settings

final class SettingsPage: NSView {
    private weak var dashboard: Dashboard?
    private let stack = NSStackView()
    private let logText = NSTextView()
    private var switchActions: [ObjectIdentifier: (Bool) -> Void] = [:]

    init(dashboard: Dashboard) {
        self.dashboard = dashboard
        super.init(frame: .zero)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let doc = FlippedView()
        doc.translatesAutoresizingMaskIntoConstraints = false
        doc.addSubview(stack)
        scroll.documentView = doc
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            doc.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            stack.leadingAnchor.constraint(equalTo: doc.leadingAnchor, constant: 44),
            stack.trailingAnchor.constraint(equalTo: doc.trailingAnchor, constant: -44),
            stack.topAnchor.constraint(equalTo: doc.topAnchor, constant: 56),
            stack.bottomAnchor.constraint(equalTo: doc.bottomAnchor, constant: -30),
        ])
        logText.isEditable = false
        logText.drawsBackground = false
        logText.font = .monospacedSystemFont(ofSize: 10.5, weight: .regular)
        logText.textColor = .secondaryLabelColor
    }

    required init?(coder: NSCoder) { fatalError() }

    func reload() {
        guard let app = dashboard?.delegate else { return }
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        switchActions.removeAll()
        let s = app.settings
        add(Style.label("Settings", size: 30, weight: .bold))

        section("This Mac")
        addSwitch("Share this Mac", "Let your other Macs view and control this one.", app.isHosting) { app.setHosting($0) }
        addSwitch("Sign in with Tailscale", "Macs on your Tailscale account connect without pairing.", s.allowTailscale) { on in
            app.updateSettings { $0.allowTailscale = on }
        }
        addSwitch("Pair with a code", "A new Mac pairs once by typing a 6-digit code shown on this screen.", s.allowPairing) { on in
            app.updateSettings { $0.allowPairing = on }
        }
        let devices = PairedDevice.store.load()
        let names = devices.isEmpty ? "None yet" : devices.map(\.name).sorted().joined(separator: ", ")
        addRow("Paired Macs", names, devices.isEmpty ? nil : PillButton("Forget All…", kind: .secondary, target: self, action: #selector(forget)))
        if app.isHosting {
            addRow("Screen Recording", HostController.hasScreenRecording ? "Allowed" : "Needed so your other Macs can see this screen",
                   HostController.hasScreenRecording ? nil : PillButton("Open Settings", kind: .primary, target: self, action: #selector(openScreen)))
            addRow("Accessibility", HostController.hasAccessibility ? "Allowed" : "Needed so your other Macs can control this one",
                   HostController.hasAccessibility ? nil : PillButton("Open Settings", kind: .primary, target: self, action: #selector(openAccessibility)))
            let addresses = HostRole.addresses().map { "\($0.1)\($0.0.hasPrefix("utun") ? " (Tailscale)" : "")" }.joined(separator: "    ")
            addRow("Addresses", addresses, nil)
        }

        section("App")
        let dock = NSPopUpButton(frame: .zero, pullsDown: false)
        dock.addItems(withTitles: DockIcon.allCases.map(\.title))
        dock.selectItem(at: DockIcon.allCases.firstIndex(of: s.dockIcon) ?? 1)
        dock.target = self
        dock.action = #selector(dockChanged(_:))
        addRow("Dock icon", "When bb1 shows in the Dock.", dock)
        addSwitch("Keep running in the menu bar", "Closing windows leaves bb1 running until you choose Quit.", s.keepRunning) { on in
            app.updateSettings { $0.keepRunning = on }
        }
        addSwitch("Open at login", "Start bb1 when you log in, so this Mac can be reached after a restart.", app.startsAtLogin) {
            app.setStartsAtLogin($0)
        }

        section("About")
        addRow("bb1", "Version \(BB1.version)", nil)

        section("Diagnostics")
        addRow("Logs", "Everything both Macs report ends up in the sharing Mac's log.",
               PillButton("Open Logs Folder", kind: .secondary, target: self, action: #selector(openLogs)))
        if app.isHosting {
            logText.string = HostPanel.tail(Log.fileURL, lines: 14).joined(separator: "\n")
            let box = CardView()
            logText.translatesAutoresizingMaskIntoConstraints = false
            box.addSubview(logText)
            NSLayoutConstraint.activate([
                logText.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 12),
                logText.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -12),
                logText.topAnchor.constraint(equalTo: box.topAnchor, constant: 10),
                logText.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -10),
                box.heightAnchor.constraint(equalToConstant: 200),
            ])
            add(box)
            box.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
    }

    // MARK: Rows

    private func add(_ v: NSView) { stack.addArrangedSubview(v) }

    private func section(_ title: String) {
        let spacer = NSView()
        spacer.heightAnchor.constraint(equalToConstant: 10).isActive = true
        add(spacer)
        add(Style.label(title.uppercased(), size: 11, weight: .semibold, color: .tertiaryLabelColor))
    }

    private func addRow(_ title: String, _ detail: String, _ control: NSView?) {
        let t = Style.label(title, size: 13.5, weight: .medium)
        let d = Style.label(detail, size: 12, color: .secondaryLabelColor)
        let text = NSStackView(views: [t, d])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 2
        var views: [NSView] = [text, NSView()]
        if let control { views.append(control) }
        let row = NSStackView(views: views)
        row.distribution = .fill
        row.edgeInsets = NSEdgeInsets(top: 12, left: 16, bottom: 12, right: 16)
        let card = CardView()
        row.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor), row.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            row.topAnchor.constraint(equalTo: card.topAnchor), row.bottomAnchor.constraint(equalTo: card.bottomAnchor),
        ])
        add(card)
        card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }

    private func addSwitch(_ title: String, _ detail: String, _ on: Bool, _ changed: @escaping (Bool) -> Void) {
        let sw = ToggleSwitch()
        sw.isOn = on
        sw.target = self
        sw.action = #selector(switchChanged(_:))
        switchActions[ObjectIdentifier(sw)] = changed
        addRow(title, detail, sw)
    }

    @objc private func switchChanged(_ sender: ToggleSwitch) {
        switchActions[ObjectIdentifier(sender)]?(sender.isOn)
        DispatchQueue.main.async { self.reload() }
    }

    @objc private func dockChanged(_ sender: NSPopUpButton) {
        let option = DockIcon.allCases[sender.indexOfSelectedItem]
        dashboard?.delegate?.updateSettings { $0.dockIcon = option }
        dashboard?.delegate?.refreshDock()
    }

    @objc private func forget() {
        let alert = NSAlert()
        alert.messageText = "Forget all paired Macs?"
        alert.informativeText = "They'll need to pair again (or sign in with Tailscale) next time they connect."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        PairedDevice.store.save([])
        Log.info("host: forgot all paired devices")
        reload()
    }

    @objc private func openScreen() {
        CGRequestScreenCaptureAccess()
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!)
    }

    @objc private func openAccessibility() {
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!)
    }

    @objc private func openLogs() {
        NSWorkspace.shared.open(FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/bb1"))
    }
}

final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
