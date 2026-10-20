import AppKit
import BB1Core

/// What scripts/install-host.sh and uninstall-host.sh used to do, done by the app itself:
/// live in Applications, be the only copy, keep the login item pointing at this copy, and remove itself.
enum Installer {
    static var appURL: URL { URL(fileURLWithPath: Bundle.main.bundlePath) }
    private static var home: String { FileManager.default.homeDirectoryForCurrentUser.path }
    /// Started by launchd from the login item.
    static var launchedByLaunchd: Bool { ProcessInfo.processInfo.environment["XPC_SERVICE_NAME"] == LoginItem.label }

    /// Only an installed bundle manages itself (not `swift run`, not a test copy with its own support folder).
    static var isInstalledApp: Bool {
        Bundle.main.bundlePath.hasSuffix(".app") && BB1.build > 0 && ProcessInfo.processInfo.environment["BB1_SUPPORT_DIR"] == nil
    }

    // MARK: Move to Applications

    /// Opened from the disk image, Downloads or the Desktop: offer to move into Applications.
    /// Returns true when it's moving (the app relaunches from there; stop starting up).
    static func offerMoveIfNeeded() -> Bool {
        let path = appURL.path
        guard isInstalledApp,
              path.hasPrefix("/Volumes/") || path.contains("/AppTranslocation/")
                || path.hasPrefix(home + "/Downloads/") || path.hasPrefix(home + "/Desktop/") else { return false }
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Move bb1 to your Applications folder?"
        alert.informativeText = "bb1 runs from Applications, so it can update itself and start when you log in."
        alert.addButton(withTitle: "Move to Applications")
        alert.addButton(withTitle: "Not Now")
        guard alert.runModal() == .alertFirstButtonReturn else { return false }
        do {
            let dest = try moveToApplications()
            Log.info("install: moved to \(dest.path), relaunching")
            var script = "while kill -0 \(getpid()) 2>/dev/null; do sleep 0.1; done\n/usr/bin/open -n \(quote(dest.path))\n"
            // Started from the disk image: eject it once the copy is running.
            if path.hasPrefix("/Volumes/") {
                let volume = "/Volumes/" + path.dropFirst("/Volumes/".count).split(separator: "/")[0]
                script += "sleep 2\n/usr/bin/hdiutil detach \(quote(volume)) -quiet\n"
            }
            try runAfterExit(script)
            exit(0)
        } catch {
            let fail = NSAlert()
            fail.messageText = "bb1 couldn't move itself"
            fail.informativeText = "\(error.localizedDescription)\n\nDrag bb1 into Applications yourself, then open it from there."
            fail.runModal()
            return false
        }
    }

    /// Copy this app into /Applications (or ~/Applications if that isn't writable), replacing an older copy.
    private static func moveToApplications() throws -> URL {
        let fm = FileManager.default
        let system = URL(fileURLWithPath: "/Applications", isDirectory: true)
        let folder = fm.isWritableFile(atPath: system.path) ? system
            : URL(fileURLWithPath: home + "/Applications", isDirectory: true)
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        let dest = folder.appendingPathComponent("bb1.app", isDirectory: true)
        if fm.fileExists(atPath: dest.path) {
            quitOtherCopies()
            try fm.trashItem(at: dest, resultingItemURL: nil)
        }
        try fm.copyItem(at: appURL, to: dest)
        // The copy came off a downloaded disk image; it's the same signed app, so drop the "downloaded" mark.
        let xattr = Process()
        xattr.executableURL = URL(fileURLWithPath: "/usr/bin/xattr")
        xattr.arguments = ["-dr", "com.apple.quarantine", dest.path]
        try? xattr.run()
        xattr.waitUntilExit()
        return dest
    }

    // MARK: One copy

