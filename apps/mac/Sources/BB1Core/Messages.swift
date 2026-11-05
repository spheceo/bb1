import Foundation

public enum VideoCodec: UInt8, Codable {
    case h264 = 1
    case hevc = 2

    public var name: String { self == .hevc ? "HEVC" : "H.264" }
    /// The name used in `ClientHello.codecs`.
    public var id: String { self == .hevc ? "hevc" : "h264" }
}

public struct ClientHello: Codable {
    public var version: Int
    public var nonce: Data
    public var name: String
    // Version 4
    public var deviceId: String?
    /// "device" (remembered pairing), "tailscale", "code" (new pairing), "key" (old pairing key)
    public var mode: String?
    public var ecdhPub: Data?
    // Added with self-update (older peers leave these out)
    /// The app's build number.
    public var appBuild: Int?
    /// "update": not a streaming session, only for checking the host's build and updating it.
    public var purpose: String?
    /// Codecs the client can decode ("hevc", "h264"). nil = both (every Mac client).
    /// The Windows client sends ["h264"]: Windows has no built-in HEVC decoder.
    public var codecs: [String]?
}

public struct HostHello: Codable {
    public var version: Int
    public var nonce: Data
    public var proof: Data
    public var name: String
    // Version 4
    public var hostId: String?
    public var ecdhPub: Data?
    /// The app's build number (added with self-update).
    public var appBuild: Int?
}

/// Host → client: a small picture of the host's main screen, for the dashboard.
public struct PreviewMsg: Codable {
    /// nil when there's no picture (for example, no Screen Recording permission yet).
    public var jpeg: Data?
    /// Time the host spent capturing, so the client can work out the network round trip.
    public var captureMs: Double
    public init(jpeg: Data?, captureMs: Double) {
        self.jpeg = jpeg; self.captureMs = captureMs
    }
}

/// Host → client while the host updates itself.
public struct UpdateStatusMsg: Codable {
    /// "downloading", "installing", "restarting" or "failed"
    public var phase: String
    /// 0…1 while downloading
    public var progress: Double?
    public var detail: String?
    public init(phase: String, progress: Double? = nil, detail: String? = nil) {
        self.phase = phase; self.progress = progress; self.detail = detail
    }
}

/// Host → client after a new pairing: remember this host with this secret from now on.
public struct PairedMsg: Codable {
    public var hostId: String
    public var hostName: String
    public var secret: Data
    public init(hostId: String, hostName: String, secret: Data) {
        self.hostId = hostId; self.hostName = hostName; self.secret = secret
    }
}

public struct StreamConfig: Codable, Equatable {
    public var width: Int
    public var height: Int
    public var codec: VideoCodec
    public var fps: Int
    public var bitrateMbps: Int
    public var lowLatencyEncoder: Bool
    public var udpPort: UInt16
    public var sessionId: UInt32
    public var epoch: UInt16

    public init(width: Int, height: Int, codec: VideoCodec, fps: Int, bitrateMbps: Int,
                lowLatencyEncoder: Bool, udpPort: UInt16, sessionId: UInt32, epoch: UInt16) {
        self.width = width; self.height = height; self.codec = codec; self.fps = fps
        self.bitrateMbps = bitrateMbps; self.lowLatencyEncoder = lowLatencyEncoder
        self.udpPort = udpPort; self.sessionId = sessionId; self.epoch = epoch
    }
}

/// Client → host every 250 ms. Carries the client's frame counters (see docs/04-reliability.md).
public struct PingMsg: Codable {
    public var seq: UInt32
    public var clientUs: UInt64
    public var received: UInt64
    public var decoded: UInt64
    public var displayed: UInt64
    /// The client's latest round trip in ms, or how long its oldest unanswered ping has waited if that's
    /// longer. A host uses it to notice the client's line backing up early. Older peers leave it out.
    public var rttMs: Double?

    public init(seq: UInt32, clientUs: UInt64, received: UInt64, decoded: UInt64, displayed: UInt64, rttMs: Double? = nil) {
        self.seq = seq; self.clientUs = clientUs
        self.received = received; self.decoded = decoded; self.displayed = displayed; self.rttMs = rttMs
    }
}

/// Host → client reply, with the host's frame counters.
public struct PongMsg: Codable {
    public var seq: UInt32
    public var clientUs: UInt64
    public var hostUs: UInt64
    public var captured: UInt64
    public var encoded: UInt64
    public var sent: UInt64
    public var encodeAvgUs: UInt64
    public var hostRecoveries: UInt64

    public init(seq: UInt32, clientUs: UInt64, hostUs: UInt64, captured: UInt64, encoded: UInt64,
                sent: UInt64, encodeAvgUs: UInt64, hostRecoveries: UInt64) {
        self.seq = seq; self.clientUs = clientUs; self.hostUs = hostUs
        self.captured = captured; self.encoded = encoded; self.sent = sent
        self.encodeAvgUs = encodeAvgUs; self.hostRecoveries = hostRecoveries
    }
}

/// Host → client: the cursor macOS is showing right now (arrow, pointing hand, I-beam…), so the
/// client's own cursor can take the same shape. Size and hot spot are in points.
public struct CursorMsg: Codable {
    public var png: Data
    public var width: Double
    public var height: Double
    public var hotX: Double
    public var hotY: Double
    public init(png: Data, width: Double, height: Double, hotX: Double, hotY: Double) {
        self.png = png; self.width = width; self.height = height; self.hotX = hotX; self.hotY = hotY
    }
}

/// Clipboard contents copied on one side, to be pasted on the other.
public struct ClipboardMsg: Codable {
    public var text: String?
    public var png: Data?
    public init(text: String?, png: Data?) { self.text = text; self.png = png }
}

