import AppKit
import BB1Core
import Security

/// One build of bb1 published on GitHub: a release, or a nightly (marked pre-release).
struct AppRelease: Equatable {
    var build: Int
    var version: String
    var isNightly: Bool
    var zipURL: URL
    var pageURL: URL
    var size: Int

    var title: String { "bb1 \(shortName)" }
    /// "nightly-3" for a nightly, "v0.0.1" for a release ("Update to …").
    var shortName: String { isNightly ? BB1.buildName(build) : "v\(version)" }
}

struct UpdateError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

/// Keeps this Mac's bb1 up to date: checks GitHub releases, downloads a newer build, checks it's signed
/// with the same certificate as this copy, swaps the app bundle and restarts. Main thread only.
///
/// Builds are compared by build number (CFBundleVersion, the commit count). Two channels, picked in
/// Settings: Stable offers only releases, Nightly offers only nightlies (GitHub pre-releases).
///
/// Automated tests only: `BB1_UPDATE_FEED` replaces the GitHub releases URL (a file:// URL works), and
/// `BB1_TEST_AUTO_UPDATE` installs an update as soon as one is found.
final class Updater {
    static let shared = Updater()
    static let repo = "spheceo/bb1"
    /// Posted (main thread) whenever `state` changes.
    static let changed = Notification.Name("bb1.updater.changed")

    enum State: Equatable {
        case idle
        case checking
        case upToDate
        case available(AppRelease)
        case downloading(AppRelease, Double)
        case installing(AppRelease)
        case restarting(AppRelease)
        case failed(String)

        var busy: Bool {
            switch self {
            case .downloading, .installing, .restarting: return true
            default: return false
            }
        }
    }

    private(set) var state: State = .idle {
        didSet { if state != oldValue { NotificationCenter.default.post(name: Updater.changed, object: self) } }
    }
    /// Newest build on GitHub (for this Mac's channel), from the last successful check.
    private(set) var latest: AppRelease?
    private(set) var lastChecked: Date?
    /// Set on the first launch after an update, until dismissed.
    private(set) var justUpdated: JustUpdated?
    private var timer: Timer?
    /// Is bb1's window open? Remembered across the restart, so an update doesn't open a window you'd closed.
    var windowOpen: () -> Bool = { true }

    /// Only an installed, signed .app can replace itself (not `swift run`).
    var canUpdate: Bool { BB1.build > 0 && Bundle.main.bundlePath.hasSuffix(".app") }