    /// At launch: stop older copies of bb1 that run from somewhere else, point the login item at this
    /// copy, and put the old ~/Applications copy (where install-host.sh put it) in the Trash.
    static func takeOver() {
        let me = appURL.standardizedFileURL.path
        // Only a copy that lives in Applications takes over (not one opened from the disk image).
        guard isInstalledApp, me.hasPrefix("/Applications/") || me.hasPrefix(home + "/Applications/") else { return }
        if LoginItem.isOn, let program = LoginItem.program, program != Bundle.main.executablePath {
            Log.info("install: login item pointed at \(program); moving it to this copy")
            // Stops the old copy that launchd started (and keeps launchd from restarting it).
            if !launchedByLaunchd { launchctl(["bootout", "gui/\(getuid())/\(LoginItem.label)"]) }
            LoginItem.set(true)
        }
        quitOtherCopies()
        let fm = FileManager.default
        for name in ["bb1.app", "BB1.app", "BB1 Host.app"] {
            let old = URL(fileURLWithPath: home + "/Applications/" + name).standardizedFileURL
            guard old.path != me, fm.fileExists(atPath: old.path) else { continue }
            if (try? fm.trashItem(at: old, resultingItemURL: nil)) != nil { Log.info("install: moved the old copy at \(old.path) to the Trash") }
        }
    }

    /// Ask any other running bb1 to quit (and make sure it did).
    private static func quitOtherCopies() {
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: Bundle.main.bundleIdentifier ?? "")
            .filter { $0.processIdentifier != getpid() }
        guard !others.isEmpty else { return }
        for app in others {
            Log.info("install: quitting the other bb1 at \(app.bundleURL?.path ?? "?") (pid \(app.processIdentifier))")
            app.terminate()
        }
        let deadline = Date().addingTimeInterval(4)
        while others.contains(where: { !$0.isTerminated }) && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.1)) }
        others.filter { !$0.isTerminated }.forEach { $0.forceTerminate() }
    }

    // MARK: Remove

    /// Settings → Remove bb1: stop starting at login, put the app in the Trash, quit.
    /// Pairings and logs stay in ~/Library, so reinstalling picks up where it left off.
    static func removeFromThisMac() -> Never {
        Log.info("install: removing bb1 from this Mac")
        LoginItem.set(false)
        try? FileManager.default.trashItem(at: appURL, resultingItemURL: nil)
        // Exiting with 0 tells launchd not to restart it.
        exit(0)
    }

    // MARK: Helpers

    static func quote(_ s: String) -> String { "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'" }

    /// Run a shell script once this process has gone (in its own session, so launchd doesn't stop it with us).
    static func runAfterExit(_ script: String) throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("bb1-\(UUID().uuidString).sh")
        try (script + "rm -f \(quote(url.path))\n").write(to: url, atomically: true, encoding: .utf8)
        try Updater.spawnDetached(["/bin/sh", url.path])
    }

    @discardableResult
    static func launchctl(_ args: [String]) -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
        p.waitUntilExit()
        return p.terminationStatus
    }
}

/// ~/Library/LaunchAgents/com.bb1.host.plist: start bb1 at login and restart it if it crashes.
enum LoginItem {
    static let label = "com.bb1.host"
    static var url: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/LaunchAgents/\(label).plist")
    }

    static var isOn: Bool { FileManager.default.fileExists(atPath: url.path) }

    /// The executable the login item starts.
    static var program: String? {
        (NSDictionary(contentsOf: url)?["ProgramArguments"] as? [String])?.first
    }

    /// Writes or removes the login item. It takes effect at the next login (loading it now would start a second copy).
    static func set(_ on: Bool) {
        guard on else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        guard let exe = Bundle.main.executablePath else { return }
        let plist: [String: Any] = [
            "Label": label,
            "ProgramArguments": [exe],
            "RunAtLoad": true,
            // Restart after a crash, but not after Quit (exit code 0).
            "KeepAlive": ["SuccessfulExit": false],
            "ThrottleInterval": 3,
            // Capture and input need a logged-in session.
            "LimitLoadToSessionType": "Aqua",
            "ProcessType": "Interactive",
        ]
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        (plist as NSDictionary).write(to: url, atomically: true)
    }
}