/// Client → host: resend these packets of a frame (`indices` nil = the whole frame).
public struct NackMsg: Codable {
    public var frameId: UInt32
    public var indices: [UInt16]?
    public init(frameId: UInt32, indices: [UInt16]?) { self.frameId = frameId; self.indices = indices }
}

public struct KeyMsg: Codable {
    public var code: UInt16
    public var down: Bool
    public var isRepeat: Bool
    public init(code: UInt16, down: Bool, isRepeat: Bool) { self.code = code; self.down = down; self.isRepeat = isRepeat }
}

/// Positions are 0...1 relative to the remote display, top-left origin.
public struct MouseMoveMsg: Codable {
    public var x: Double
    public var y: Double
    public init(x: Double, y: Double) { self.x = x; self.y = y }
}

public struct MouseButtonMsg: Codable {
    /// 0 left, 1 right, 2+ other
    public var button: Int
    public var down: Bool
    public var clicks: Int
    public var x: Double
    public var y: Double
    public init(button: Int, down: Bool, clicks: Int, x: Double, y: Double) {
        self.button = button; self.down = down; self.clicks = clicks; self.x = x; self.y = y
    }
}

public struct ScrollMsg: Codable {
    public var dx: Double
    public var dy: Double
    public var precise: Bool
    /// NSEvent.Phase raw values
    public var phase: UInt
    public var momentum: UInt
    public init(dx: Double, dy: Double, precise: Bool, phase: UInt, momentum: UInt) {
        self.dx = dx; self.dy = dy; self.precise = precise; self.phase = phase; self.momentum = momentum
    }
}

/// One of the host's screens.
public struct DisplayInfo: Codable, Equatable {
    /// Stable for as long as the screen is connected ("69733382" on a Mac, "\\\\.\\DISPLAY3" on Windows).
    public var id: String
    public var name: String
    public var width: Int
    public var height: Int
    public var primary: Bool
    public init(id: String, name: String, width: Int, height: Int, primary: Bool) {
        self.id = id; self.name = name; self.width = width; self.height = height; self.primary = primary
    }
}

/// Host → client: the host's screens and which one is shared. Sent after welcome and when either changes.
public struct DisplaysMsg: Codable, Equatable {
    public var displays: [DisplayInfo]
    public var current: String
    public init(displays: [DisplayInfo], current: String) { self.displays = displays; self.current = current }
}

/// Client → host: one game controller's state (sent when it changes). Xbox layout, whatever the
/// controller: `buttons` uses XInput's wButtons bits (A 0x1000, B 0x2000, X 0x4000, Y 0x8000, LB 0x100,
/// RB 0x200, Back 0x20, Start 0x10, Guide 0x400, L3 0x40, R3 0x80, D-pad up 0x1 down 0x2 left 0x4 right 0x8).
public struct GamepadMsg: Codable, Equatable {
    /// The client's controller slot, 0…3. Each becomes its own virtual controller on the host.
    public var pad: Int
    /// false: the controller was unplugged (release its virtual one).
    public var connected: Bool
    public var buttons: Int
    /// Triggers 0…255.
    public var lt: Int
    public var rt: Int
    /// Sticks −32768…32767, up and right positive (XInput's convention).
    public var lx: Int
    public var ly: Int
    public var rx: Int
    public var ry: Int
    /// "DualSense Wireless Controller", for the host's log and list.
    public var name: String?
    public init(pad: Int, connected: Bool, buttons: Int, lt: Int, rt: Int, lx: Int, ly: Int, rx: Int, ry: Int, name: String?) {
        self.pad = pad; self.connected = connected; self.buttons = buttons; self.lt = lt; self.rt = rt
        self.lx = lx; self.ly = ly; self.rx = rx; self.ry = ry; self.name = name
    }
}

/// Host → client: the game wants this controller to rumble (motor strengths 0…255; 0, 0 = stop).
public struct RumbleMsg: Codable, Equatable {
    public var pad: Int
    public var large: Int
    public var small: Int
    public init(pad: Int, large: Int, small: Int) { self.pad = pad; self.large = large; self.small = small }
}

public enum ControlMessage: Codable {
    // Handshake (plaintext)
    case clientHello(ClientHello)
    case hostHello(HostHello)
    case clientProof(Data)
    case authFailed(String)

    // Everything below is sealed
    case welcome(hostName: String)
    case streamConfig(StreamConfig)
    case requestKeyframe(reason: String)
    case ping(PingMsg)
    case pong(PongMsg)
    case key(KeyMsg)
    case mouseMove(MouseMoveMsg)
    case mouseButton(MouseButtonMsg)
    case scroll(ScrollMsg)
    case releaseAll
    case setCursorInVideo(Bool)
    case notice(String)
    case bye(String)

    // Version 2
    case nack(NackMsg)
    case clientLog(String)
    case clipboard(ClipboardMsg)

    // Version 3
    /// Client → host: send (true) or stop sending (false) the host's sound.
    case setAudio(Bool)

    // Version 4
    case paired(PairedMsg)
    case cursor(CursorMsg)
    /// Client → host: Caps Lock is on/off on the client; match it.
    case capsLock(Bool)

    // Self-update: only sent to peers that announced an appBuild
    /// Client → host: download this build from GitHub, install it and restart.
    case updateHost(build: Int)
    case updateStatus(UpdateStatusMsg)

    // Screen previews (build 15+), on "update" connections
    /// Client → host: send a picture of your screen, at most this many pixels wide.
    case requestPreview(maxWidth: Int)
    case preview(PreviewMsg)

    // Screens and game controllers (unknown messages are ignored, so older peers don't mind)
    case displays(DisplaysMsg)
    /// Client → host: share this screen instead (an id from `displays`).
    case selectDisplay(id: String)
    case gamepad(GamepadMsg)
    case rumble(RumbleMsg)
}
