import AppKit
import BB1Core
import Network

/// Live pictures of your other Macs' screens for the dashboard. Main thread only.
///
/// While the dashboard is open, each reachable host gets a quiet "update" connection (it doesn't touch a
/// stream) that asks for a small screenshot every few seconds. The last picture of each Mac is kept on disk,
/// so an offline Mac still shows what it last looked like.
final class HostPreviews {
    static let shared = HostPreviews()
    /// Posted (main thread) when a picture or a latency changes.
    static let changed = Notification.Name("bb1.hostPreviews.changed")
    /// The first build whose host answers preview requests.
    static let minimumHostBuild = 15
    private static let interval: TimeInterval = 4
    private static let maxWidth = 960

    struct Preview {
        var image: CGImage
        var at: Date
    }

    private(set) var previews: [String: Preview] = [:]
    /// Network round trip to each host, from the last preview (ms).
    private(set) var latencyMs: [String: Double] = [:]

    private final class Session {
        let channel: ControlChannel
        var timer: Timer?
        var sentAt: Date?
        init(channel: ControlChannel) { self.channel = channel }
    }
    private var sessions: [String: Session] = [:]
    /// When a host's last connection failed, so retries back off.
    private var failedAt: [String: Date] = [:]

    /// Can this host send previews? (Signed in without a prompt, and new enough.)
    /// A host whose build we don't know yet gets one try (the session closes if it's too old).
    static func canPreview(_ target: ClientTarget, build: Int?) -> Bool {
        if let build, build < minimumHostBuild { return false }
        return HostUpdates.signsInQuietly(target)
    }

    /// Keep previews coming from exactly these hosts (call it whenever the dashboard refreshes).
    func keepFresh(_ targets: [ClientTarget]) {
        let wanted = Set(targets.map(\.label))
        for name in sessions.keys where !wanted.contains(name) { close(name) }
        for target in targets where sessions[target.label] == nil {
            if let failed = failedAt[target.label], Date().timeIntervalSince(failed) < 15 { continue }
            open(target)
        }
    }

    /// The dashboard closed: stop asking.
    func stopAll() {
        for name in Array(sessions.keys) { close(name) }
    }

    // MARK: Connections

    private func open(_ target: ClientTarget) {
        let name = target.label
        let conn = NWConnection(to: target.endpoint, using: ControlChannel.tcpParameters())
        let channel = ControlChannel(connection: conn, role: .client(
            clientName: Host.current().localizedName ?? "Mac", deviceId: DeviceIdentity.id, auth: target.auth,
            askCode: { $0(nil) }), queue: .main)
        channel.purpose = "update"
        let session = Session(channel: channel)
        sessions[name] = session
        channel.onAuthenticated = { [weak self, weak session] _ in
            guard let self, let session else { return }
            HostUpdates.shared.note(build: session.channel.peerBuild, for: name)
            guard session.channel.peerBuild >= HostPreviews.minimumHostBuild else { self.close(name); return }
            self.request(session)
            session.timer = Timer.scheduledTimer(withTimeInterval: HostPreviews.interval, repeats: true) { [weak self, weak session] _ in
                if let session { self?.request(session) }
            }
        }
        channel.onMessage = { [weak self, weak session] message in
            guard let self, let session, case .preview(let preview) = message else { return }
            self.received(preview, from: name, session: session)
        }
        channel.onClose = { [weak self, weak session] _ in
            guard let self, let session, self.sessions[name] === session else { return }
            session.timer?.invalidate()
            self.sessions[name] = nil
            self.failedAt[name] = Date()
        }
        channel.start()
    }

    private func close(_ name: String) {
        guard let session = sessions.removeValue(forKey: name) else { return }
        session.timer?.invalidate()
        session.channel.close(.closed("dashboard closed"))
    }

    private func request(_ session: Session) {
        session.sentAt = Date()
        session.channel.send(.requestPreview(maxWidth: HostPreviews.maxWidth))
    }

    private func received(_ preview: PreviewMsg, from name: String, session: Session) {
        if let sent = session.sentAt {
            latencyMs[name] = max(1, Date().timeIntervalSince(sent) * 1000 - preview.captureMs)
        }
        failedAt[name] = nil
        if let jpeg = preview.jpeg, let image = HostPreviews.decode(jpeg) {
            previews[name] = Preview(image: image, at: Date())
            try? jpeg.write(to: HostPreviews.cacheURL(name), options: .atomic)
        }
        NotificationCenter.default.post(name: HostPreviews.changed, object: self)
    }

    // MARK: Disk cache

    private static var cacheDir: URL {
        let dir = BB1.supportDirectory().appendingPathComponent("previews", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private static func cacheURL(_ name: String) -> URL {
        let safe = name.map { $0.isLetter || $0.isNumber ? $0 : "_" }
        return cacheDir.appendingPathComponent(String(safe) + ".jpg")
    }

    /// The last picture of a host, from memory or disk.
    func preview(for name: String) -> Preview? {
        if let p = previews[name] { return p }
        let url = HostPreviews.cacheURL(name)
        guard let data = try? Data(contentsOf: url), let image = HostPreviews.decode(data) else { return nil }
        let at = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date ?? Date()
        let p = Preview(image: image, at: at)
        previews[name] = p
        return p
    }

    private static func decode(_ data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}
