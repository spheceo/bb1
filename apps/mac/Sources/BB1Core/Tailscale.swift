import Foundation

/// Reads from the local Tailscale app via its command-line tool. Everything here blocks for up to a
/// few seconds, so call it off the main thread.
public enum Tailscale {
    public struct Peer {
        public let name: String
        public let ip: String
        public let online: Bool
        public let login: String
        public let os: String
        /// Non-nil when Tailscale has a direct path (e.g. "192.168.3.77:48994").
        public let directAddress: String?
        /// The relay region it falls back to (e.g. "jnb").
        public let relay: String?
    }

    public struct Status {
        public let selfLogin: String
        public let selfDisplayName: String
        public let selfPictureURL: URL?
        public let selfName: String
        public let selfIPs: [String]
        public let peers: [Peer]
    }

    private static let candidates = [
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ]

    public static var isInstalled: Bool { cli != nil }

    private static var cli: String? { candidates.first { FileManager.default.isExecutableFile(atPath: $0) } }

    private static func run(_ args: [String], timeout: TimeInterval = 4) -> Data? {
        guard let cli else { return nil }
        let task = Process()
        task.executableURL = URL(fileURLWithPath: cli)
        task.arguments = args
        let out = Pipe()
        task.standardOutput = out
        task.standardError = Pipe()
        do { try task.run() } catch { return nil }
        let data = Locked(Data())
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            let d = out.fileHandleForReading.readDataToEndOfFile()
            data.with { $0 = d }
            done.signal()
        }
        if done.wait(timeout: .now() + timeout) == .timedOut {
            task.terminate()
            return nil
        }
        task.waitUntilExit()
        return task.terminationStatus == 0 ? data.get : nil
    }

    public static func status() -> Status? {
        guard let data = run(["status", "--json"]),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let me = root["Self"] as? [String: Any] else { return nil }
        let users = root["User"] as? [String: Any] ?? [:]
        func login(_ node: [String: Any]) -> String {
            let id = (node["UserID"] as? NSNumber)?.stringValue ?? "\(node["UserID"] ?? "")"
            return (users[id] as? [String: Any])?["LoginName"] as? String ?? ""
        }
        let peers = (root["Peer"] as? [String: Any] ?? [:]).values.compactMap { value -> Peer? in
            guard let p = value as? [String: Any], let ips = p["TailscaleIPs"] as? [String], let ip = ips.first else { return nil }
            let cur = p["CurAddr"] as? String ?? ""
            let relay = p["Relay"] as? String ?? ""
            return Peer(name: p["HostName"] as? String ?? ip, ip: ip, online: p["Online"] as? Bool ?? false,
                        login: login(p), os: p["OS"] as? String ?? "",
                        directAddress: cur.isEmpty ? nil : cur, relay: relay.isEmpty ? nil : relay)
        }
        let meID = (me["UserID"] as? NSNumber)?.stringValue ?? "\(me["UserID"] ?? "")"
        let profile = users[meID] as? [String: Any] ?? [:]
        return Status(selfLogin: login(me), selfDisplayName: profile["DisplayName"] as? String ?? login(me),
                      selfPictureURL: (profile["ProfilePicURL"] as? String).flatMap(URL.init(string:)),
                      selfName: me["HostName"] as? String ?? "",
                      selfIPs: me["TailscaleIPs"] as? [String] ?? [], peers: peers)
    }

    /// The Tailscale account that owns the device at this address, if it's on our tailnet.
    public static func whoisLogin(_ ip: String) -> String? {
        guard let data = run(["whois", "--json", ip]),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let profile = root["UserProfile"] as? [String: Any] else { return nil }
        return profile["LoginName"] as? String
    }

    /// 100.64.0.0/10 or fd7a:115c:a1e0::/48
    public static func isTailscaleIP(_ address: String) -> Bool {
        let a = address.split(separator: "%").first.map(String.init) ?? address
        if a.lowercased().hasPrefix("fd7a:115c:a1e0:") { return true }
        let parts = a.split(separator: ".").compactMap { Int($0) }
        return parts.count == 4 && parts[0] == 100 && (64...127).contains(parts[1])
    }

    /// "direct", "relayed via jnb", or nil if not a Tailscale peer.
    public static func pathDescription(to ip: String, in status: Status?) -> String? {
        guard let peer = status?.peers.first(where: { $0.ip == ip }) else { return nil }
        if peer.directAddress != nil { return "direct" }
        return "relayed\(peer.relay.map { " via \($0)" } ?? "")"
    }
}
