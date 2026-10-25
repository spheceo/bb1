import Foundation

/// This Mac's permanent bb1 identity (same ID whether it runs as host or client).
public enum DeviceIdentity {
    public static let id: String = {
        let url = BB1.supportDirectory().appendingPathComponent("device-id")
        if let text = try? String(contentsOf: url, encoding: .utf8) {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { return trimmed }
        }
        let id = UUID().uuidString
        try? id.write(to: url, atomically: true, encoding: .utf8)
        return id
    }()

    public static var name: String { Host.current().localizedName ?? "Mac" }

    public static func randomSecret() -> Data {
        var d = Data(count: 32)
        _ = d.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 32, $0.baseAddress!) }
        return d
    }
}

/// A small JSON list stored with 0600 permissions in ~/Library/Application Support/bb1.
public final class JSONStore<Item: Codable> {
    private let url: URL
    private let lock = NSLock()

    public init(_ filename: String) {
        url = BB1.supportDirectory().appendingPathComponent(filename)
    }

    public func load() -> [Item] {
        lock.lock()
        defer { lock.unlock() }
        guard let data = try? Data(contentsOf: url) else { return [] }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return (try? decoder.decode([Item].self, from: data)) ?? []
    }

    public func save(_ items: [Item]) {
        lock.lock()
        defer { lock.unlock() }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        guard let data = try? encoder.encode(items) else { return }
        try? data.write(to: url, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }
}

/// Host side: a client Mac that's allowed in without asking again.
public struct PairedDevice: Codable {
    public var id: String
    public var name: String
    public var secret: Data
    public var pairedAt: Date
    public var lastSeen: Date?
    /// How it was first trusted: "code", "tailscale" or "key".
    public var via: String

    public init(id: String, name: String, secret: Data, pairedAt: Date, lastSeen: Date?, via: String) {
        self.id = id; self.name = name; self.secret = secret; self.pairedAt = pairedAt; self.lastSeen = lastSeen; self.via = via
    }

    public static let store = JSONStore<PairedDevice>("paired-devices.json")
}

/// Client side: a host Mac this Mac has paired with.
public struct KnownHost: Codable {
    public var hostId: String
    public var name: String
    public var secret: Data
    public var tailscaleIP: String?
    /// The address typed when pairing ("Connect by Address"), if any.
    public var address: String?
    public var pairedAt: Date

    public init(hostId: String, name: String, secret: Data, tailscaleIP: String?, address: String?, pairedAt: Date) {
        self.hostId = hostId; self.name = name; self.secret = secret; self.tailscaleIP = tailscaleIP
        self.address = address; self.pairedAt = pairedAt
    }

    public static let store = JSONStore<KnownHost>("known-hosts.json")
}
