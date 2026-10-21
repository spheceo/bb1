import AppKit
import BB1Core

enum AppMode: String, Codable {
    case host, client
}

enum DockIcon: String, Codable, CaseIterable {
    case always, whileWindowOpen, never

    var title: String {
        switch self {
        case .always: return "Always"
        case .whileWindowOpen: return "Only While a Window Is Open"
        case .never: return "Never (Menu Bar Only)"
        }
    }
}

/// ~/Library/Application Support/bb1/settings.json
struct AppSettings: Codable {
    var mode: AppMode?
    var dockIcon: DockIcon = .whileWindowOpen
    /// Closing windows leaves bb1 running in the menu bar until Quit.
    var keepRunning = true
    /// Host: devices on this Mac's Tailscale account can connect without pairing.
    var allowTailscale = true
    /// Host: new Macs can pair with a 6-digit code shown on this screen.
    var allowPairing = true
    /// Share this Mac (run the host). nil = decide from the older `mode` setting.
    var hosting: Bool?
    /// Update channel: true = Nightly (GitHub pre-releases only), false = Stable (releases only).
    var nightly = true

    enum CodingKeys: String, CodingKey {
        case mode, dockIcon, keepRunning, allowTailscale, allowPairing, hosting
        case nightly = "testBuilds" // its name before nightlies, kept so the choice carries over
    }

    var isHosting: Bool { hosting ?? (mode == .host) }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        mode = try c.decodeIfPresent(AppMode.self, forKey: .mode)
        dockIcon = try c.decodeIfPresent(DockIcon.self, forKey: .dockIcon) ?? .whileWindowOpen
        keepRunning = try c.decodeIfPresent(Bool.self, forKey: .keepRunning) ?? true
        allowTailscale = try c.decodeIfPresent(Bool.self, forKey: .allowTailscale) ?? true
        allowPairing = try c.decodeIfPresent(Bool.self, forKey: .allowPairing) ?? true
        hosting = try c.decodeIfPresent(Bool.self, forKey: .hosting)
        nightly = try c.decodeIfPresent(Bool.self, forKey: .nightly) ?? true
    }

    private static var url: URL { BB1.supportDirectory().appendingPathComponent("settings.json") }

    static func load() -> AppSettings {
        guard let data = try? Data(contentsOf: url), let s = try? JSONDecoder().decode(AppSettings.self, from: data) else {
            return AppSettings()
        }
        return s
    }

    func save() {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .prettyPrinted
        try? encoder.encode(self).write(to: AppSettings.url, options: .atomic)
    }
}

