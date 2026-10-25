import Foundation

/// What's inside one sealed video frame: codec, parameter sets on keyframes, then AVCC sample data
/// (NAL units with 4-byte big-endian length prefixes, as VideoToolbox produces them).
public struct FramePayload {
    public var codec: VideoCodec
    public var parameterSets: [Data]
    public var sampleData: Data

    public init(codec: VideoCodec, parameterSets: [Data], sampleData: Data) {
        self.codec = codec; self.parameterSets = parameterSets; self.sampleData = sampleData
    }

    public func encode() -> Data {
        var d = Data(capacity: sampleData.count + 64)
        d.append(codec.rawValue)
        d.append(UInt8(parameterSets.count))
        for p in parameterSets {
            d.appendLE(UInt16(p.count))
            d.append(p)
        }
        d.append(sampleData)
        return d
    }

    public static func decode(_ data: Data) -> FramePayload? {
        let d = Data(data)
        guard d.count >= 2, let codec = VideoCodec(rawValue: d[0]) else { return nil }
        var offset = 2
        var sets: [Data] = []
        for _ in 0..<Int(d[1]) {
            guard offset + 2 <= d.count else { return nil }
            let len = Int(d.readLE(offset, as: UInt16.self))
            offset += 2
            guard offset + len <= d.count else { return nil }
            sets.append(d.subdata(in: offset..<offset + len))
            offset += len
        }
        return FramePayload(codec: codec, parameterSets: sets, sampleData: d.subdata(in: offset..<d.count))
    }
}

/// 20-byte header on every video datagram (docs/02-protocol.md).
public struct VideoPacketHeader {
    public static let type: UInt8 = 0x01
    public static let size = 20
    public static let keyframeFlag: UInt8 = 1 << 0
    public static let refreshFlag: UInt8 = 1 << 1

    public var flags: UInt8
    public var epoch: UInt16
    public var frameId: UInt32
    public var index: UInt16
    public var count: UInt16
    public var captureUs: UInt64

    public var isKeyframe: Bool { flags & VideoPacketHeader.keyframeFlag != 0 }

    func write(into d: inout Data) {
        d.append(VideoPacketHeader.type)
        d.append(flags)
        d.appendLE(epoch)
        d.appendLE(frameId)
        d.appendLE(index)
        d.appendLE(count)
        d.appendLE(captureUs)
    }

    public static func parse(_ d: Data) -> VideoPacketHeader? {
        guard d.count > size, d.first == type else { return nil }
        return VideoPacketHeader(flags: d[d.startIndex + 1],
                                 epoch: d.readLE(2), frameId: d.readLE(4),
                                 index: d.readLE(8), count: d.readLE(10), captureUs: d.readLE(12))
    }

    /// Authenticated (not encrypted) frame metadata.
    public static func aad(flags: UInt8, epoch: UInt16, frameId: UInt32, captureUs: UInt64) -> Data {
        var d = Data([flags])
        d.appendLE(epoch)
        d.appendLE(frameId)
        d.appendLE(captureUs)
        return d
    }
}

public enum Packetizer {
    public static let chunkSize = BB1.maxDatagram - VideoPacketHeader.size

    public static func packets(sealed: Data, flags: UInt8, epoch: UInt16, frameId: UInt32, captureUs: UInt64) -> [Data] {
        let count = (sealed.count + chunkSize - 1) / chunkSize
        precondition(count <= Int(UInt16.max), "frame too large")
        var out: [Data] = []
        out.reserveCapacity(count)
        var offset = sealed.startIndex
        for i in 0..<count {
            let end = min(offset + chunkSize, sealed.endIndex)
            var p = Data(capacity: VideoPacketHeader.size + end - offset)
            VideoPacketHeader(flags: flags, epoch: epoch, frameId: frameId, index: UInt16(i),
                              count: UInt16(count), captureUs: captureUs).write(into: &p)
            p.append(sealed[offset..<end])
            out.append(p)
            offset = end
        }
        return out
    }
}

/// Collects datagrams into whole frames. Not thread-safe; use from one queue.
/// Frames may complete out of order (after a retransmit); the caller puts them back in order.
public final class Reassembler {
    public struct Frame {
        public let flags: UInt8
        public let epoch: UInt16
        public let frameId: UInt32
        public let captureUs: UInt64
        public let sealed: Data
        public let firstPacketUs: UInt64
        public let wasRetransmitted: Bool
        public var isKeyframe: Bool { flags & VideoPacketHeader.keyframeFlag != 0 }
        public var aad: Data { VideoPacketHeader.aad(flags: flags, epoch: epoch, frameId: frameId, captureUs: captureUs) }
    }

    /// Something to ask the host to resend.
    public struct Request {
        public let frameId: UInt32
        public let indices: [UInt16]?  // nil = whole frame (we never saw any of it)
    }

