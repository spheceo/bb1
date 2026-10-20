import BB1Core
import Foundation

// Quick checks for the protocol pieces that don't need a screen or a network.
// Run with: swift run bb1-selftest

var failures = 0
func check(_ name: String, _ condition: @autoclosure () throws -> Bool) {
    do {
        if try condition() { print("PASS  \(name)") } else { print("FAIL  \(name)"); failures += 1 }
    } catch {
        print("FAIL  \(name): \(error)"); failures += 1
    }
}

// Pairing key text round trip
let key = PairingKey.generate()
check("pairing key round trip", PairingKey(string: key.display) == key)
check("pairing key ignores case and dashes", PairingKey(string: key.display.lowercased().replacingOccurrences(of: "-", with: "")) == key)
check("pairing key rejects garbage", PairingKey(string: "hello") == nil)
check("pairing key format", key.display.count == 29)

// Handshake: both sides derive the same keys, wrong key fails
let cn = Handshake.randomNonce(), hn = Handshake.randomNonce()
let proof = Handshake.proof(key, side: "client", clientNonce: cn, hostNonce: hn)
check("proof verifies", Handshake.verify(key, side: "client", clientNonce: cn, hostNonce: hn, proof: proof))
check("proof is side-bound", !Handshake.verify(key, side: "host", clientNonce: cn, hostNonce: hn, proof: proof))
check("wrong key rejected", !Handshake.verify(PairingKey.generate(), side: "client", clientNonce: cn, hostNonce: hn, proof: proof))

let keysA = Handshake.sessionKeys(key, clientNonce: cn, hostNonce: hn)
let keysB = Handshake.sessionKeys(key, clientNonce: cn, hostNonce: hn)
let sealer = StreamSealer(key: keysA.clientToHost)
let opener = StreamSealer(key: keysB.clientToHost)
check("stream seal/open", try opener.open(try sealer.seal(Data("one".utf8))) == Data("one".utf8)
      && opener.open(try sealer.seal(Data("two".utf8))) == Data("two".utf8))
check("stream rejects replay", (try? opener.open(try StreamSealer(key: keysA.clientToHost).seal(Data("one".utf8)))) == nil)

// Frame payload encode/decode
let payload = FramePayload(codec: .hevc, parameterSets: [Data([1, 2, 3]), Data([4, 5])], sampleData: Data(repeating: 9, count: 5000))
let decoded = FramePayload.decode(payload.encode())
check("payload round trip", decoded?.codec == .hevc && decoded?.parameterSets == payload.parameterSets && decoded?.sampleData == payload.sampleData)

// Seal a frame, packetize, shuffle, reassemble, open
let big = Data((0..<250_000).map { UInt8($0 & 0xFF) })
let aad = VideoPacketHeader.aad(flags: VideoPacketHeader.keyframeFlag, epoch: 3, frameId: 42, captureUs: 123_456)
let sealed = try FrameCrypto.seal(big, key: keysA.video, frameId: 42, aad: aad)
var packets = Packetizer.packets(sealed: sealed, flags: VideoPacketHeader.keyframeFlag, epoch: 3, frameId: 42, captureUs: 123_456)
check("packets fit in a datagram", packets.allSatisfy { $0.count <= BB1.maxDatagram })
packets.shuffle()
let r = Reassembler()
var frame: Reassembler.Frame?
for p in packets { if let f = r.add(p, nowUs: 1) { frame = f } }
check("reassembled frame", frame != nil && frame!.isKeyframe && frame!.epoch == 3)
check("frame opens", try FrameCrypto.open(frame!.sealed, key: keysB.video, frameId: 42, aad: frame!.aad) == big)
check("tampered metadata rejected", (try? FrameCrypto.open(frame!.sealed, key: keysB.video, frameId: 42,
                                                          aad: VideoPacketHeader.aad(flags: 0, epoch: 3, frameId: 42, captureUs: 123_456))) == nil)

// A lost packet is asked for again; the frame completes when the resent packet arrives, even after a newer frame
let r2 = Reassembler()
let p1 = Packetizer.packets(sealed: sealed, flags: 0, epoch: 1, frameId: 1, captureUs: 0)
let p2 = Packetizer.packets(sealed: sealed, flags: 0, epoch: 1, frameId: 2, captureUs: 0)
let p4 = Packetizer.packets(sealed: sealed, flags: 0, epoch: 1, frameId: 4, captureUs: 0)
var got: [UInt32] = []
for p in p1.dropLast() { if let f = r2.add(p, nowUs: 0) { got.append(f.frameId) } }
for p in p2 { if let f = r2.add(p, nowUs: 1_000) { got.append(f.frameId) } }
let asks = r2.requests(nowUs: 10_000, quietUs: 3_000, retryUs: 8_000, maxTries: 4)
check("asks for exactly the missing packet", asks.count == 1 && asks[0].frameId == 1 && asks[0].indices == [UInt16(p1.count - 1)])
check("doesn't ask again too soon", r2.requests(nowUs: 12_000, quietUs: 3_000, retryUs: 8_000, maxTries: 4).isEmpty)
if let f = r2.add(p1.last!, nowUs: 13_000) { got.append(f.frameId); check("resent frame is marked", f.wasRetransmitted) }
check("frame completes after resend", got == [2, 1])
for p in p4 { _ = r2.add(p, nowUs: 20_000) }
let whole = r2.requests(nowUs: 30_000, quietUs: 3_000, retryUs: 8_000, maxTries: 4)
check("a frame never seen is asked for whole", whole.contains { $0.frameId == 3 && $0.indices == nil })
r2.floor = 4
check("nothing asked below the floor", r2.requests(nowUs: 60_000, quietUs: 3_000, retryUs: 8_000, maxTries: 4).isEmpty)

// UDP hello
let hello = UDPHello.make(sessionId: 77, key: keysA.video)
check("udp hello parses", UDPHello.parse(hello) == 77 && UDPHello.verify(hello, key: keysB.video))
check("udp hello wrong key", !UDPHello.verify(hello, key: Handshake.sessionKeys(PairingKey.generate(), clientNonce: cn, hostNonce: hn).video))

// Control messages survive JSON
let msgs: [ControlMessage] = [.key(KeyMsg(code: 55, down: true, isRepeat: false)), .releaseAll,
                              .scroll(ScrollMsg(dx: 1.5, dy: -3, precise: true, phase: 4, momentum: 0))]
let roundTrip = try msgs.map { try JSONDecoder().decode(ControlMessage.self, from: JSONEncoder().encode($0)) }
check("control messages round trip", roundTrip.count == 3)

print(failures == 0 ? "\nAll checks passed." : "\n\(failures) check(s) failed.")
exit(failures == 0 ? 0 : 1)
