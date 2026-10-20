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

    init(forcedMode: AppMode?, hostOptions: HostOptions, clientOptions: ClientOptions) {
        self.forcedMode = forcedMode
        self.hostOptions = hostOptions
        self.clientOptions = clientOptions
    }

    /// Started by launchd at login (no window then), or opened by the user.
    private var launchedAtLogin: Bool { ProcessInfo.processInfo.environment["XPC_SERVICE_NAME"] == "com.bb1.host" }

    var isHosting: Bool { hostRole != nil }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let hosting = forcedMode.map { $0 == .host } ?? settings.isHosting
        Log.setup(hosting ? "host" : "client")
        Log.info("app: starting bb1 v\(BB1.version)\(hosting ? ", sharing this Mac" : "")")
        buildMainMenu()
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu

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
        if clientOptions.host != nil {
            role.start()
        } else if !launchedAtLogin {
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

    private static var loginAgentURL: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/LaunchAgents/com.bb1.host.plist")
    }

    var startsAtLogin: Bool { FileManager.default.fileExists(atPath: AppDelegate.loginAgentURL.path) }

    /// Writes or removes the login item. It takes effect at the next login (loading it now would start a second copy).
    func setStartsAtLogin(_ on: Bool) {
        let url = AppDelegate.loginAgentURL
        guard on else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        guard let exe = Bundle.main.executablePath else { return }
        let plist: [String: Any] = [
            "Label": "com.bb1.host",
            "ProgramArguments": [exe],
            "RunAtLoad": true,
            "KeepAlive": ["SuccessfulExit": false],
            "ThrottleInterval": 3,
            "LimitLoadToSessionType": "Aqua",
            "ProcessType": "Interactive",
        ]
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        (plist as NSDictionary).write(to: url, atomically: true)
    }

    // MARK: Menu bar

    private func updateIcon() {
        guard let button = statusItem?.button else { return }
        if hostRole?.hasProblem == true {
            button.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: "bb1 needs attention")
            button.appearsDisabled = false
        } else {
            button.image = MenuBarIcon.image
            // Full strength while sharing or connected; dimmed when idle.
            button.appearsDisabled = !(isHosting || clientRole?.isActive == true)
        }
        button.title = ""
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === streamMenu {
            clientRole.fillStreamMenu(menu)
            return
        }
        menu.removeAllItems()
        let open = NSMenuItem(title: "Open bb1", action: #selector(openDashboard), keyEquivalent: "")
        open.target = self
        open.attributedTitle = NSAttributedString(string: "Open bb1", attributes: [.font: NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)])
        menu.addItem(open)
        menu.addItem(.separator())
        hostRole?.addItems(to: menu)
        clientRole.addItems(to: menu)
        menu.addItem(.separator())

        let share = NSMenuItem(title: "Share This Mac", action: #selector(toggleSharing), keyEquivalent: "")
        share.target = self
        share.state = isHosting ? .on : .off
        menu.addItem(share)
        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)
        if let hostRole {
            let more = NSMenu()
            more.addItem(hostRole.faultMenuItem())
            let item = NSMenuItem(title: "Diagnostics", action: nil, keyEquivalent: "")
            item.submenu = more
            menu.addItem(item)
        }
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit bb1", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
    }

    @objc private func openDashboard() { dashboard.show(.computers) }
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
