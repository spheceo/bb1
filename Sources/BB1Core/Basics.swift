import Foundation

public enum BB1 {
    /// The app version (from the VERSION file, stamped into Info.plist by scripts/bundle.sh). "dev" for `swift run`.
    public static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }

    /// The build number (commit count, stamped into Info.plist as CFBundleVersion). Updates compare this.
    /// 0 for `swift run`, which can't update itself.
    public static var build: Int {
        (Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String).flatMap(Int.init) ?? 0
    }

    /// Builds after this one are nightlies, numbered from 1 (build 26 = nightly-1). Updates still compare
    /// build numbers; the number in the name is just for people.
    public static let nightlyBase = 25

    /// "nightly-3" (the test builds before nightlies: "build 25").
    public static func buildName(_ build: Int) -> String {
        build > nightlyBase ? "nightly-\(build - nightlyBase)" : "build \(build)"
    }

    /// A proper release (stamped into Info.plist by scripts/bundle.sh), not a nightly.
    public static var isRelease: Bool { Bundle.main.object(forInfoDictionaryKey: "BB1Release") as? Bool ?? false }

    /// "bb1 0.0.1" for a release, "bb1 nightly-3" for a nightly.
    public static var displayName: String { isRelease ? "bb1 \(version)" : "bb1 \(buildName(build))" }

    /// 2: packet retransmission (NACK), client log forwarding, clipboard. 3: sound.
    /// 4: remembered pairings, Tailscale sign-in, 6-digit pairing codes, forward-secret key exchange.
    /// Hosts still accept version 1–3 clients (pairing key); newer features need both sides.
    public static let protocolVersion = 4
    public static let minimumProtocolVersion = 1
    public static let controlPort: UInt16 = 47800
    public static let videoPort: UInt16 = 47801
    public static let bonjourType = "_bb1._tcp"
    /// Largest UDP datagram we send. Fits Tailscale's 1280 MTU with room for IP/UDP headers.
    public static let maxDatagram = 1200

    /// ~/Library/Application Support/bb1 (`BB1_SUPPORT_DIR` overrides it, so tests don't touch real pairings).
    public static func supportDirectory() -> URL {
        let dir = ProcessInfo.processInfo.environment["BB1_SUPPORT_DIR"].map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/bb1", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
}

// MARK: - Logging

public enum Log {
    private static let lock = NSLock()
    private static var handle: FileHandle?
    private static var written: UInt64 = 0
    /// A log over this size moves to <name>.old.log and starts again (checked at start-up and while running).
    private static let maxBytes: UInt64 = 20_000_000
    public private(set) static var fileURL: URL?
    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS"
        return f
    }()

    /// Called with every line (without the timestamp). Used to forward client logs to the host.
    public static var sink: ((String) -> Void)?

    /// Logs to stderr and ~/Library/Logs/bb1/<name>.log. `BB1_LOG_NAME` overrides the name (tests use it).
    public static func setup(_ name: String) {
        let name = ProcessInfo.processInfo.environment["BB1_LOG_NAME"] ?? name
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/bb1", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("\(name).log")
        lock.lock()
        open(url)
        lock.unlock()
    }

    public static func info(_ message: String) {
        lock.lock()
        write(message)
        let sink = self.sink
        lock.unlock()
        sink?(message)
    }

    /// Writes to the log file only (no forwarding). For lines that came from the other side.
    public static func local(_ message: String) {
        lock.lock()
        write(message)
        lock.unlock()
    }

    /// Lock held.
    private static func write(_ message: String) {
        let line = Data("\(formatter.string(from: Date())) \(message)\n".utf8)
        FileHandle.standardError.write(line)
        handle?.write(line)
        written += UInt64(line.count)
        if written > maxBytes, let url = fileURL { open(url) }
    }

    /// Lock held. Opens the log for appending, moving a full one aside first.
    private static func open(_ url: URL) {
        let fm = FileManager.default
        try? handle?.close()
        handle = nil
        let size = ((try? fm.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.uint64Value ?? 0
        if size > maxBytes {
            let old = url.deletingPathExtension().appendingPathExtension("old.log")
            try? fm.removeItem(at: old)
            if (try? fm.moveItem(at: url, to: old)) == nil { try? fm.removeItem(at: url) }
        }
        if !fm.fileExists(atPath: url.path) { fm.createFile(atPath: url.path, contents: nil) }
        handle = try? FileHandle(forWritingTo: url)
        written = handle.map { $0.seekToEndOfFile() } ?? 0
        fileURL = url
    }
}

// MARK: - Clock

/// Microseconds on the host clock (mach_absolute_time), the same clock ScreenCaptureKit stamps frames with.
public enum Clock {
    private static let timebase: mach_timebase_info_data_t = {
        var t = mach_timebase_info_data_t()
        mach_timebase_info(&t)
        return t
    }()

    public static func nowUs() -> UInt64 { machToUs(mach_absolute_time()) }

    public static func machToUs(_ ticks: UInt64) -> UInt64 {
        ticks * UInt64(timebase.numer) / UInt64(timebase.denom) / 1000
    }
}

// MARK: - Locking

public final class Locked<Value> {
    private var value: Value
    private let lock = NSLock()

    public init(_ value: Value) { self.value = value }

    @discardableResult
    public func with<R>(_ body: (inout Value) -> R) -> R {
        lock.lock()
        defer { lock.unlock() }
        return body(&value)
    }

    public var get: Value { with { $0 } }
}

// MARK: - Little-endian bytes

extension Data {
    mutating func appendLE<T: FixedWidthInteger>(_ value: T) {
        var v = value.littleEndian
        Swift.withUnsafeBytes(of: &v) { append(contentsOf: $0) }
    }

    func readLE<T: FixedWidthInteger>(_ offset: Int, as: T.Type = T.self) -> T {
        withUnsafeBytes { T(littleEndian: $0.loadUnaligned(fromByteOffset: offset, as: T.self)) }
    }
}
