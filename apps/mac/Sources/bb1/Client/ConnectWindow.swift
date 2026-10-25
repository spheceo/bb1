import AppKit
import BB1Core
import Network

/// Remembers the last host and its pairing key in ~/Library/Application Support/bb1/client.json (mode 0600).
enum ScaleMode: String, Codable, CaseIterable {
    case fit, stretch, actual
    var title: String {
        switch self {
        case .fit: return "Fit (Black Bars)"
        case .stretch: return "Stretch to Fill"
        case .actual: return "Actual Pixels (1:1)"
        }
    }
}

enum InputMode: String, Codable, CaseIterable {
    case all, keyboard, mouse, none
    var title: String {
        switch self {
        case .all: return "Keyboard & Mouse"
        case .keyboard: return "Keyboard Only"
        case .mouse: return "Mouse Only"
        case .none: return "View Only"
        }
    }
    var keyboard: Bool { self == .all || self == .keyboard }
    var mouse: Bool { self == .all || self == .mouse }
}

struct ClientSettings: Codable {
    var host = ""
    var pairingKey = ""
    var invertScroll = false
    var scale = ScaleMode.fit
    var input = InputMode.all
    var sound = true
    /// Use this Mac's own cursor over the stream (instant), shaped like the host's. Default on.
    var localCursor = true
    var cursorInVideo: Bool {
        get { !localCursor }
        set { localCursor = !newValue }
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        host = try c.decodeIfPresent(String.self, forKey: .host) ?? ""
        pairingKey = try c.decodeIfPresent(String.self, forKey: .pairingKey) ?? ""
        invertScroll = try c.decodeIfPresent(Bool.self, forKey: .invertScroll) ?? false
        scale = (try? c.decodeIfPresent(ScaleMode.self, forKey: .scale)) ?? .fit
        input = (try? c.decodeIfPresent(InputMode.self, forKey: .input)) ?? .all
        sound = try c.decodeIfPresent(Bool.self, forKey: .sound) ?? true
        localCursor = try c.decodeIfPresent(Bool.self, forKey: .localCursor) ?? true
    }

    private static var url: URL { BB1.supportDirectory().appendingPathComponent("client.json") }

    static func load() -> ClientSettings {
        guard let data = try? Data(contentsOf: url),
              let s = try? JSONDecoder().decode(ClientSettings.self, from: data) else { return ClientSettings() }
        return s
    }

    func save() {
        guard let data = try? JSONEncoder().encode(self) else { return }
        try? data.write(to: ClientSettings.url, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: ClientSettings.url.path)
    }
}

/// "host", "host:port", "[v6]:port", or "192.168.1.5"
func parseEndpoint(_ text: String) -> NWEndpoint? {
    var host = text.trimmingCharacters(in: .whitespaces)
    var port = BB1.controlPort
    guard !host.isEmpty else { return nil }
    if host.hasPrefix("["), let close = host.firstIndex(of: "]") {
        let rest = host[host.index(after: close)...]
        if rest.hasPrefix(":"), let p = UInt16(rest.dropFirst()) { port = p }
        host = String(host[host.index(after: host.startIndex)..<close])
    } else if host.filter({ $0 == ":" }).count == 1, let colon = host.firstIndex(of: ":") {
        if let p = UInt16(host[host.index(after: colon)...]) { port = p }
        host = String(host[..<colon])
    }
    return .hostPort(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port)!)
}
