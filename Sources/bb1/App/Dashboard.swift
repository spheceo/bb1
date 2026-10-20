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
    func removeFromThisMac()
}

/// bb1's main window: Your Macs (live screens of your other Macs), with Settings behind the gear.
final class Dashboard: NSObject, NSWindowDelegate {
    enum Page { case computers, settings }

    let window: NSWindow
    weak var delegate: DashboardDelegate?
    let directory = HostDirectory()
    private let content = NSView()
    private var computers: ComputersPage?
    private var settingsPage: SettingsPage?
    private var page: Page = .computers
    private var timer: Timer?
    var onClosed: (() -> Void)?

    init(delegate: DashboardDelegate) {
        self.delegate = delegate
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 980, height: 680),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                          backing: .buffered, defer: false)
        super.init()
        window.title = "bb1"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        // An empty toolbar makes the title bar taller, so the window buttons (and the header that lines up
        // with them) sit in a roomy strip instead of against the top edge.
        let toolbar = NSToolbar(identifier: "bb1.dashboard")
        toolbar.showsBaselineSeparator = false
        window.toolbar = toolbar
        window.toolbarStyle = .unified
        window.appearance = NSAppearance(named: .darkAqua)
        window.backgroundColor = Style.background
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.minSize = NSSize(width: 640, height: 480)
        window.setFrameAutosaveName("bb1.dashboard.v3")
        window.contentView = content
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
        Updater.shared.checkIfStale()
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.refresh() }
    }

    func hide() {
        timer?.invalidate()
        timer = nil
        HostPreviews.shared.stopAll()
        window.orderOut(nil)
    }

    /// Re-read what's shown (host status, discovered Macs). Settings is only rebuilt on demand.
    func refresh() {
        guard window.isVisible, page == .computers else { return }
        computers?.reload()
    }

    func showComputers() { display(.computers) }
    func showSettings() { display(.settings) }

    private func display(_ p: Page) {
        page = p
        if p != .computers { HostPreviews.shared.stopAll() }
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

    // MARK: Updating

    /// Other Macs being updated as part of `updateEverything` (this Mac goes last).
    private(set) var updatingHosts: Set<String> = []
    private(set) var pendingRelease: AppRelease?

    /// Update your other Macs that are behind (each downloads the build itself), then this one.
    func updateEverything(_ release: AppRelease) {
        guard !Updater.shared.state.busy, updatingHosts.isEmpty else { return }
        let targets = hostsBehind(release)
        guard !targets.isEmpty else {
            if release.build > BB1.build { Updater.shared.install(release) }
            return
        }
        pendingRelease = release
        updatingHosts = Set(targets.map(\.label))
        for t in targets {
            HostUpdates.shared.update(t, to: release.build) { [weak self] _ in
                guard let self else { return }
                self.updatingHosts.remove(t.label)
                guard self.updatingHosts.isEmpty else { return }
                self.pendingRelease = nil
                if release.build > BB1.build { Updater.shared.install(release) } else { self.refresh() }
            }
        }
        refresh()
    }

    /// Online Macs on an older build that can be updated from here (and aren't updating already).
    func hostsBehind(_ release: AppRelease) -> [ClientTarget] {
        directory.entries.compactMap { e in
            guard e.online, let build = HostUpdates.shared.builds[e.name], build < release.build,
                  HostUpdates.shared.phases[e.name].map({ $0.finished }) ?? true,
                  let target = ComputersPage.target(for: e), HostUpdates.canUpdate(target, build: build) else { return nil }
            return target
        }
    }

    func windowWillClose(_ notification: Notification) {
        timer?.invalidate()
        timer = nil
        HostPreviews.shared.stopAll()
        onClosed?()
    }
}

// MARK: - Computers

