import AppKit
import BB1Core
import WebKit

/// The dashboard's contents as the shared React UI (apps/ui), the page the Linux app shows, in a web view.
///
/// - The page (apps/ui/dist/index.html, copied to bb1.app/Contents/Resources/ui/ by scripts/bundle.sh) is
///   served as bb1://localhost/, and your other Macs' screenshots as bb1://localhost/preview/<name>.
///   `BB1_UI_URL=http://localhost:5173` loads the UI's dev server instead (cd apps/ui && bun run dev).
/// - App → page: the whole state (apps/ui/src/types.ts, State) through window.bb1Receive, whenever
///   something changes and once a second while the window is open.
/// - Page → app: one JSON command per message (window.webkit.messageHandlers.bb1).
///
/// Every command runs the same code the native pages run. Streams keep their own windows, which ask for
/// pairing codes themselves, so `stream` stays null here. If the page isn't there, doesn't load, doesn't
/// start or keeps crashing, the Dashboard falls back to its native pages.
final class WebDashboard: NSObject, WKNavigationDelegate {
    /// The page's background (--bg in apps/ui/src/styles.css), so nothing flashes white.
    static let background = NSColor(srgbRed: 9 / 255, green: 9 / 255, blue: 10 / 255, alpha: 1)
    private static var logDir: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/bb1", isDirectory: true)
    }

    private weak var dashboard: Dashboard?
    private let webView: DashboardWebView
    private let server: PageServer
    private let startURL: URL
    /// The page said "ready": it has its bb1Receive and wants the state.
    private var ready = false
    /// The last state sent, so an unchanged one isn't sent again every second.
    private var lastState: String?
    private var pushQueued = false
    /// A page asked for (Settings from the menu bar) before the page was ready.
    private var pendingPage: Dashboard.Page?
    /// Why the last stream ended, shown until dismissed.
    private var lastEnded: String?
    private var crashes = 0
    private var watchdogArmed = false
    private var doneDismissScheduled = false
    private var soundApps: (apps: [SoundApps.App], at: Date)?

    /// nil: use the native pages (asked for, or the page isn't there).
    init?(dashboard: Dashboard, page: Dashboard.Page) {
        let env = ProcessInfo.processInfo.environment
        if env["BB1_NATIVE_DASHBOARD"] == "1" || UserDefaults.standard.bool(forKey: "bb1.nativeDashboard") {
            Log.info("dashboard: native (asked for with BB1_NATIVE_DASHBOARD or bb1.nativeDashboard)")
            return nil
        }
        let file = WebDashboard.pageFile()
        let html = file.flatMap { try? Data(contentsOf: $0) }
        let query = page == .settings ? "?page=settings" : ""
        if let dev = env["BB1_UI_URL"], !dev.isEmpty, let url = URL(string: dev + query) {
            startURL = url
            Log.info("dashboard: web UI from \(dev)")
        } else if let file, html != nil {
            startURL = URL(string: "bb1://localhost/" + query)!
            Log.info("dashboard: web UI from \(file.path)")
        } else {
            Log.info("dashboard: native (apps/ui's page isn't in this build: cd apps/ui && bun install && bun run build)")
            return nil
        }
        self.dashboard = dashboard
        server = PageServer(html: html)
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(server, forURLScheme: "bb1")
        webView = DashboardWebView(frame: .zero, configuration: config)
        super.init()
        server.preview = { [weak self] name in self?.previewJPEG(name) }
        webView.configuration.userContentController.add(MessageRelay(self), name: "bb1")
        webView.navigationDelegate = self
        // The page paints its own background; until it does, the window's shows through (no white flash).
        // (Key-value coding finds WebKit's _setDrawsBackground:; checked first, as a missing key would crash.)
        if webView.responds(to: NSSelectorFromString("_setDrawsBackground:")) || webView.responds(to: NSSelectorFromString("setDrawsBackground:")) {
            webView.setValue(false, forKey: "drawsBackground")
        }
        webView.underPageBackgroundColor = WebDashboard.background
        webView.allowsBackForwardNavigationGestures = false
        webView.allowsMagnification = false
        // A file dropped on the window would replace the page with it.
        webView.unregisterDraggedTypes()
        // Safari's Web Inspector, for copies built from source.
        if BB1.build == 0 { webView.isInspectable = true }
        for name in [Updater.changed, HostUpdates.changed, HostPreviews.changed] {
            NotificationCenter.default.addObserver(self, selector: #selector(somethingChanged), name: name, object: nil)
        }
    }

    /// The bundled page, or for `swift run` and other copies outside bb1.app, the UI's build in the source tree.
    static func pageFile() -> URL? {
        if let url = Bundle.main.url(forResource: "index", withExtension: "html", subdirectory: "ui") { return url }
        // An installed bb1.app without its page is broken; better the native pages than some stray file.
        guard !Bundle.main.bundlePath.hasSuffix(".app") else { return nil }
        let fm = FileManager.default
        for start in [Bundle.main.bundleURL, URL(fileURLWithPath: #filePath)] {
            var dir = start.standardizedFileURL
            for _ in 0..<8 {
                for candidate in ["apps/ui/dist/index.html", "ui/dist/index.html"] {
                    let url = dir.appendingPathComponent(candidate)
                    if fm.fileExists(atPath: url.path) { return url }
                }
                dir = dir.deletingLastPathComponent()
            }
        }
        return nil
    }

    /// Fills the window under the title bar, not behind it: the strip with the window buttons stays the
    /// window's own, so it drags and double-click zooms like any title bar (a web view would take those clicks).
    func install(in content: NSView, window: NSWindow) {
        window.backgroundColor = WebDashboard.background
        webView.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(webView)
        NSLayoutConstraint.activate([
            webView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            webView.topAnchor.constraint(equalTo: content.safeAreaLayoutGuide.topAnchor),
            webView.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
        webView.load(URLRequest(url: startURL))
    }

    /// Back to the native pages: let go of everything.
    func remove() {
        NotificationCenter.default.removeObserver(self)
        ready = false
        webView.navigationDelegate = nil
        webView.configuration.userContentController.removeScriptMessageHandler(forName: "bb1")
        webView.stopLoading()
        webView.removeFromSuperview()
    }

    func focus() { webView.window?.makeFirstResponder(webView) }

    /// The window came up: send the state now, and make sure the page starts.
    func windowShown() {
        refresh()
        guard !ready, !watchdogArmed else { return }
        watchdogArmed = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self] in
            guard let self else { return }
            self.watchdogArmed = false
            if !self.ready { self.fallBack("the web UI didn't start within 10 s") }
        }
    }

    /// Switch the page to Your computers or Settings, with its own buttons (it keeps the page it's on itself).
    func show(_ page: Dashboard.Page) {
        guard ready else { pendingPage = page; return }
        let button = page == .settings ? "header button.icon-button[title=\"Settings\"]" : "header button.pill.link"
        webView.evaluateJavaScript("document.querySelector('\(button)')?.click()", completionHandler: nil)
    }

    func show(message: String) {
        lastEnded = message
        setNeedsPush()
    }

    /// Keep the screenshots and builds coming (as ComputersPage.reload does), then send the state.
    func refresh() {
        guard let dashboard else { return }
        var previewTargets: [ClientTarget] = []
        for e in dashboard.directory.entries where e.online {
            guard let target = ComputersPage.target(for: e) else { continue }
            if HostPreviews.canPreview(target, build: HostUpdates.shared.builds[e.name]) { previewTargets.append(target) }
            HostUpdates.shared.refresh(target)
        }
        if dashboard.window.isVisible { HostPreviews.shared.keepFresh(previewTargets) }
        push()
    }

    @objc private func somethingChanged() { setNeedsPush() }

    /// Several changes at once (a burst of screenshots) become one push.
    private func setNeedsPush() {
        guard !pushQueued else { return }
        pushQueued = true
        DispatchQueue.main.async { [weak self] in
            self?.pushQueued = false
            self?.push()
        }
    }

    private func push() {
        guard ready, let dashboard, dashboard.window.isVisible || lastState == nil, let json = stateJSON(),
              json != lastState else { return }
        lastState = json
        webView.evaluateJavaScript("window.bb1Receive && window.bb1Receive({type:'state',state:\(json)})", completionHandler: nil)
    }

    private func fallBack(_ why: String) {
        DispatchQueue.main.async { [weak self] in self?.dashboard?.fallBackToNative(why) }
    }

    // MARK: Loading

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        failed(error)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        failed(error)
    }

    private func failed(_ error: Error) {
        let e = error as NSError
        // A load replaced by another one isn't a failure.
        if e.domain == NSURLErrorDomain && e.code == NSURLErrorCancelled { return }
        fallBack("the web UI didn't load (\(error.localizedDescription))")
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        ready = false
        lastState = nil
        crashes += 1
        Log.info("dashboard: the web view's process ended (\(crashes) time\(crashes == 1 ? "" : "s"))")
        if crashes >= 3 { fallBack("the web UI keeps crashing"); return }
        webView.load(URLRequest(url: startURL))
    }

    /// bb1://localhost/preview/<name>: the shown name back to the Mac, and its last screenshot.
    private func previewJPEG(_ name: String) -> Data? {
        guard let e = entry(named: name) else { return nil }
        return HostPreviews.shared.jpeg(for: e.name)
    }

    // MARK: Commands (apps/ui/src/types.ts, Command)

    fileprivate func received(_ body: Any) {
        var command = body as? [String: Any]
        if command == nil, let text = body as? String, let data = text.data(using: .utf8) {
            command = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        }
        guard let command, let cmd = command["cmd"] as? String else {
            Log.info("dashboard: a message from the page that isn't a command")
            return
        }
        perform(cmd, command)
        setNeedsPush()
    }

    private func perform(_ cmd: String, _ c: [String: Any]) {
        guard let dashboard, let app = dashboard.delegate else { return }
        let str = { (key: String) -> String in c[key] as? String ?? "" }
        let flag = { (key: String) -> Bool in c[key] as? Bool ?? false }
        let viewer = (c["id"] as? Int).map { UInt32(truncatingIfNeeded: $0) }
        switch cmd {
        case "ready":
            if !ready { Log.info("dashboard: web UI ready") }
            ready = true
            lastState = nil
            if let page = pendingPage {
                pendingPage = nil
                show(page)
            }
        case "connect":
            connect(address: str("address"), label: str("label"))
        case "disconnect", "sendCode":
            break // the stream window has its own Disconnect and asks for the pairing code itself
        case "setSharing":
            setSharing(flag("on"))
        case "setViewerAccess":
            if let viewer, let access = HostController.Access(rawValue: str("what")) {
                app.hostRole?.host.setAccess(access, flag("on"), viewer: viewer)
            }
        case "disconnectViewer":
            if let viewer { app.hostRole?.host.disconnect(viewer: viewer) }
        case "setSettings":
            guard let s = c["settings"] as? [String: Any] else { break }
            if let on = s["allowTailscale"] as? Bool { app.updateSettings { $0.allowTailscale = on } }
            if let on = s["allowPairing"] as? Bool { app.updateSettings { $0.allowPairing = on } }
            if let on = s["nightly"] as? Bool {
                app.updateSettings { $0.nightly = on }
                Updater.shared.check()
            }
            if let on = s["sharing"] as? Bool { setSharing(on) }
        case "openLogs":
            NSWorkspace.shared.open(WebDashboard.logDir)
        case "dismissEnded":
            lastEnded = nil
        case "update":
            // The header's pill, as ComputersPage.pillClicked.
            let updater = Updater.shared
            if updater.state.busy || !dashboard.updatingHosts.isEmpty { break }
            if let r = updater.latest, r.build > BB1.build {
                dashboard.updateEverything(r)
            } else if updater.justUpdated != nil {
                updater.dismissJustUpdated()
            }
        case "updateComputer":
            guard let e = entry(named: str("name")), let latest = Updater.shared.latest,
                  let target = ComputersPage.target(for: e) else { break }
            HostUpdates.shared.update(target, to: latest.build)
        case "checkUpdates":
            Updater.shared.check(userInitiated: true)
        case "dismissUpdate":
            Updater.shared.dismissJustUpdated()
        case "forgetComputer":
            guard let host = entry(named: str("name"))?.known else { break }
            KnownHost.store.save(KnownHost.store.load().filter { $0.hostId != host.hostId })
            dashboard.directory.rebuild()
        case "renameComputer":
            guard let e = entry(named: str("name")) else { break }
            let new = str("newName").trimmingCharacters(in: .whitespacesAndNewlines)
            MacNames.custom[e.name] = new.isEmpty || new == e.name ? nil : new
        case "setRoute":
            guard let e = entry(named: str("name")) else { break }
            ComputersPage.preferHome[e.name] = str("route") == "home"
        case "cancelPairing":
            app.hostRole?.host.cancelPairing()
        case "forgetPaired":
            confirmForgetPaired()
        case "setMuted":
            let id = str("id")
            var muted = Set(app.settings.mutedApps)
            if flag("muted") { muted.insert(id) } else { muted.remove(id) }
            let ids = SoundApps.all.map { $0.id }.filter { muted.contains($0) }
            app.updateSettings { $0.mutedApps = ids }
        case "fixPermission":
            if str("which") == "screenRecording" { Permissions.fixScreenRecording() } else { Permissions.fixAccessibility() }
        case "setApp":
            guard let a = c["app"] as? [String: Any] else { break }
            if let on = a["openAtLogin"] as? Bool { app.setStartsAtLogin(on) }
            if let on = a["keepRunning"] as? Bool { app.updateSettings { $0.keepRunning = on } }
            if let dock = a["dock"] as? String, let icon = DockIcon.allCases.first(where: { WebDashboard.name(of: $0) == dock }) {
                app.updateSettings { $0.dockIcon = icon }
                app.refreshDock()
            }
        default:
            Log.info("dashboard: unknown command \(cmd) from the page")
        }
    }

    /// A Mac by the name the page shows (yours for it, or its own).
    private func entry(named name: String) -> HostDirectory.Entry? {
        let entries = dashboard?.directory.entries ?? []
        return entries.first { MacNames.display($0.name) == name } ?? entries.first { $0.name == name }
    }

    /// A tile's Connect (its address is the Mac's own name), or an address typed by hand
    /// (as ComputersPage.connectByAddress).
    private func connect(address: String, label: String) {
        guard let app = dashboard?.delegate else { return }
        lastEnded = nil
        let entries = dashboard?.directory.entries ?? []
        if let e = entries.first(where: { $0.name == address }) ?? (label.isEmpty ? nil : entry(named: label)) {
            if let target = ComputersPage.target(for: e) { app.connect(target) }
            return
        }
        var options = ClientOptions()
        options.host = address.trimmingCharacters(in: .whitespaces)
        guard let target = options.target() else {
            lastEnded = "That address doesn't look right."
            return
        }
        app.connect(target)
    }

    private func setSharing(_ on: Bool) {
        guard let app = dashboard?.delegate else { return }
        if on, let host = app.hostRole?.host, host.status().listenerError != nil {
            // "Try again" after sharing failed to start listening: start it afresh.
            app.setHosting(false)
            app.setHosting(true)
        } else {
            app.setHosting(on)
        }
    }

    private func confirmForgetPaired() {
        guard let window = dashboard?.window else { return }
        let alert = NSAlert()
        alert.messageText = "Forget all paired computers?"
        alert.informativeText = "They'll need to pair again (or sign in with Tailscale) next time they connect."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            PairedDevice.store.save([])
            Log.info("host: forgot all paired devices")
            self?.setNeedsPush()
        }
    }

    private static func name(of icon: DockIcon) -> String {
        switch icon {
        case .always: return "always"
        case .whileWindowOpen: return "open"
        case .never: return "never"
        }
    }

    // MARK: State (apps/ui/src/types.ts, State)

    private func stateJSON() -> String? {
        guard let dashboard, let app = dashboard.delegate else { return nil }
        let s = app.settings
        let build = BB1.isRelease ? "v\(BB1.version)" : (BB1.build == 0 ? "dev" : BB1.buildName(BB1.build))
        let computers: [[String: Any]] = dashboard.directory.entries.map { self.computer($0) }
        var state: [String: Any] = [
            "platform": "mac",
            "me": DeviceIdentity.name,
            "build": build,
            "tailscaleRunning": dashboard.directory.hasTailscale,
            "computers": computers,
            // The page reads null as "no stream"; streams have their own windows on the Mac.
            "stream": NSNull(),
            "sharing": sharing(app),
            "settings": [
                "sound": true, "clipboard": true, "modifierLayout": "standard", // the Mac has no "Connecting" settings
                "allowTailscale": s.allowTailscale, "allowPairing": s.allowPairing,
                "sharing": app.isHosting, "nightly": s.nightly,
            ] as [String: Any],
            "logDir": WebDashboard.logDir.path,
            "updateStatus": updateStatus(s),
            "pairedCount": PairedDevice.store.load().count,
            "mutedApps": mutedApps(s),
            "app": ["openAtLogin": app.startsAtLogin, "keepRunning": s.keepRunning, "dock": WebDashboard.name(of: s.dockIcon)] as [String: Any],
        ]
        if let lastEnded { state["lastEnded"] = lastEnded }
        if let pill = updatePill() { state["update"] = pill }
        if app.isHosting {
            state["permissions"] = ["screenRecording": Permissions.screenRecording, "accessibility": Permissions.accessibility]
        }
        guard JSONSerialization.isValidJSONObject(state),
              let data = try? JSONSerialization.data(withJSONObject: state) else {
            Log.info("dashboard: couldn't encode the state for the page")
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    /// One tile (as ComputersPage.model).
    private func computer(_ e: HostDirectory.Entry) -> [String: Any] {
        let target = ComputersPage.target(for: e)
        let pairing: Bool = { if case .code? = target?.auth { return true }; return false }()
        let phase = HostUpdates.shared.phases[e.name]
        let updating = phase.map { !$0.finished } ?? false
        var c: [String: Any] = ["name": MacNames.display(e.name), "online": e.online, "quiet": !pairing, "canRename": true]
        // The page connects by this; it's the Mac's own name, turned back into a target on "connect".
        if e.online, target != nil, !updating { c["address"] = e.name }
        if e.online, let ms = HostPreviews.shared.latencyMs[e.name], ms.isFinite { c["latencyMs"] = ms }
        if let preview = HostPreviews.shared.preview(for: e.name) {
            c["previewVersion"] = Int(preview.at.timeIntervalSince1970 * 1000)
            if !e.online { c["previewAgo"] = ComputersPage.ago(preview.at) }
        }
        if e.known != nil { c["canForget"] = true }
        if e.lan != nil && e.tailscaleIP != nil {
            c["routes"] = ["home", "tailscale"]
            c["route"] = ComputersPage.usesTailscale(e) ? "tailscale" : "home"
        }
        if updating { c["updating"] = Dashboard.progress(of: phase) }
        if !updating, e.online, let latest = Updater.shared.latest, let target,
           let build = HostUpdates.shared.builds[e.name], build < latest.build, HostUpdates.canUpdate(target, build: build) {
            c["canUpdate"] = true
        }
        return c
    }

    private func sharing(_ app: DashboardDelegate) -> [String: Any] {
        guard let role = app.hostRole else { return ["state": "off", "viewers": [Any]()] }
        let status = role.host.status()
        var sharing: [String: Any] = [
            "state": status.listenerError == nil ? "on" : "failed",
            "viewers": status.viewers.map { v -> [String: Any] in
                ["id": Int(v.id), "name": v.name, "mouse": v.mouse, "keyboard": v.keyboard, "controller": v.controller]
            },
        ]
        if let error = status.listenerError { sharing["error"] = error }
        if let p = role.pairing { sharing["pairing"] = ["device": p.device, "code": p.code] }
        let addresses = HostRole.addresses().map { "\($0.1)\($0.0.hasPrefix("utun") ? " Tailscale" : "")" }.joined(separator: "   ")
        if !addresses.isEmpty { sharing["addresses"] = addresses }
        return sharing
    }

    /// The header's update button (as ComputersPage.reloadPill).
    private func updatePill() -> [String: Any]? {
        guard let dashboard else { return nil }
        let updater = Updater.shared
        switch updater.state {
        case .downloading(_, let p): return ["kind": "busy", "label": "Updating · \(Int(p * 100))%", "progress": p]
        case .installing: return ["kind": "busy", "label": "Installing…", "progress": 1.0]
        case .restarting: return ["kind": "busy", "label": "Restarting bb1…", "progress": 1.0]
        default: break
        }
        if !dashboard.updatingHosts.isEmpty {
            let names = dashboard.updatingHosts.sorted().map { MacNames.display($0) }
            let progress = dashboard.updateProgress ?? 0
            return ["kind": "busy", "label": "Updating \(ComputersPage.list(names)) · \(Int(progress * 100))%", "progress": progress]
        }
        if let r = updater.latest, r.build > BB1.build {
            if case .failed = updater.state { return ["kind": "failed", "label": "Update failed · Try again"] }
            var pill: [String: Any] = ["kind": "offer", "label": "Update to \(r.shortName)"]
            let behind = dashboard.hostsBehind(r).map { MacNames.display($0.label) }
            if !behind.isEmpty { pill["also"] = behind }
            return pill
        }
        if let u = updater.justUpdated {
            dismissDoneLater()
            return ["kind": "done", "label": "Updated to \(BB1.isRelease ? "v\(BB1.version)" : BB1.buildName(u.to))"]
        }
        return nil
    }

    /// "Updated to …" stays a few seconds once you can see it, then goes (as ComputersPage.fadeOutDonePill).
    private func dismissDoneLater() {
        guard !doneDismissScheduled, dashboard?.window.isVisible == true else { return }
        doneDismissScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) {
            if Updater.shared.justUpdated != nil { Updater.shared.dismissJustUpdated() }
        }
    }

    /// Settings → Updates (as SettingsPage's row).
    private func updateStatus(_ s: AppSettings) -> [String: Any] {
        let updater = Updater.shared
        var text = "Up to date"
        var canCheck = true
        switch updater.state {
        case .idle: text = "Checks every few minutes"
        case .checking: text = "Checking…"; canCheck = false
        case .upToDate: text = "Up to date"
        case .available(let r): text = "\(r.title) is ready"; canCheck = false
        case .downloading(_, let p): text = "Downloading · \(Int(p * 100))%"; canCheck = false
        case .installing, .restarting: text = "Installing…"; canCheck = false
        case .failed(let message): text = message
        }
        if case .upToDate = updater.state, !s.nightly, (updater.latest?.build ?? 0) < BB1.build {
            // Moved from Nightly to Stable: nothing older is installed, the next release replaces this.
            text = "Newer than the latest release; the next one replaces it"
        }
        if !updater.canUpdate { text = "Development copy, doesn't update itself"; canCheck = false }
        return ["text": text, "canCheck": canCheck]
    }

    /// "Don't share sound from": the apps on this Mac (looked up at most every 30 s).
    private func mutedApps(_ s: AppSettings) -> [[String: Any]] {
        if soundApps == nil || Date().timeIntervalSince(soundApps!.at) > 30 {
            soundApps = (SoundApps.installed().map { $0.app }, Date())
        }
        let muted = Set(s.mutedApps)
        return (soundApps?.apps ?? []).map { ["id": $0.id, "name": $0.name, "muted": muted.contains($0.id)] }
    }
}