/// One app. It can share this Mac (host) and connect to other Macs (client) at the same time,
/// like Parsec. The menu bar icon is always there; the Dock icon follows the setting.
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate, DashboardDelegate {
    private(set) var settings = AppSettings.load()
    private let forcedMode: AppMode?
    private let hostOptions: HostOptions
    private let clientOptions: ClientOptions
    private var statusItem: NSStatusItem!
    private(set) var hostRole: HostRole?
    private var clientRole: ClientRole!
    private var dashboard: Dashboard!
    private weak var streamMenu: NSMenu?
    /// The menu bar menu while it's open, so news (a new build) shows up without reopening it.
    private weak var openStatusMenu: NSMenu?

    init(forcedMode: AppMode?, hostOptions: HostOptions, clientOptions: ClientOptions) {
        self.forcedMode = forcedMode
        self.hostOptions = hostOptions
        self.clientOptions = clientOptions
    }

    /// Started by launchd at login (no window then), or opened by the user.
    private var launchedAtLogin: Bool { Installer.launchedByLaunchd }

    var isHosting: Bool { hostRole != nil }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let hosting = forcedMode.map { $0 == .host } ?? settings.isHosting
        Log.setup(hosting ? "host" : "client")
        Log.info("app: starting bb1 v\(BB1.version) (build \(BB1.build))\(hosting ? ", sharing this Mac" : "")")
        if Installer.offerMoveIfNeeded() { return }
        Installer.takeOver()
        buildMainMenu()
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu

        // The menu bar icon shows update progress (this Mac's and your other Macs').
        for name in [Updater.changed, HostUpdates.changed] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                guard let self else { return }
                self.updateIcon()
                if let menu = self.openStatusMenu { self.menuNeedsUpdate(menu) }
            }
        }
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.willCloseNotification, NSWindow.didMiniaturizeNotification] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { self?.updateDock() }
            }
        }

        dashboard = Dashboard(delegate: self)
        let role = ClientRole(options: clientOptions)
        role.onChange = { [weak self] in self?.updateIcon(); self?.dashboard.refresh() }
        role.keepRunning = { [weak self] in self?.settings.keepRunning ?? true }
        role.showDashboard = { [weak self] message in self?.dashboard.show(.computers, message: message) }
        role.hideDashboard = { [weak self] in self?.dashboard.hide() }
        clientRole = role
        dashboard.onClosed = { [weak self] in self?.clientRole.windowsClosed() }

        if hosting { startHost() }
        Updater.shared.windowOpen = { [weak self] in self?.dashboard.window.isVisible ?? false }
        Updater.shared.start()
        if Updater.shared.justUpdated != nil { celebrateUpdate() }
        // After an update, come back the way it was (window open or just the menu bar icon).
        let showWindow = Updater.shared.justUpdated?.windowOpen ?? !launchedAtLogin
        if clientOptions.host != nil {
            role.start()
        } else if showWindow {
            dashboard.show()
        }
        updateIcon()
        updateDock()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { dashboard.show() }
        return true
    }

    // MARK: DashboardDelegate

    func updateSettings(_ change: (inout AppSettings) -> Void) {
        var fresh = AppSettings.load()
        change(&fresh)
        fresh.save()
        settings = fresh
    }

    func connect(_ target: ClientTarget) { clientRole.connect(target) }

    func refreshDock() { updateDock() }

    func setHosting(_ on: Bool) {
        if on == isHosting { return }
        if on {
            updateSettings { $0.hosting = true }
            // A shared Mac has to come back after a restart (and after a crash).
            if Installer.isInstalledApp && !LoginItem.isOn { LoginItem.set(true) }
            Log.setup("host")
            startHost()
        } else {
            if hostRole?.hasClient == true {
                let alert = NSAlert()
                alert.messageText = "Stop sharing this Mac?"
                alert.informativeText = "Another Mac is connected right now. It will be disconnected, and this Mac can't be reached until you share it again."
                alert.addButton(withTitle: "Stop Sharing")
                alert.addButton(withTitle: "Cancel")
                NSApp.activate(ignoringOtherApps: true)
                guard alert.runModal() == .alertFirstButtonReturn else { return }
            }
            updateSettings { $0.hosting = false }
            hostRole?.stop()
            hostRole = nil
            Log.setup("client")
        }
        updateIcon()
        dashboard.refresh()
    }

    private func startHost() {
        let role = HostRole(options: hostOptions)
        role.onChange = { [weak self] in self?.updateIcon(); self?.dashboard.refresh() }
        role.showDashboard = { [weak self] in self?.dashboard.show(.settings) }
        hostRole = role
        role.start()
    }

    var startsAtLogin: Bool { LoginItem.isOn }

    func setStartsAtLogin(_ on: Bool) { LoginItem.set(on) }

    // MARK: Menu bar

    /// Plays once after bb1 restarts into a new build (see `celebrateUpdate`).
    private var celebration: Timer?

    private func updateIcon() {
        guard let button = statusItem?.button, celebration == nil else { return }
        button.toolTip = nil
        if let progress = dashboard?.updateProgress {
            // One bar for the whole update: your other Macs first, then this one.
            button.image = MenuBarIcon.progress(progress)
            button.appearsDisabled = false
            button.toolTip = "Updating · \(Int(progress * 100))%"
        } else if hostRole?.hasProblem == true {
            button.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: "bb1 needs attention")
            button.appearsDisabled = false
        } else {
            button.image = MenuBarIcon.image
            // Full strength while sharing or connected; dimmed when idle.
            button.appearsDisabled = !(isHosting || clientRole?.isActive == true)
        }
        button.title = ""
    }

    /// Just updated: the full progress bar becomes a checkmark, holds, then the usual mark comes back.
    private func celebrateUpdate() {
        guard let button = statusItem?.button else { return }
        let start = Date()
        func ease(_ t: Double) -> Double { let c = min(max(t, 0), 1); return c * c * (3 - 2 * c) }
        func phase(_ t: Double, _ from: Double, _ to: Double) -> Double { ease((t - from) / (to - from)) }
        button.appearsDisabled = false
        button.toolTip = "Updated to \(BB1.displayName)"
        celebration = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { [weak self] timer in
            let t = Date().timeIntervalSince(start)
            button.image = MenuBarIcon.updated(bar: 1 - phase(t, 0.3, 0.6), check: phase(t, 0.6, 1.05),
                                               checkOpacity: 1 - phase(t, 2.6, 3.0), half: phase(t, 2.7, 3.1))
            guard t >= 3.1 else { return }
            timer.invalidate()
            self?.celebration = nil
            self?.updateIcon()
        }
    }

    func menuWillOpen(_ menu: NSMenu) {
        guard menu === statusItem?.menu else { return }
        openStatusMenu = menu
        // Opening the menu is a good moment to look for a new build (at most once a minute).
        Updater.shared.checkIfStale()
    }

    func menuDidClose(_ menu: NSMenu) {
        if menu === openStatusMenu { openStatusMenu = nil }
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === streamMenu {
            clientRole.fillStreamMenu(menu)
            return
        }
        menu.removeAllItems()
        // Every item that does something has an icon; status lines don't.
        @discardableResult
        func action(_ title: String, _ symbol: String, _ selector: Selector?, key: String = "") -> NSMenuItem {
            let item = NSMenuItem(title: title, action: selector, keyEquivalent: key)
            item.target = self
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
            menu.addItem(item)
            return item
        }
        func info(_ title: String) {
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        }
        let open = action("Open bb1", "macwindow", #selector(openDashboard))
        open.attributedTitle = NSAttributedString(string: "Open bb1", attributes: [.font: NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)])
        switch Updater.shared.state {
        case .available(let r) where dashboard.updatingHosts.isEmpty:
            action("Update to \(r.title.lowercasedFirst)", "arrow.down.circle", #selector(updateFromMenu))
        case .downloading(let r, let p):
            info("Updating to \(r.shortName) · \(Int(p * 100))%")
        case .installing(let r), .restarting(let r):
            info("Installing \(r.shortName)…")
        default:
            if let r = dashboard.pendingRelease { info("Updating your other Macs to \(r.shortName)…") }
        }
        menu.addItem(.separator())
        // Sharing and connection status only when there's something to say (Open bb1 covers connecting).
        let before = menu.numberOfItems
        hostRole?.addItems(to: menu)
        clientRole.addItems(to: menu)
        if menu.numberOfItems > before { menu.addItem(.separator()) }

        action("Share This Mac", "dot.radiowaves.left.and.right", #selector(toggleSharing)).state = isHosting ? .on : .off
        action("Settings…", "gearshape", #selector(openSettings), key: ",")
        if let hostRole {
            let more = NSMenu()
            more.addItem(hostRole.faultMenuItem())
            action("Diagnostics", "stethoscope", nil).submenu = more
        }
        menu.addItem(.separator())
        info("\(BB1.displayName)\(Updater.shared.justUpdated != nil ? " · just updated" : "")")
        action("Quit bb1", "xmark.square", #selector(NSApplication.terminate(_:)), key: "q").target = nil
    }

    @objc private func openDashboard() { dashboard.show(.computers) }
    /// Update from the menu bar without opening the window (your other Macs first, then this one).
    @objc private func updateFromMenu() {
        guard case .available(let r) = Updater.shared.state else { return }
        dashboard.updateEverything(r)
    }
    @objc private func openSettings() { dashboard.show(.settings) }
    @objc private func toggleSharing() { setHosting(!isHosting) }

    // MARK: Dock icon

    private func updateDock() {
        let windowOpen = NSApp.windows.contains { $0.isVisible && !$0.isMiniaturized && $0.styleMask.contains(.titled) }
        let wantDock: Bool
        switch settings.dockIcon {
        case .always: wantDock = true
        case .whileWindowOpen: wantDock = windowOpen || NSApp.windows.contains { $0.isMiniaturized }
        case .never: wantDock = false
        }
        let policy: NSApplication.ActivationPolicy = wantDock ? .regular : .accessory
        guard NSApp.activationPolicy() != policy else { return }
        NSApp.setActivationPolicy(policy)
        if wantDock && windowOpen { NSApp.activate(ignoringOtherApps: true) }
    }

    private func buildMainMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        appMenu.addItem(settingsItem)
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit bb1", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu

        let editItem = NSMenuItem()
        main.addItem(editItem)
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit

        let streamItem = NSMenuItem()
        let stream = NSMenu(title: "Stream")
        stream.autoenablesItems = false
        stream.delegate = self
        streamItem.submenu = stream
        main.addItem(streamItem)
        streamMenu = stream
        NSApp.mainMenu = main
    }
}
