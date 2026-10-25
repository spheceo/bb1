import BB1Core
import Foundation
import Network

/// Every host this Mac can reach: found on the home network (Bonjour), on your Tailscale account,
/// or remembered from an earlier pairing. Merged by name, refreshed in the background.
final class HostDirectory {
    struct Entry {
        var name: String
        var lan: NWEndpoint?
        var tailscaleIP: String?
        /// On the same Tailscale account as this Mac, and bb1 is answering.
        var onMyTailscale = false
        var known: KnownHost?
        var online: Bool { lan != nil || onMyTailscale }

        var detail: String {
            var parts: [String] = []
            if known != nil { parts.append("Paired") } else if onMyTailscale { parts.append("Your Tailscale account") }
            if lan != nil { parts.append("home network") } else if onMyTailscale { parts.append("over Tailscale") }
            if !online { parts.append("offline") }
            if known == nil && !onMyTailscale && lan != nil { parts = ["Home network · not paired yet"] }
            return parts.joined(separator: " · ")
        }
    }

    /// Main thread.
    var onChange: (([Entry]) -> Void)?
    private(set) var entries: [Entry] = []

    private var browser: NWBrowser?
    private var bonjour: [String: NWEndpoint] = [:]
    private var tailscale: [(name: String, ip: String)] = []
    private var tailscaleLogin: String?
    private(set) var tailscaleName: String?
    private(set) var tailscalePicture: URL?
    private var timer: Timer?
    private var probing = false

    func start() {
        let browser = NWBrowser(for: .bonjour(type: BB1.bonjourType, domain: nil), using: .tcp)
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            var found: [String: NWEndpoint] = [:]
            for r in results {
                // Skip this Mac itself (Bonjour may add " (2)" if two copies advertise the same name).
                if case .service(let name, _, _, _) = r.endpoint, name != DeviceIdentity.name,
                   !name.hasPrefix(DeviceIdentity.name + " (") { found[name] = r.endpoint }
            }
            DispatchQueue.main.async {
                self?.bonjour = found
                self?.rebuild()
            }
        }
        browser.start(queue: .main)
        self.browser = browser
        refreshTailscale()
        timer = Timer.scheduledTimer(withTimeInterval: 10, repeats: true) { [weak self] _ in self?.refreshTailscale() }
    }

    func stop() {
        browser?.cancel()
        browser = nil
        timer?.invalidate()
        timer = nil
    }

    var hasTailscale: Bool { tailscaleLogin != nil }
    var tailscaleAccount: String? { tailscaleLogin }

    func refreshNow() { refreshTailscale() }

    /// Online Macs on my Tailscale account, kept only if bb1 answers on them.
    private func refreshTailscale() {
        guard !probing else { return }
        probing = true
        DispatchQueue.global().async {
            let status = Tailscale.status()
            let me = status?.selfLogin
            let candidates = (status?.peers ?? []).filter { $0.online && $0.os == "macOS" && $0.login == me }
            var reachable: [(String, String)] = []
            let group = DispatchGroup()
            let lock = NSLock()
            for peer in candidates {
                group.enter()
                HostDirectory.probe(peer.ip) { ok in
                    if ok { lock.lock(); reachable.append((peer.name, peer.ip)); lock.unlock() }
                    group.leave()
                }
            }
            group.wait()
            DispatchQueue.main.async {
                self.probing = false
                self.tailscaleLogin = me
                self.tailscaleName = status?.selfDisplayName
                self.tailscalePicture = status?.selfPictureURL
                self.tailscale = reachable
                self.rebuild()
            }
        }
    }

    /// Is bb1 listening there? (TCP connect with a short timeout.)
    private static func probe(_ ip: String, done: @escaping (Bool) -> Void) {
        let conn = NWConnection(host: NWEndpoint.Host(ip), port: NWEndpoint.Port(rawValue: BB1.controlPort)!, using: .tcp)
        let finished = Locked(false)
        func finish(_ ok: Bool) {
            guard finished.with({ f -> Bool in defer { f = true }; return !f }) else { return }
            conn.cancel()
            done(ok)
        }
        conn.stateUpdateHandler = { state in
            switch state {
            case .ready: finish(true)
            case .failed, .waiting: finish(false)
            default: break
            }
        }
        conn.start(queue: .global())
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) { finish(false) }
    }

    func rebuild() {
        var byName: [String: Entry] = [:]
        for host in KnownHost.store.load() {
            byName[host.name] = Entry(name: host.name, tailscaleIP: host.tailscaleIP, known: host)
        }
        for (name, endpoint) in bonjour {
            byName[name, default: Entry(name: name)].lan = endpoint
        }
        for (name, ip) in tailscale {
            var e = byName[name] ?? byName.values.first(where: { $0.known?.tailscaleIP == ip }) ?? Entry(name: name)
            e.tailscaleIP = ip
            e.onMyTailscale = true
            byName[e.name] = e
        }
        entries = byName.values.sorted { ($0.online ? 0 : 1, $0.name) < ($1.online ? 0 : 1, $1.name) }
        onChange?(entries)
    }

    /// The best way to sign in to this host, and over which network.
    static func target(for e: Entry, preferTailscale: Bool = false) -> ClientTarget? {
        let port = NWEndpoint.Port(rawValue: BB1.controlPort)!
        if let known = e.known {
            if let lan = e.lan, !(preferTailscale && e.tailscaleIP != nil) {
                return ClientTarget(endpoint: lan, label: e.name, auth: .device(secret: known.secret))
            }
            if let ip = e.tailscaleIP {
                return ClientTarget(endpoint: .hostPort(host: NWEndpoint.Host(ip), port: port), label: e.name,
                                    auth: .device(secret: known.secret), tailscaleIP: ip)
            }
            return nil
        }
        if e.onMyTailscale, let ip = e.tailscaleIP, preferTailscale || e.lan == nil || e.known == nil {
            return ClientTarget(endpoint: .hostPort(host: NWEndpoint.Host(ip), port: port), label: e.name,
                                auth: .tailscale, tailscaleIP: ip)
        }
        if let lan = e.lan {
            // Moving over from the old pairing key: use it once, and the host turns it into a pairing.
            let settings = ClientSettings.load()
            if settings.host == e.name, let key = PairingKey(string: settings.pairingKey) {
                return ClientTarget(endpoint: lan, label: e.name, auth: .legacyKey(key))
            }
            return ClientTarget(endpoint: lan, label: e.name, auth: .code)
        }
        return nil
    }
}