/// Serves the page and the screenshots on bb1://localhost/.
private final class PageServer: NSObject, WKURLSchemeHandler {
    let html: Data?
    /// A Mac's last screenshot by its shown name (JPEG).
    var preview: ((String) -> Data?)?

    init(html: Data?) { self.html = html }

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let url = urlSchemeTask.request.url else {
            urlSchemeTask.didFailWithError(URLError(.badURL))
            return
        }
        // `path` comes percent-decoded ("/preview/Ana's MacBook").
        let path = url.path
        var body: Data?
        var type = "text/html; charset=utf-8"
        var cache = "no-cache"
        if path.hasPrefix("/preview/") {
            body = preview?(String(path.dropFirst("/preview/".count)))
            type = "image/jpeg"
            cache = "max-age=31536000" // the URL changes with every new screenshot
        } else if path.isEmpty || path == "/" || path == "/index.html" {
            body = html
        }
        let headers = ["Content-Type": body == nil ? "text/plain" : type, "Cache-Control": cache,
                       "Content-Length": String(body?.count ?? 0)]
        guard let response = HTTPURLResponse(url: url, statusCode: body == nil ? 404 : 200, httpVersion: "HTTP/1.1",
                                             headerFields: headers) else {
            urlSchemeTask.didFailWithError(URLError(.cannotParseResponse))
            return
        }
        urlSchemeTask.didReceive(response)
        urlSchemeTask.didReceive(body ?? Data())
        urlSchemeTask.didFinish()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {
        // Every request is answered at once in `start`, so there's nothing left to stop.
    }
}

/// The page's messages, without the content controller keeping the dashboard alive (it holds handlers strongly).
private final class MessageRelay: NSObject, WKScriptMessageHandler {
    weak var target: WebDashboard?

    init(_ target: WebDashboard) { self.target = target }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        target?.received(message.body)
    }
}

/// The page is the app: no Reload / Back in its right-click menu.
private final class DashboardWebView: WKWebView {
    override func willOpenMenu(_ menu: NSMenu, with event: NSEvent) {
        menu.removeAllItems()
        super.willOpenMenu(menu, with: event)
    }
}
