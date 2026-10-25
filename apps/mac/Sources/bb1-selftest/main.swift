import BB1Core
import CryptoKit
import Foundation

// Quick checks for the protocol pieces that don't need a screen or a network.
// Run with: swift run bb1-selftest
// `swift run bb1-selftest --vectors <file>` writes fixed test vectors that other clients
// (apps/windows) check themselves against: protocol/vectors.json.

if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--vectors" {
    try writeVectors(to: URL(fileURLWithPath: CommandLine.arguments[2]))
    exit(0)
}

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

// MARK: - Test vectors for other clients

/// Hello messages have no public initialisers; build them the way they arrive.
func fromJSON<T: Decodable>(_ object: [String: Any]) throws -> T {
    try JSONDecoder().decode(T.self, from: JSONSerialization.data(withJSONObject: object))
}

func writeVectors(to url: URL) throws {
    func bytes(_ n: Int, _ seed: UInt8) -> Data { Data((0..<n).map { UInt8(truncatingIfNeeded: Int(seed) &+ $0 &* 7) }) }
    func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
    let clientPriv = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: bytes(32, 1))
    let hostPriv = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: bytes(32, 2))
    let clientNonce = bytes(32, 3), hostNonce = bytes(32, 4), secret = bytes(32, 5)
    let clientPub = clientPriv.publicKey.rawRepresentation, hostPub = hostPriv.publicKey.rawRepresentation
    let shared = try clientPriv.sharedSecretFromKeyAgreement(with: hostPriv.publicKey)
    let transcript = HandshakeV4.transcript(clientNonce: clientNonce, hostNonce: hostNonce, clientPub: clientPub,
                                            hostPub: hostPub, deviceId: "DEVICE-1", hostId: "HOST-1", mode: "device")
    let keys = HandshakeV4.sessionKeys(shared: shared, secret: secret, transcript: transcript)
    let tsKeys = HandshakeV4.sessionKeys(shared: shared, secret: Data(), transcript: transcript)
    func raw(_ k: SymmetricKey) -> Data { k.withUnsafeBytes { Data($0) } }
    let sealer = StreamSealer(key: keys.clientToHost)
    let sealed0 = try sealer.seal(Data("first".utf8)), sealed1 = try sealer.seal(Data("second".utf8))
    let aad = VideoPacketHeader.aad(flags: 3, epoch: 7, frameId: 1234, captureUs: 987_654_321)
    let frame = try FrameCrypto.seal(bytes(3000, 9), key: keys.video, frameId: 1234, aad: aad)
    let packets = Packetizer.packets(sealed: frame, flags: 3, epoch: 7, frameId: 1234, captureUs: 987_654_321)
    let audio = AudioPacket.make(seq: 99, captureUs: 555, pcm: bytes(AudioFormat.bytesPerPacket, 11), key: keys.video)!
    let payload = FramePayload(codec: .h264, parameterSets: [Data([0x67, 1, 2]), Data([0x68, 3])],
                               sampleData: Data([0, 0, 0, 2, 0x65, 0x88])).encode()

    let messages: [ControlMessage] = [
        .clientHello(try fromJSON(["version": 4, "nonce": clientNonce.base64EncodedString(), "name": "PC",
                                   "deviceId": "DEVICE-1", "mode": "tailscale", "ecdhPub": clientPub.base64EncodedString(),
                                   "appBuild": 3, "codecs": ["h264"]])),
        .hostHello(try fromJSON(["version": 4, "nonce": hostNonce.base64EncodedString(), "proof": Data([1, 2]).base64EncodedString(),
                                 "name": "kenobi", "hostId": "HOST-1", "ecdhPub": hostPub.base64EncodedString(), "appBuild": 80])),
        .clientProof(Data([9, 8, 7])),
        .authFailed("nope"),
        .welcome(hostName: "kenobi"),
        .streamConfig(StreamConfig(width: 1920, height: 1080, codec: .h264, fps: 60, bitrateMbps: 30,
                                   lowLatencyEncoder: true, udpPort: 47801, sessionId: 5, epoch: 2)),
        .requestKeyframe(reason: "lost"),
        .ping(PingMsg(seq: 1, clientUs: 2, received: 3, decoded: 4, displayed: 5)),
        .pong(PongMsg(seq: 1, clientUs: 2, hostUs: 3, captured: 4, encoded: 5, sent: 6, encodeAvgUs: 7, hostRecoveries: 8)),
        .key(KeyMsg(code: 55, down: true, isRepeat: false)),
        .mouseMove(MouseMoveMsg(x: 0.25, y: 0.75)),
        .mouseButton(MouseButtonMsg(button: 1, down: true, clicks: 2, x: 0.5, y: 0.125)),
        .scroll(ScrollMsg(dx: 1.5, dy: -2.5, precise: false, phase: 0, momentum: 0)),
        .releaseAll,
        .setCursorInVideo(false),
        .notice("hello"),
        .bye("done"),
        .nack(NackMsg(frameId: 9, indices: [1, 2])),
        .nack(NackMsg(frameId: 10, indices: nil)),
        .clientLog("line"),
        .clipboard(ClipboardMsg(text: "copied", png: nil)),
        .clipboard(ClipboardMsg(text: nil, png: Data([0x89, 0x50]))),
        .setAudio(true),
        .paired(PairedMsg(hostId: "HOST-1", hostName: "kenobi", secret: secret)),
        .cursor(CursorMsg(png: Data([1]), width: 16.5, height: 24.5, hotX: 4.5, hotY: 2.5)),
        .capsLock(true),
    ]
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    let json = try messages.map { String(decoding: try encoder.encode($0), as: UTF8.self) }

    let vectors: [String: Any] = [
        "comment": "Written by `swift run bb1-selftest --vectors protocol/vectors.json`. Byte values are hex.",
        "clientPriv": hex(bytes(32, 1)), "hostPriv": hex(bytes(32, 2)),
        "clientPub": hex(clientPub), "hostPub": hex(hostPub),
        "shared": hex(shared.withUnsafeBytes { Data($0) }),
        "clientNonce": hex(clientNonce), "hostNonce": hex(hostNonce), "secret": hex(secret),
        "deviceId": "DEVICE-1", "hostId": "HOST-1", "mode": "device",
        "transcript": hex(transcript),
        "clientProof": hex(HandshakeV4.proof(secret: secret, side: "client", transcript: transcript)),
        "hostProof": hex(HandshakeV4.proof(secret: secret, side: "host", transcript: transcript)),
        "c2h": hex(raw(keys.clientToHost)), "h2c": hex(raw(keys.hostToClient)), "video": hex(raw(keys.video)),
        "tailscaleVideo": hex(raw(tsKeys.video)),
        "sealed0": hex(sealed0), "sealed1": hex(sealed1),
        "framePlain": hex(bytes(3000, 9)), "frameAad": hex(aad), "frameSealed": hex(frame),
        "framePackets": packets.map(hex),
        "audioPcm": hex(bytes(AudioFormat.bytesPerPacket, 11)), "audioPacket": hex(audio),
        "udpHello": hex(UDPHello.make(sessionId: 5, key: keys.video)),
        "payload": hex(payload),
        "messages": json,
    ]
    let data = try JSONSerialization.data(withJSONObject: vectors, options: [.prettyPrinted, .sortedKeys])
    try data.write(to: url)
    print("wrote \(url.path)")
}