    func start() {
        if let u = JustUpdated.take() {
            Log.info("update: now running build \(u.to) (updated from build \(u.from))")
            justUpdated = u
        }
        guard canUpdate, timer == nil else { return }
        // After the installer's own clean-up (it removes its folder 3 s after starting this copy).
        DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
            if self?.state.busy == false { Updater.removeLeftovers() }
        }
        check()
        // GitHub answers "nothing changed" (304) to a repeat request without counting it against the
        // hourly limit, so checking often is cheap.
        timer = Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in self?.check() }
        NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.checkIfStale()
        }
    }

    /// Check again if the last check was over a minute ago (when bb1 comes to the front).
    func checkIfStale() {
        guard canUpdate else { return }
        if let lastChecked, Date().timeIntervalSince(lastChecked) < 60 { return }
        check()
    }

    /// Ask GitHub for the newest build. `userInitiated` shows "Checking…" and any error.
    func check(userInitiated: Bool = false) {
        guard canUpdate else {
            if userInitiated { state = .failed("This copy of bb1 isn't an installed app, so it can't update itself.") }
            return
        }
        guard !state.busy, state != .checking else { return }
        if userInitiated { state = .checking }
        Updater.fetchReleases { [weak self] result in
            guard let self, !self.state.busy else { return }
            switch result {
            case .failure(let error):
                Log.info("update: check failed: \(error.localizedDescription)")
                if userInitiated { self.state = .failed("Couldn't check for updates: \(error.localizedDescription)") }
            case .success(let releases):
                self.lastChecked = Date()
                let nightlyChannel = AppSettings.load().nightly
                let before = self.latest
                self.latest = releases.filter { $0.isNightly == nightlyChannel }.max { $0.build < $1.build }
                if self.latest != before { NotificationCenter.default.post(name: Updater.changed, object: self) }
                if let latest = self.latest, latest.build > BB1.build {
                    if self.state != .available(latest) { Log.info("update: \(latest.title) is available (this is build \(BB1.build))") }
                    self.state = .available(latest)
                    if ProcessInfo.processInfo.environment["BB1_TEST_AUTO_UPDATE"] != nil { self.install(latest) }
                } else {
                    self.state = .upToDate
                }
            }
        }
    }

    /// An update cut short (power off, crash, quit mid-download) can leave its staging folder next to
    /// the app, with the previous bb1.app inside it, or a downloaded zip. Nothing is installing at
    /// start-up, so anything still there is stale.
    private static func removeLeftovers() {
        let fm = FileManager.default
        let staging = URL(fileURLWithPath: Bundle.main.bundlePath).deletingLastPathComponent()
            .appendingPathComponent(".bb1-update", isDirectory: true)
        if fm.fileExists(atPath: staging.path) {
            if (try? fm.removeItem(at: staging)) != nil { Log.info("update: removed a leftover \(staging.path)") }
        }
        let tmp = fm.temporaryDirectory
        for name in (try? fm.contentsOfDirectory(atPath: tmp.path)) ?? [] where name.hasPrefix("bb1-update-") && name.hasSuffix(".zip") {
            try? fm.removeItem(at: tmp.appendingPathComponent(name))
        }
        // Window positions saved by older builds under names nothing reads any more.
        for key in ["NSWindow Frame bb1.dashboard", "NSWindow Frame bb1.dashboard.v2", "NSWindow Frame bb1.dashboard.v3"] {
            UserDefaults.standard.removeObject(forKey: key)
        }
    }

    func dismissJustUpdated() {
        justUpdated = nil
        NotificationCenter.default.post(name: Updater.changed, object: self)
    }

    // MARK: Installing

    /// Download, check, swap and restart. `report` hears each step (the host forwards it to the client).
    func install(_ release: AppRelease, report: ((UpdateStatusMsg) -> Void)? = nil) {
        guard !state.busy else { return }
        let fail: (String) -> Void = { [weak self] message in
            Log.info("update: failed: \(message)")
            self?.state = .failed(message)
            report?(UpdateStatusMsg(phase: "failed", detail: message))
        }
        guard canUpdate else { fail("This copy of bb1 isn't an installed app, so it can't update itself."); return }
        let app = URL(fileURLWithPath: Bundle.main.bundlePath)
        let folder = app.deletingLastPathComponent()
        guard !app.path.contains("/AppTranslocation/") else {
            fail("Move bb1 into your Applications folder and open it from there, then update.")
            return
        }
        guard FileManager.default.isWritableFile(atPath: folder.path) else {
            fail("bb1 isn't allowed to replace itself in \(folder.path).")
            return
        }

        Log.info("update: installing \(release.title) (this is build \(BB1.build))")
        state = .downloading(release, 0)
        report?(UpdateStatusMsg(phase: "downloading", progress: 0))
        var lastReported = 0.0
        Download.start(release.zipURL, progress: { [weak self] p in
            guard let self, case .downloading = self.state else { return }
            // Tell the UI and the client in 2% steps, not on every packet.
            guard p - lastReported >= 0.02 || p >= 1 else { return }
            lastReported = p
            self.state = .downloading(release, p)
            report?(UpdateStatusMsg(phase: "downloading", progress: p))
        }, done: { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                fail("Download failed: \(error.localizedDescription)")
            case .success(let zip):
                self.state = .installing(release)
                report?(UpdateStatusMsg(phase: "installing"))
                DispatchQueue.global(qos: .userInitiated).async {
                    let staging = folder.appendingPathComponent(".bb1-update", isDirectory: true)
                    do {
                        let newApp = try Updater.unpack(zip, into: staging, expectBuild: release.build)
                        try? FileManager.default.removeItem(at: zip)
                        DispatchQueue.main.async {
                            self.state = .restarting(release)
                            report?(UpdateStatusMsg(phase: "restarting"))
                            self.relaunch(app: app, newApp: newApp, staging: staging, release: release)
                        }
                    } catch {
                        try? FileManager.default.removeItem(at: zip)
                        try? FileManager.default.removeItem(at: staging)
                        DispatchQueue.main.async { fail(error.localizedDescription) }
                    }
                }
            }
        })
    }

    /// Host: a client asked for this exact build. Look it up on GitHub, then install it.
    func installBuild(_ build: Int, report: @escaping (UpdateStatusMsg) -> Void) {
        guard build > BB1.build else {
            report(UpdateStatusMsg(phase: "upToDate", detail: "Already on \(BB1.buildName(BB1.build))"))
            return
        }
        guard !state.busy else { report(UpdateStatusMsg(phase: "failed", detail: "An update is already running")); return }
        Updater.fetchReleases { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                report(UpdateStatusMsg(phase: "failed", detail: "Couldn't reach GitHub: \(error.localizedDescription)"))
            case .success(let releases):
                guard let release = releases.first(where: { $0.build == build }) else {
                    report(UpdateStatusMsg(phase: "failed", detail: "\(BB1.buildName(build)) isn't on GitHub"))
                    return
                }
                self.install(release, report: report)
            }
        }
    }

    /// Unzip next to the app (same volume, so the swap is a rename) and make sure it's really bb1:
    /// same bundle ID, the expected build, and signed with this copy's certificate.
    private static func unpack(_ zip: URL, into staging: URL, expectBuild: Int) throws -> URL {
        let fm = FileManager.default
        try? fm.removeItem(at: staging)
        try fm.createDirectory(at: staging, withIntermediateDirectories: true)
        let ditto = Process()
        ditto.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        ditto.arguments = ["-x", "-k", zip.path, staging.path]
        try ditto.run()
        ditto.waitUntilExit()
        guard ditto.terminationStatus == 0 else { throw UpdateError("Couldn't unzip the update") }
        guard let name = try fm.contentsOfDirectory(atPath: staging.path).first(where: { $0.hasSuffix(".app") }) else {
            throw UpdateError("The download has no app in it")
        }
        let newApp = staging.appendingPathComponent(name, isDirectory: true)
        guard let info = Bundle(url: newApp)?.infoDictionary,
              info["CFBundleIdentifier"] as? String == Bundle.main.bundleIdentifier else {
            throw UpdateError("The download isn't bb1")
        }
        guard (info["CFBundleVersion"] as? String).flatMap(Int.init) == expectBuild else {
            throw UpdateError("The download isn't build \(expectBuild)")
        }
        try checkSignature(newApp)
        return newApp
    }

    /// bb1 signed with the owner's Developer ID (Apple-issued, team 5PLDN9BDV4). Builds up to nightly-4 were
    /// signed with a local certificate; this lets them move over to Developer ID-signed builds.
    private static let developerIDRequirement = """
        anchor apple generic and identifier "com.bb1.host" and certificate leaf[subject.OU] = "5PLDN9BDV4" \
        and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists
        """

    /// The new app must satisfy this app's designated requirement (same bundle ID, same signing certificate),
    /// or be bb1 signed with the owner's Developer ID.
    private static func checkSignature(_ app: URL) throws {
        var me: SecCode?
        var meStatic: SecStaticCode?
        var requirement: SecRequirement?
        var new: SecStaticCode?
        guard SecCodeCopySelf([], &me) == errSecSuccess, let me,
              SecCodeCopyStaticCode(me, [], &meStatic) == errSecSuccess, let meStatic,
              SecCodeCopyDesignatedRequirement(meStatic, [], &requirement) == errSecSuccess, let requirement,
              SecStaticCodeCreateWithPath(app as CFURL, [], &new) == errSecSuccess, let new else {
            throw UpdateError("Couldn't check the update's signature")
        }
        let flags = SecCSFlags(rawValue: kSecCSCheckAllArchitectures | kSecCSStrictValidate | kSecCSCheckNestedCode)
        var status = SecStaticCodeCheckValidity(new, flags, requirement)
        var developerID: SecRequirement?
        if status != errSecSuccess,
           SecRequirementCreateWithString(developerIDRequirement as CFString, [], &developerID) == errSecSuccess, let developerID {
            status = SecStaticCodeCheckValidity(new, flags, developerID)
        }
        guard status == errSecSuccess else {
            throw UpdateError("The update isn't signed with bb1's certificate (error \(status)), so it wasn't installed")
        }
    }

    /// Hand over to a small script that waits for this process to exit, swaps the bundles and starts
    /// the new one (through launchd if bb1 is the login item, so crash-restart keeps working).
    private func relaunch(app: URL, newApp: URL, staging: URL, release: AppRelease) {
        let script = """
        pid=$1; app=$2; new=$3; staging=$4
        i=0; while kill -0 "$pid" 2>/dev/null && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
        kill -9 "$pid" 2>/dev/null
        old="$staging/previous.app"
        rm -rf "$old"
        if mv "$app" "$old"; then
          mv "$new" "$app" || mv "$old" "$app"
        fi
        job="gui/$(id -u)/com.bb1.host"
        if launchctl print "$job" 2>/dev/null | grep -qF "$app/Contents/MacOS/bb1"; then
          launchctl kickstart "$job"
        else
          /usr/bin/open -n \(Updater.testEnvironment.map { "--env '\($0)' " }.joined())"$app"
        fi
        sleep 3
        rm -rf "$staging"
        """
        let scriptURL = staging.appendingPathComponent("install.sh")
        do {
            try script.write(to: scriptURL, atomically: true, encoding: .utf8)
            JustUpdated(from: BB1.build, to: release.build, windowOpen: windowOpen()).save()
            // A new session, so launchd doesn't kill the script along with this app when it exits.
            try Updater.spawnDetached(["/bin/sh", scriptURL.path, "\(getpid())", app.path, newApp.path, staging.path])
        } catch {
            JustUpdated.clear()
            state = .failed("Couldn't start the installer: \(error.localizedDescription)")
            return
        }
        Log.info("update: restarting into build \(release.build)")
        // Give the last status message a moment to reach the client.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { exit(0) }
    }

    /// Test settings carried over to the restarted app, so a test copy stays separate from the real one.
    private static var testEnvironment: [String] {
        ["BB1_SUPPORT_DIR", "BB1_UPDATE_FEED"].compactMap { key in
            ProcessInfo.processInfo.environment[key].map { "\(key)=\($0)" }
        }
    }

    static func spawnDetached(_ args: [String]) throws {
        var attr = posix_spawnattr_t(nil as OpaquePointer?)
        posix_spawnattr_init(&attr)
        defer { posix_spawnattr_destroy(&attr) }
        posix_spawnattr_setflags(&attr, Int16(POSIX_SPAWN_SETSID))
        var argv = args.map { strdup($0) } + [nil]
        defer { argv.forEach { free($0) } }
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, args[0], nil, &attr, &argv, environ)
        guard rc == 0 else { throw UpdateError("posix_spawn failed (\(rc))") }
    }

    // MARK: GitHub

    /// The last answer from GitHub and its ETag, so a repeat request can be answered with 304.
    private static var cached: (etag: String, releases: [AppRelease])?

    /// All published builds, newest first. Callback on the main thread.
    static func fetchReleases(_ done: @escaping (Result<[AppRelease], Error>) -> Void) {
        let feed = ProcessInfo.processInfo.environment["BB1_UPDATE_FEED"] ?? "https://api.github.com/repos/\(repo)/releases?per_page=30"
        var request = URLRequest(url: URL(string: feed)!)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("bb1/\(BB1.version) (build \(BB1.build))", forHTTPHeaderField: "User-Agent")
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 20
        let cached = Updater.cached
        if let cached { request.setValue(cached.etag, forHTTPHeaderField: "If-None-Match") }
        URLSession.shared.dataTask(with: request) { data, response, error in
            let http = response as? HTTPURLResponse
            let result: Result<[AppRelease], Error>
            if let error {
                result = .failure(error)
            } else if http?.statusCode == 304, let cached {
                result = .success(cached.releases)
            } else if let http = response as? HTTPURLResponse, http.statusCode != 200, request.url?.isFileURL == false {
                result = .failure(UpdateError("GitHub answered \(http.statusCode)"))
            } else {
                result = Result { try parse(data ?? Data()) }
            }
            DispatchQueue.main.async {
                if case .success(let releases) = result, let etag = http?.value(forHTTPHeaderField: "ETag") {
                    Updater.cached = (etag, releases)
                }
                done(result)
            }
        }.resume()
    }

    static func parse(_ data: Data) throws -> [AppRelease] {
        struct Release: Decodable {
            var prerelease: Bool
            var draft: Bool
            var html_url: URL
            var assets: [Asset]
        }
        struct Asset: Decodable {
            var name: String
            var browser_download_url: URL
            var size: Int
        }
        return try JSONDecoder().decode([Release].self, from: data).compactMap { r in
            guard !r.draft else { return nil }
            for a in r.assets {
                guard let (version, build) = zipName(a.name) else { continue }
                return AppRelease(build: build, version: version, isNightly: r.prerelease, zipURL: a.browser_download_url,
                                  pageURL: r.html_url, size: a.size)
            }
            return nil
        }
    }

    /// "bb1-0.0.1-build12.zip" or "bb1-v0.0.1-build12.zip" → ("0.0.1", 12)
    static func zipName(_ name: String) -> (String, Int)? {
        guard name.hasPrefix("bb1-"), name.hasSuffix(".zip") else { return nil }
        var middle = name.dropFirst(4).dropLast(4)
        if middle.hasPrefix("v") { middle = middle.dropFirst() }
        let parts = middle.components(separatedBy: "-build")
        guard parts.count == 2, let build = Int(parts[1]), !parts[0].isEmpty else { return nil }
        return (parts[0], build)
    }
}