final class ComputersPage: NSView {
    private weak var dashboard: Dashboard?
    private let flow = FlowView()
    private let avatar = AvatarView()
    private lazy var pill = UpdatePill(target: self, action: #selector(pillClicked))
    private let message = Style.label("", size: 12, color: .secondaryLabelColor)
    private var entries: [HostDirectory.Entry] = []
    private var tiles: [String: MacTile] = [:]
    private let thisMac = ThisMacTile()
    /// Header items line up with the window buttons; set in `layout()` once those exist.
    private var brandCenter: NSLayoutConstraint!
    private var brandLeading: NSLayoutConstraint!
    private var cornerCenter: NSLayoutConstraint!
    /// Per Mac: connect over the home network instead of Tailscale (Tailscale is the default).
    private static var preferHome: [String: Bool] {
        get { UserDefaults.standard.dictionary(forKey: "bb1.preferHome") as? [String: Bool] ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "bb1.preferHome") }
    }

    init(dashboard: Dashboard) {
        self.dashboard = dashboard
        super.init(frame: .zero)

        // Header, in the title bar row: the brand after the window buttons; update, settings and account on the right.
        let brandMark = NSImageView(image: NSImage(size: NSSize(width: 18, height: 14), flipped: false) { rect in
            ComputerArt.drawMark(in: rect.insetBy(dx: 0.5, dy: 0.5), opacity: 1)
            return true
        })
        let brandName = Style.label("bb1", size: 14, weight: .bold)
        let brand = NSStackView(views: [brandMark, brandName])
        brand.spacing = 7
        let gear = IconButton(symbol: "gearshape", tooltip: "Settings", target: self, action: #selector(openSettings))
        let corner = NSStackView(views: [pill, gear, avatar])
        corner.spacing = 10
        corner.alignment = .centerY
        corner.detachesHiddenViews = true
        pill.isHidden = true

        let title = NSTextField(labelWithAttributedString: NSAttributedString(string: "Your Macs", attributes: [
            .font: NSFont.systemFont(ofSize: 30, weight: .bold), .kern: -0.6, .foregroundColor: NSColor.white]))
        let add = NSButton(image: NSImage(systemSymbolName: "plus", accessibilityDescription: "Connect by address")!
            .withSymbolConfiguration(.init(pointSize: 15, weight: .semibold))!, target: self, action: #selector(connectByAddress))
        add.isBordered = false
        add.contentTintColor = NSColor.white.withAlphaComponent(0.4)
        add.toolTip = "Connect by address"
        let titleRow = NSStackView(views: [title, add])
        titleRow.spacing = 10
        titleRow.alignment = .centerY

        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let doc = FlippedView()
        doc.translatesAutoresizingMaskIntoConstraints = false
        flow.translatesAutoresizingMaskIntoConstraints = false
        flow.spacing = 20
        // Big screens: two across in a normal window, one when narrow, three when wide.
        flow.adaptive = { width in
            let gap: CGFloat = 20
            let columns = max(1, min(3, Int((width + gap) / (340 + gap))))
            let w = floor((width - CGFloat(columns - 1) * gap) / CGFloat(columns))
            return (columns, NSSize(width: w, height: floor(w * 10 / 16) + MacTile.footerHeight))
        }
        doc.addSubview(flow)
        scroll.documentView = doc
        NSLayoutConstraint.activate([
            flow.leadingAnchor.constraint(equalTo: doc.leadingAnchor),
            flow.trailingAnchor.constraint(equalTo: doc.trailingAnchor),
            flow.topAnchor.constraint(equalTo: doc.topAnchor, constant: 4),
            flow.bottomAnchor.constraint(equalTo: doc.bottomAnchor, constant: -28),
            doc.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])

        for v in [brand, corner, titleRow, scroll, message] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        brandLeading = brand.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 86)
        brandCenter = brand.centerYAnchor.constraint(equalTo: topAnchor, constant: 20)
        cornerCenter = corner.centerYAnchor.constraint(equalTo: topAnchor, constant: 20)
        NSLayoutConstraint.activate([
            brandLeading, brandCenter, cornerCenter,
            corner.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            titleRow.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 40),
            titleRow.topAnchor.constraint(equalTo: topAnchor, constant: 70),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 40),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -40),
            scroll.topAnchor.constraint(equalTo: titleRow.bottomAnchor, constant: 24),
            scroll.bottomAnchor.constraint(equalTo: message.topAnchor, constant: -8),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 40),
            message.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -40),
            message.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -14),
        ])

        thisMac.onToggle = { [weak self] on in
            guard let app = self?.dashboard?.delegate else { return }
            app.setHosting(on)
            self?.reload()
        }
        thisMac.onFix = { [weak self] in self?.dashboard?.showSettings() }
        for name in [Updater.changed, HostUpdates.changed, HostPreviews.changed] {
            NotificationCenter.default.addObserver(self, selector: #selector(updatesChanged), name: name, object: nil)
        }
    }

    @objc private func updatesChanged() { reload() }

    required init?(coder: NSCoder) { fatalError() }

    override func layout() {
        if let m = TitleBar.metrics(in: self) {
            let center = round(m.centerFromTop)
            if brandCenter.constant != center { brandCenter.constant = center; cornerCenter.constant = center }
            let leading = round(m.afterButtons + 16)
            if brandLeading.constant != leading { brandLeading.constant = leading }
        }
        super.layout()
    }

    func setMessage(_ text: String) { message.stringValue = text }

    func reload() {
        guard let dashboard, let app = dashboard.delegate else { return }
        entries = dashboard.directory.entries
        avatar.set(name: dashboard.directory.tailscaleName, picture: dashboard.directory.tailscalePicture)

        var views: [NSView] = []
        var previewTargets: [ClientTarget] = []
        for (i, e) in entries.enumerated() {
            let tile = tiles[e.name] ?? MacTile()
            tiles[e.name] = tile
            tile.configure(model(for: e))
            tile.onClick = { [weak self] in self?.connect(index: i) }
            tile.onBadge = { [weak self] in self?.updateHost(index: i) }
            tile.makeMenu = { [weak self] in self?.menu(index: i) }
            views.append(tile)
            if e.online, let target = ComputersPage.target(for: e),
               HostPreviews.canPreview(target, build: HostUpdates.shared.builds[e.name]) { previewTargets.append(target) }
        }
        tiles = tiles.filter { name, _ in entries.contains { $0.name == name } }
        configureThisMac(app)
        views.append(thisMac)
        flow.setItems(views)
        if window?.isVisible == true { HostPreviews.shared.keepFresh(previewTargets) }
        for e in entries where e.online {
            if let target = ComputersPage.target(for: e) { HostUpdates.shared.refresh(target) }
        }
        reloadPill()
    }

    // MARK: Update pill

    private func reloadPill() {
        guard let dashboard else { return }
        let updater = Updater.shared
        if updater.justUpdated == nil { pill.alphaValue = 1 }
        switch updater.state {
        case .downloading(_, let p): pill.show(.busy(p), "Updating · \(Int(p * 100))%"); return
        case .installing: pill.show(.busy(1), "Installing…"); return
        case .restarting: pill.show(.busy(1), "Restarting bb1…"); return
        default: break
        }
        if !dashboard.updatingHosts.isEmpty {
            let names = dashboard.updatingHosts.sorted()
            let progress = names.map { name -> Double in
                switch HostUpdates.shared.phases[name] {
                case .downloading(let p)?: return p * 0.8
                case .installing?, .restarting?: return 0.85
                case .waiting?: return 0.95
                case .done?, .failed?: return 1
                default: return 0
                }
            }.reduce(0, +) / Double(names.count)
            pill.show(.busy(progress), "Updating \(ComputersPage.list(names)) · \(Int(progress * 100))%",
                      tooltip: "Your other Macs update first, then this one.")
            return
        }
        if let r = updater.latest, r.build > BB1.build {
            if case .failed(let message) = updater.state {
                pill.show(.failed, "Update failed · Try again", tooltip: message)
                return
            }
            let behind = dashboard.hostsBehind(r).map(\.label)
            let also = behind.isEmpty ? "" : " Also updates \(ComputersPage.list(behind))."
            pill.show(.offer, "Update to \(r.shortName)", tooltip: "You have build \(BB1.build).\(also)")
            return
        }
        if let u = updater.justUpdated {
            pill.show(.done, "Updated to build \(u.to)", tooltip: "Updated from build \(u.from).")
            fadeOutDonePill()
            return
        }
        pill.isHidden = true
    }

    private var doneFadeScheduled = false

    /// "Updated to build N" stays for a few seconds once you can see it, then fades away.
    private func fadeOutDonePill() {
        guard !doneFadeScheduled, window?.isVisible == true else { return }
        doneFadeScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { [weak self] in
            guard let self, Updater.shared.justUpdated != nil else { return }
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.6
                self.pill.animator().alphaValue = 0
            }, completionHandler: {
                Updater.shared.dismissJustUpdated()
            })
        }
    }

    @objc private func pillClicked() {
        let updater = Updater.shared
        if updater.state.busy || !(dashboard?.updatingHosts.isEmpty ?? true) { return }
        if let r = updater.latest, r.build > BB1.build {
            dashboard?.updateEverything(r)
        } else if updater.justUpdated != nil {
            updater.dismissJustUpdated()
        }
    }

    /// "A", "A and B", "A, B and C"
    static func list(_ names: [String]) -> String {
        names.count <= 1 ? names.joined() : names.dropLast().joined(separator: ", ") + " and " + names.last!
    }

    // MARK: Tiles

    private func configureThisMac(_ app: DashboardDelegate) {
        var status = "Not shared"
        var live = false
        var problem = false
        if app.isHosting {
            if app.hostRole?.hasProblem == true {
                status = "Needs permissions"
                problem = true
            } else if let client = app.hostRole?.host.status().clientName {
                status = "\(client) is connected"
                live = true
            } else {
                status = app.startsAtLogin ? "Shared · starts at login" : "Shared"
                live = true
            }
        }
        thisMac.configure(shared: app.isHosting, status: status, live: live, problem: problem)
        thisMac.toolTip = "\(DeviceIdentity.name) · build \(BB1.build)"
    }

    private func model(for e: HostDirectory.Entry) -> MacTile.Model {
        let target = ComputersPage.target(for: e)
        let pairing: Bool = { if case .code? = target?.auth { return true }; return false }()
        let phase = HostUpdates.shared.phases[e.name]
        let preview = HostPreviews.shared.preview(for: e.name)
        var status: String
        var live = e.online
        var progress: Double?
        var tooltip: String?
        switch phase {
        case .starting?: status = "Getting ready to update…"
        case .downloading(let p)?: status = "Updating · \(Int(p * 100))%"; progress = p
        case .installing?: status = "Installing update…"; progress = 1
        case .restarting?: status = "Restarting…"; progress = 1
        case .waiting?: status = "Starting up…"; progress = 1
        case .done(let build)?: status = "Updated to build \(build)"
        case .failed(let why)?: status = "Update didn't finish"; live = false; tooltip = why
        case nil:
            if !e.online {
                status = preview.map { "Offline · seen \(ComputersPage.ago($0.at))" } ?? "Offline"
            } else if pairing {
                status = "Online · needs pairing"
            } else if let ms = HostPreviews.shared.latencyMs[e.name] {
                status = "Online · \(Int(ms.rounded())) ms"
            } else {
                status = "Online"
            }
        }
        let updating = phase.map { !$0.finished } ?? false
        var badge: String?
        if !updating, e.online, let latest = Updater.shared.latest, let target,
           let build = HostUpdates.shared.builds[e.name], build < latest.build, HostUpdates.canUpdate(target, build: build) {
            badge = phase == nil ? "Update" : "Retry update"
            tooltip = tooltip ?? "\(e.name) has build \(build). Update it to \(latest.title.lowercasedFirst)."
        }
        return MacTile.Model(name: e.name, status: status, live: live, image: preview?.image, dim: !e.online,
                             action: e.online && target != nil && !updating ? (pairing ? "Pair" : "Connect") : nil,
                             progress: progress, badge: badge, tooltip: tooltip)
    }

    /// "just now", "5 min ago", "2 h ago", "3 days ago"
    static func ago(_ date: Date) -> String {
        let s = Date().timeIntervalSince(date)
        switch s {
        case ..<60: return "just now"
        case ..<3600: return "\(Int(s / 60)) min ago"
        case ..<86_400: return "\(Int(s / 3600)) h ago"
        default: let d = Int(s / 86_400); return d == 1 ? "yesterday" : "\(d) days ago"
        }
    }

    private static func usesTailscale(_ e: HostDirectory.Entry) -> Bool {
        e.tailscaleIP != nil && (e.lan == nil || preferHome[e.name] != true)
    }

    /// How to reach this Mac, honouring the Tailscale / home network choice.
    static func target(for e: HostDirectory.Entry) -> ClientTarget? {
        HostDirectory.target(for: e, preferTailscale: ComputersPage.usesTailscale(e))
    }

    private func menu(index: Int) -> NSMenu? {
        guard index < entries.count else { return nil }
        let e = entries[index]
        let menu = NSMenu()
        if e.lan != nil && e.tailscaleIP != nil {
            let ts = NSMenuItem(title: "Connect over Tailscale", action: #selector(useTailscale(_:)), keyEquivalent: "")
            ts.state = ComputersPage.usesTailscale(e) ? .on : .off
            let home = NSMenuItem(title: "Connect over Home Network", action: #selector(useHome(_:)), keyEquivalent: "")
            home.state = ComputersPage.usesTailscale(e) ? .off : .on
            for item in [ts, home] { item.target = self; item.tag = index; menu.addItem(item) }
        }
        if e.known != nil {
            if !menu.items.isEmpty { menu.addItem(.separator()) }
            let forget = NSMenuItem(title: "Forget This Mac", action: #selector(forget(_:)), keyEquivalent: "")
            forget.target = self
            forget.tag = index
            menu.addItem(forget)
        }
        return menu.items.isEmpty ? nil : menu
    }

    // MARK: Actions

    @objc private func useTailscale(_ sender: NSMenuItem) {
        guard sender.tag < entries.count else { return }
        ComputersPage.preferHome[entries[sender.tag].name] = false
        reload()
    }

    @objc private func useHome(_ sender: NSMenuItem) {
        guard sender.tag < entries.count else { return }
        ComputersPage.preferHome[entries[sender.tag].name] = true
        reload()
    }

    @objc private func forget(_ sender: NSMenuItem) {
        guard sender.tag < entries.count, let host = entries[sender.tag].known else { return }
        KnownHost.store.save(KnownHost.store.load().filter { $0.hostId != host.hostId })
        dashboard?.directory.rebuild()
    }

    private func updateHost(index: Int) {
        guard index < entries.count, let latest = Updater.shared.latest,
              let target = ComputersPage.target(for: entries[index]) else { return }
        HostUpdates.shared.update(target, to: latest.build)
    }

    private func connect(index: Int) {
        guard index < entries.count else { return }
        let e = entries[index]
        guard let target = ComputersPage.target(for: e) else { return }
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
    private var backLeading: NSLayoutConstraint!
    private var backCenter: NSLayoutConstraint!

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
        // Lines up with "Your Macs" (no extra inset for the title bar).
        scroll.automaticallyAdjustsContentInsets = false
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
            stack.leadingAnchor.constraint(equalTo: doc.leadingAnchor, constant: 40),
            stack.trailingAnchor.constraint(equalTo: doc.trailingAnchor, constant: -40),
            stack.topAnchor.constraint(equalTo: doc.topAnchor, constant: 70),
            stack.bottomAnchor.constraint(equalTo: doc.bottomAnchor, constant: -30),
        ])
        // Back to Your Macs, in the title bar row where the brand sits on the other page.
        let back = PillButton("Your Macs", kind: .link, target: self, action: #selector(goBack))
        back.image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .bold))
        back.imagePosition = .imageLeading
        back.imageHugsTitle = true
        back.contentTintColor = .white
        back.translatesAutoresizingMaskIntoConstraints = false
        addSubview(back)
        backLeading = back.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 86)
        backCenter = back.centerYAnchor.constraint(equalTo: topAnchor, constant: 20)
        NSLayoutConstraint.activate([backLeading, backCenter])
        logText.isEditable = false
        logText.drawsBackground = false
        logText.font = .monospacedSystemFont(ofSize: 10.5, weight: .regular)
        logText.textColor = .secondaryLabelColor
        NotificationCenter.default.addObserver(self, selector: #selector(updatesChanged), name: Updater.changed, object: nil)
    }

    @objc private func updatesChanged() {
        guard window != nil else { return }
        reload()
    }

    @objc private func removeApp() {
        let alert = NSAlert()
        alert.messageText = "Remove bb1 from this Mac?"
        alert.informativeText = "bb1 quits, stops starting at login, and moves to the Trash. Your other Macs can't connect to this one until you install it again."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        dashboard?.delegate?.removeFromThisMac()
    }

    @objc private func goBack() { dashboard?.showComputers() }

    override func layout() {
        if let m = TitleBar.metrics(in: self) {
            backCenter.constant = round(m.centerFromTop)
            backLeading.constant = round(m.afterButtons + 16)
        }
        super.layout()
    }

    @objc private func checkForUpdates() { Updater.shared.check(userInitiated: true) }

    @objc private func updateNow() {
        guard let r = Updater.shared.latest else { return }
        dashboard?.show(.computers)
        dashboard?.updateEverything(r)
    }

    required init?(coder: NSCoder) { fatalError() }

    func reload() {
        guard let app = dashboard?.delegate else { return }
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        switchActions.removeAll()
        let s = app.settings
        add(NSTextField(labelWithAttributedString: NSAttributedString(string: "Settings", attributes: [
            .font: NSFont.systemFont(ofSize: 30, weight: .bold), .kern: -0.6, .foregroundColor: NSColor.white])))

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

        if Installer.isInstalledApp {
            addRow("Remove bb1", "Quit, stop starting at login and move the app to the Trash. Pairings are kept.",
                   PillButton("Remove…", kind: .secondary, target: self, action: #selector(removeApp)))
        }

        section("Updates")
        let updater = Updater.shared
        var detail = "Version \(BB1.version) · build \(BB1.build)"
        var control: NSView? = PillButton("Check for Updates", kind: .secondary, target: self, action: #selector(checkForUpdates))
        switch updater.state {
        case .idle: break
        case .checking: detail += " · Checking…"; control = nil
        case .upToDate: detail += " · Up to date"
        case .available(let r):
            detail += " · \(r.title) is available"
            control = PillButton("Update", kind: .primary, target: self, action: #selector(updateNow))
        case .downloading(_, let p): detail += " · Downloading update · \(Int(p * 100))%"; control = nil
        case .installing, .restarting: detail += " · Installing update…"; control = nil
        case .failed(let message): detail += " · \(message)"
        }
        if !updater.canUpdate { detail = "Version \(BB1.version) · development copy (updates work in the installed app)"; control = nil }
        addRow("bb1", detail, control)
        addSwitch("Include test builds", "Offer every test build as an update, not only releases.", s.testBuilds) { on in
            app.updateSettings { $0.testBuilds = on }
            Updater.shared.check()
        }

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