    private struct Partial {
        var header: VideoPacketHeader
        var chunks: [Data?]
        var received: Int
        var firstUs: UInt64
        var lastPacketUs: UInt64
        var nacks = 0
        var lastNackUs: UInt64 = 0
    }

    private struct Unseen {
        var sinceUs: UInt64
        var nacks = 0
        var lastNackUs: UInt64 = 0
    }

    private var partials: [UInt32: Partial] = [:]
    private var unseen: [UInt32: Unseen] = [:]
    private var completed = Set<UInt32>()
    private var highestSeen: UInt32?
    /// Frames at or below this are no longer wanted (already shown or skipped).
    public var floor: UInt32 = 0 {
        didSet {
            partials = partials.filter { $0.key > floor }
            unseen = unseen.filter { $0.key > floor }
            completed = completed.filter { $0 > floor }
        }
    }
    public private(set) var droppedIncomplete: UInt64 = 0

    /// Something is half-received or missing (so it's worth checking for resends).
    public var hasPending: Bool { !partials.isEmpty || !unseen.isEmpty }

    public init() {}

    public func reset() {
        partials.removeAll()
        unseen.removeAll()
        completed.removeAll()
        highestSeen = nil
        floor = 0
    }

    public func add(_ datagram: Data, nowUs: UInt64) -> Frame? {
        guard let h = VideoPacketHeader.parse(datagram), h.count > 0, h.index < h.count else { return nil }
        guard h.frameId > floor, !completed.contains(h.frameId) else { return nil }
        // Frames skipped between the newest one seen and this one were lost entirely.
        if let high = highestSeen, h.frameId > high &+ 1, h.frameId &- high < 64 {
            var id = high &+ 1
            while id != h.frameId {
                if partials[id] == nil && !completed.contains(id) { unseen[id] = Unseen(sinceUs: nowUs) }
                id &+= 1
            }
        }
        if highestSeen == nil || h.frameId > highestSeen! { highestSeen = h.frameId }
        let wasUnseen = unseen.removeValue(forKey: h.frameId) != nil

        var p = partials[h.frameId] ?? Partial(header: h, chunks: Array(repeating: nil, count: Int(h.count)),
                                               received: 0, firstUs: nowUs, lastPacketUs: nowUs)
        if wasUnseen { p.nacks = max(p.nacks, 1) }
        guard p.chunks.count == Int(h.count), p.chunks[Int(h.index)] == nil else { return nil }
        p.chunks[Int(h.index)] = datagram.dropFirst(VideoPacketHeader.size)
        p.received += 1
        p.lastPacketUs = nowUs
        guard p.received == p.chunks.count else {
            partials[h.frameId] = p
            return nil
        }
        partials[h.frameId] = nil
        completed.insert(h.frameId)
        if completed.count > 512 { completed = completed.filter { h.frameId &- $0 < 256 } }
        var sealed = Data(capacity: p.chunks.count * Packetizer.chunkSize)
        for c in p.chunks { sealed.append(c!) }
        return Frame(flags: h.flags, epoch: h.epoch, frameId: h.frameId, captureUs: h.captureUs,
                     sealed: sealed, firstPacketUs: p.firstUs, wasRetransmitted: p.nacks > 0)
    }

    /// What to ask the host to resend: frames that went quiet with packets missing, and frames we never saw.
    /// Each item is asked for at most `maxTries` times, `retryUs` apart.
    public func requests(nowUs: UInt64, quietUs: UInt64, retryUs: UInt64, maxTries: Int) -> [Request] {
        var out: [Request] = []
        for (id, var p) in partials where nowUs &- p.lastPacketUs > quietUs && p.nacks < maxTries
            && (p.nacks == 0 || nowUs &- p.lastNackUs > retryUs) {
            let missing = p.chunks.indices.filter { p.chunks[$0] == nil }.map { UInt16($0) }
            p.nacks += 1
            p.lastNackUs = nowUs
            partials[id] = p
            out.append(Request(frameId: id, indices: missing))
        }
        for (id, var u) in unseen where u.nacks < maxTries && (u.nacks == 0 || nowUs &- u.lastNackUs > retryUs) {
            u.nacks += 1
            u.lastNackUs = nowUs
            unseen[id] = u
            out.append(Request(frameId: id, indices: nil))
        }
        return out
    }

    /// Drops frames that have been incomplete for too long. Returns how many were dropped.
    @discardableResult
    public func expire(nowUs: UInt64, maxAgeUs: UInt64) -> Int {
        let old = partials.filter { nowUs &- $0.value.firstUs > maxAgeUs }.map(\.key)
        for id in old { partials[id] = nil }
        unseen = unseen.filter { nowUs &- $0.value.sinceUs <= maxAgeUs }
        droppedIncomplete += UInt64(old.count)
        return old.count
    }
}