/// Written just before restarting into a new build, read once by the new build to say "Updated".
struct JustUpdated: Codable {
    var from: Int
    var to: Int
    /// bb1's window was open before the restart (nil from builds before 21).
    var windowOpen: Bool?

    private static var url: URL { BB1.supportDirectory().appendingPathComponent("just-updated.json") }

    func save() { try? JSONEncoder().encode(self).write(to: JustUpdated.url, options: .atomic) }
    static func clear() { try? FileManager.default.removeItem(at: url) }

    /// The update that just finished, if this launch is its first run.
    static func take() -> JustUpdated? {
        guard let data = try? Data(contentsOf: url), let u = try? JSONDecoder().decode(JustUpdated.self, from: data) else { return nil }
        clear()
        return u.to == BB1.build ? u : nil
    }
}

/// A file download with progress. Callbacks on the main thread.
private final class Download: NSObject, URLSessionDownloadDelegate {
    private let progress: (Double) -> Void
    private let done: (Result<URL, Error>) -> Void
    private var finished = false

    private init(progress: @escaping (Double) -> Void, done: @escaping (Result<URL, Error>) -> Void) {
        self.progress = progress
        self.done = done
    }

    static func start(_ url: URL, progress: @escaping (Double) -> Void, done: @escaping (Result<URL, Error>) -> Void) {
        let delegate = Download(progress: progress, done: done)
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 30
        // The session keeps the delegate alive until it's invalidated.
        URLSession(configuration: config, delegate: delegate, delegateQueue: nil).downloadTask(with: url).resume()
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64,
                    totalBytesWritten written: Int64, totalBytesExpectedToWrite total: Int64) {
        guard total > 0 else { return }
        let p = Double(written) / Double(total)
        DispatchQueue.main.async { self.progress(p) }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        if let http = downloadTask.response as? HTTPURLResponse, http.statusCode != 200, http.url?.isFileURL == false {
            finish(.failure(UpdateError("GitHub answered \(http.statusCode)")))
            return
        }
        // The temporary file is deleted when this returns; move it first.
        let dest = FileManager.default.temporaryDirectory.appendingPathComponent("bb1-update-\(UUID().uuidString).zip")
        do {
            try FileManager.default.moveItem(at: location, to: dest)
            finish(.success(dest))
        } catch {
            finish(.failure(error))
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error { finish(.failure(error)) }
        session.finishTasksAndInvalidate()
    }

    private func finish(_ result: Result<URL, Error>) {
        DispatchQueue.main.async {
            guard !self.finished else { return }
            self.finished = true
            self.done(result)
        }
    }
}
