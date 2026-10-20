import BB1Core
import Foundation
import Network

/// Which build of bb1 each of your other Macs runs, and updating them from this Mac. Main thread only.
///
/// A host's build is learned whenever this Mac signs in to it (streaming, or a short "update" connection
/// that doesn't disturb a stream). Hosts from before self-update report no build (0); they have to be
/// updated by hand once, and can't be sent an update message (they'd treat the connection as a stream).
final class HostUpdates {
    static let shared = HostUpdates()
    /// Posted (main thread) whenever a build or an update's progress changes.
    static let changed = Notification.Name("bb1.hostUpdates.changed")

    enum Phase: Equatable {
        case starting
        case downloading(Double)
        case installing
        case restarting
        /// The host restarted; waiting until it answers with the new build.
        case waiting
        case done(Int)
        case failed(String)

        var finished: Bool {
            switch self {
            case .done, .failed: return true
            default: return false
            }
        }
    }

    /// Host name → build. 0 = a bb1 too old to update remotely.
    private(set) var builds: [String: Int] = UserDefaults.standard.dictionary(forKey: "bb1.hostBuilds") as? [String: Int] ?? [:] {
        didSet { UserDefaults.standard.set(builds, forKey: "bb1.hostBuilds") }
    }
    private(set) var phases: [String: Phase] = [:]
    private var channels: [String: ControlChannel] = [:]

    func note(build: Int, for host: String) {
        guard builds[host] != build else { return }
        builds[host] = build
        changed()
    }

    /// Can this host be asked to update? (Signed-in without a pairing prompt, and new enough to understand.)
    static func canUpdate(_ target: ClientTarget, build: Int?) -> Bool {
        guard let build, build > 0 else { return false }
        return signsInQuietly(target)
    }

    /// Signs in without showing anything on the host (no pairing code).
    static func signsInQuietly(_ target: ClientTarget) -> Bool {
        switch target.auth {
        case .device, .tailscale: return true
        case .code, .legacyKey: return false
        }
    }

    private var lastRefresh: [String: Date] = [:]

    /// Ask the host which build it runs, at most every 5 minutes. Also for a host whose build we don't know
    /// yet (just paired); not for one known to be too old for update connections (build 0).
    func refresh(_ target: ClientTarget) {
        let name = target.label
        guard HostUpdates.signsInQuietly(target), builds[name] != 0, channels[name] == nil else { return }
        if let last = lastRefresh[name], Date().timeIntervalSince(last) < 300 { return }
        lastRefresh[name] = Date()
        open(target) { [weak self] channel in
            self?.note(build: channel.peerBuild, for: target.label)
            channel.close(.closed("checked build"))
        }
    }

    /// Update bb1 on that Mac to `build` (it downloads it from GitHub itself) and follow along.
    func update(_ target: ClientTarget, to build: Int, finished: ((Bool) -> Void)? = nil) {
        let name = target.label
        guard phases[name].map({ $0.finished }) ?? true else { return }
        Log.info("update: asking \(name) to update to build \(build)")
        set(name, .starting)
        let channel = open(target, ready: { [weak self] channel in
            guard let self else { return }
            self.note(build: channel.peerBuild, for: name)
            if channel.peerBuild >= build {
                self.set(name, .done(channel.peerBuild))
                channel.close(.closed("already up to date"))
                finished?(true)
            } else {
                channel.send(.updateHost(build: build))
            }
        }, closed: { [weak self] why in
            guard let self else { return }
            switch self.phases[name] {
            case .restarting?:
                self.set(name, .waiting)
                self.waitForRestart(target, build: build, deadline: Date().addingTimeInterval(120), finished: finished)
            case let phase? where !phase.finished:
                self.set(name, .failed(why))
                finished?(false)
            default:
                break
            }
        })
        channel?.onMessage = { [weak self, weak channel] message in
            guard let self, case .updateStatus(let status) = message else { return }
            switch status.phase {
            case "downloading": self.set(name, .downloading(status.progress ?? 0))
            case "installing": self.set(name, .installing)
            case "restarting": self.set(name, .restarting)
            case "upToDate":
                self.set(name, .done(max(build, self.builds[name] ?? 0)))
                channel?.close(.closed("up to date"))
                finished?(true)
            default:
                self.set(name, .failed(status.detail ?? "the update failed"))
                channel?.close(.closed("update failed"))
                finished?(false)
            }
        }
    }

    /// After the host quits to install, reconnect every 2 s until it answers with the new build.
    private func waitForRestart(_ target: ClientTarget, build: Int, deadline: Date, finished: ((Bool) -> Void)?) {
        let name = target.label
        guard Date() < deadline else {
            set(name, .failed("\(name) didn't come back after updating"))
            finished?(false)
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let self, self.phases[name] == .waiting else { return }
            var answered = false
            self.open(target, ready: { [weak self] channel in
                answered = true
                guard let self else { return }
                self.note(build: channel.peerBuild, for: name)
                channel.close(.closed("checked build"))
                if channel.peerBuild >= build {
                    Log.info("update: \(name) is back on build \(channel.peerBuild)")
                    self.set(name, .done(channel.peerBuild))
                    finished?(true)
                } else {
                    self.set(name, .failed("\(name) restarted but is still on build \(channel.peerBuild)"))
                    finished?(false)
                }
            }, closed: { [weak self] _ in
                guard let self, !answered else { return }
                self.waitForRestart(target, build: build, deadline: deadline, finished: finished)
            })
        }
    }

    /// Clear a finished update's state (so the card goes back to normal).
    func dismiss(_ host: String) {
        guard phases[host]?.finished == true else { return }
        phases[host] = nil
        changed()
    }

    // MARK: Plumbing

    private func set(_ host: String, _ phase: Phase) {
        guard phases[host] != phase else { return }
        phases[host] = phase
        changed()
        if case .done = phase {
            // Show "Updated" for a while, then go back to the normal card.
            DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self] in
                if case .done? = self?.phases[host] { self?.dismiss(host) }
            }
        }
    }

    private func changed() { NotificationCenter.default.post(name: HostUpdates.changed, object: self) }

    /// A signed-in connection with purpose "update". `closed` runs once, unless `ready` closed it itself.
    @discardableResult
    private func open(_ target: ClientTarget, ready: @escaping (ControlChannel) -> Void,
                      closed: ((String) -> Void)? = nil) -> ControlChannel? {
        let name = target.label
        let conn = NWConnection(to: target.endpoint, using: ControlChannel.tcpParameters())
        let channel = ControlChannel(connection: conn, role: .client(
            clientName: Host.current().localizedName ?? "Mac", deviceId: DeviceIdentity.id, auth: target.auth,
            askCode: { $0(nil) }), queue: .main)
        channel.purpose = "update"
        channels[name] = channel
        var readied = false
        channel.onAuthenticated = { [weak channel] _ in
            guard let channel else { return }
            readied = true
            ready(channel)
        }
        channel.onClose = { [weak self, weak channel] reason in
            if let self, self.channels[name] === channel { self.channels[name] = nil }
            let why: String
            switch reason {
            case .rejected(let r): why = "couldn't sign in: \(r)"
            case .closed(let r): why = r
            }
            // A probe that `ready` closed on purpose doesn't count as a failure.
            if readied, why.hasPrefix("checked build") || why.hasPrefix("already up to date") { return }
            closed?(why)
        }
        channel.start()
        return channel
    }
}
