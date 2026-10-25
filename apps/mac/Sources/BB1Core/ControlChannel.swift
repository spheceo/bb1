import CryptoKit
import Foundation
import Network

/// The reliable TCP channel: handshake, then sealed, length-prefixed JSON messages.
/// All callbacks run on `queue`. `send` can be called from any thread.
public final class ControlChannel {
    /// How a client proves it's allowed in.
    public enum ClientAuth {
        /// The pairing key from version 1–3 (still accepted, then upgraded to a pairing).
        case legacyKey(PairingKey)
        /// A remembered pairing with this host.
        case device(secret: Data)
        /// The host checks with Tailscale that this device is on the same account.
        case tailscale
        /// First-time pairing: the host shows a 6-digit code; `askCode` gets it from the user.
        case code

        public var mode: String {
            switch self {
            case .legacyKey: return "key"
            case .device: return "device"
            case .tailscale: return "tailscale"
            case .code: return "code"
            }
        }
    }

    /// What the host is told about a connecting client before deciding.
    public struct AuthRequest {
        public let mode: String
        public let deviceId: String
        public let deviceName: String
        public let remoteAddress: String?
    }

    public enum AuthDecision {
        case secret(Data)        // prove knowledge of this
        case tailscaleVerified   // Tailscale vouched for the device
        case code(String)        // a pairing code is being shown on the host
        case reject(String)
    }

    public enum Role {
        case host(hostName: String, hostId: String, legacyKey: PairingKey,
                  authorize: (AuthRequest, @escaping (AuthDecision) -> Void) -> Void)
        case client(clientName: String, deviceId: String, auth: ClientAuth,
                    askCode: (@escaping (String?) -> Void) -> Void)
    }

    public enum CloseReason {
        case rejected(String)   // not allowed in; retrying the same way won't help
        case closed(String)
    }

    public let connection: NWConnection
    public let queue: DispatchQueue
    private let role: Role
    private var sendSealer: StreamSealer?
    private var recvSealer: StreamSealer?
    private var clientNonce = Data()
    private var hostNonce = Data()
    private var closed = false
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    // Version 4 handshake state
    private var ephemeral: Curve25519.KeyAgreement.PrivateKey?
    private var shared: SharedSecret?
    private var transcript = Data()
    private var proofSecret = Data()

    public private(set) var keys: SessionKeys?
    public private(set) var peerName = ""
    /// Protocol version the other side announced. New message types are only sent to peers that know them.
    public private(set) var peerVersion = 1
    /// The other side's permanent ID (version 4+).
    public private(set) var peerId = ""
    /// How this session was authenticated: "device", "tailscale", "code" or "key".
    public private(set) var authMode = "key"
    /// The other side's app build, if it announced one (0 = an older bb1 that can't update remotely).
    public private(set) var peerBuild = 0
    /// Host: the codecs the client can decode (nil = any; every Mac client).
    public private(set) var peerCodecs: [String]?
    /// Client: set before `start()`. Host: what the client said it connected for ("update", or nil to stream).
    public var purpose: String?
    /// Client side: waiting for the user to type a pairing code (don't time out).
    public private(set) var awaitingCode = false
    public var onAuthenticated: ((SessionKeys) -> Void)?
    public var onMessage: ((ControlMessage) -> Void)?
    public var onClose: ((CloseReason) -> Void)?

    public init(connection: NWConnection, role: Role, queue: DispatchQueue) {
        self.connection = connection
        self.role = role
        self.queue = queue
    }

    public static func tcpParameters() -> NWParameters {
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.connectionTimeout = 5
        let params = NWParameters(tls: nil, tcp: tcp)
        params.serviceClass = .responsiveData
        return params
    }

    public func start() {
        connection.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                if case .client(let name, let deviceId, let auth, _) = self.role {
                    self.clientNonce = Handshake.randomNonce()
                    let eph = Curve25519.KeyAgreement.PrivateKey()
                    self.ephemeral = eph
                    self.authMode = auth.mode
                    self.writePlain(.clientHello(ClientHello(version: BB1.protocolVersion, nonce: self.clientNonce, name: name,
                                                             deviceId: deviceId, mode: auth.mode,
                                                             ecdhPub: eph.publicKey.rawRepresentation,
                                                             appBuild: BB1.build, purpose: self.purpose)))
                }
            case .waiting(let error):
                self.close(.closed("can't reach host (\(error.localizedDescription))"))
            case .failed(let error):
                self.close(.closed("connection failed (\(error.localizedDescription))"))
            case .cancelled:
                self.close(.closed("connection closed"))
            default:
                break
            }
        }
        connection.start(queue: queue)
        receiveNext()
    }

    public func send(_ message: ControlMessage) {
        queue.async { [weak self] in
            guard let self, !self.closed, let sealer = self.sendSealer else { return }
            do {
                self.write(try sealer.seal(try self.encoder.encode(message)))
            } catch {
                self.close(.closed("encode failed: \(error)"))
            }
        }
    }

    public func close(_ reason: CloseReason) {
        guard !closed else { return }
        closed = true
        connection.cancel()
        onClose?(reason)
        onClose = nil
        onMessage = nil
        onAuthenticated = nil
    }

    public var remoteHost: NWEndpoint.Host? {
        if case .hostPort(let host, _)? = connection.currentPath?.remoteEndpoint { return host }
        if case .hostPort(let host, _) = connection.endpoint { return host }
        return nil
    }

    // MARK: Framing

    private func writePlain(_ message: ControlMessage) {
        guard let body = try? encoder.encode(message) else { return }
        write(body)
    }

    private func write(_ body: Data) {
        var frame = Data(capacity: body.count + 4)
        frame.appendLE(UInt32(body.count))
        frame.append(body)
        connection.send(content: frame, completion: .contentProcessed { [weak self] error in
            if let error { self?.close(.closed("send failed (\(error.localizedDescription))")) }
        })
    }

    private func receiveNext() {
        connection.receive(minimumIncompleteLength: 4, maximumLength: 4) { [weak self] data, _, _, error in
            guard let self, !self.closed else { return }
            guard let data, data.count == 4 else {
                self.close(.closed(error.map { "receive failed (\($0.localizedDescription))" } ?? "host closed the connection"))
                return
            }
            let length = Int(data.readLE(0, as: UInt32.self))
            guard length > 0, length < 12_000_000 else { self.close(.closed("bad frame length")); return }
            self.connection.receive(minimumIncompleteLength: length, maximumLength: length) { [weak self] body, _, _, error in
                guard let self, !self.closed else { return }
                guard let body, body.count == length else {
                    self.close(.closed(error.map { "receive failed (\($0.localizedDescription))" } ?? "connection closed"))
                    return
                }
                let plain: Data
                do {
                    plain = try self.recvSealer.map { try $0.open(body) } ?? body
                } catch {
                    self.close(.closed("message failed authentication"))
                    return
                }
                if let message = try? self.decoder.decode(ControlMessage.self, from: plain) {
                    self.handle(message)
                } else if self.keys == nil {
                    self.close(.closed("bad handshake message"))
                    return
                } else {
                    // A newer peer may send message types we don't know yet; skip them.
                    Log.info("control: ignoring unknown message")
                }
                self.receiveNext()
            }
        }
    }

    // MARK: Handshake

    /// The other end's IP address ("100.82.185.25").
    public var remoteAddress: String? {
        if case .hostPort(let host, _)? = connection.currentPath?.remoteEndpoint { return "\(host)" }
        if case .hostPort(let host, _) = connection.endpoint { return "\(host)" }
        return nil
    }

    private func handle(_ message: ControlMessage) {
        if keys != nil {
            onMessage?(message)
            return
        }
        switch (role, message) {
        case (.host(let name, let hostId, let legacyKey, let authorize), .clientHello(let hello)):
            guard (BB1.minimumProtocolVersion...BB1.protocolVersion).contains(hello.version), hello.nonce.count == 32 else {
                rejectAndClose("version mismatch (client \(hello.version), host \(BB1.protocolVersion))")
                return
            }
            clientNonce = hello.nonce
            hostNonce = Handshake.randomNonce()
            peerName = hello.name
            peerVersion = hello.version
            peerBuild = hello.appBuild ?? 0
            peerCodecs = hello.codecs
            purpose = hello.purpose
            guard hello.version >= 4, let clientPub = hello.ecdhPub else {
                // Version 1–3 client: the old pairing-key handshake.
                authMode = "key"
                let proof = Handshake.proof(legacyKey, side: "host", clientNonce: clientNonce, hostNonce: hostNonce)
                writePlain(.hostHello(HostHello(version: BB1.protocolVersion, nonce: hostNonce, proof: proof, name: name)))
                return
            }
            peerId = hello.deviceId ?? ""
            authMode = hello.mode ?? "key"
            let request = AuthRequest(mode: authMode, deviceId: peerId, deviceName: hello.name, remoteAddress: remoteAddress)
            let proceed: (AuthDecision) -> Void = { [weak self] decision in
                guard let self else { return }
                self.queue.async { self.hostContinue(decision, name: name, hostId: hostId, clientPub: clientPub) }
            }
            if authMode == "key" { proceed(.secret(legacyKey.bytes)) } else { authorize(request, proceed) }

        case (.host(_, _, let legacyKey, _), .clientProof(let proof)):
            guard !hostNonce.isEmpty else { rejectAndClose("unexpected proof"); return }
            if let shared {
                // Version 4
                let ok = proofSecret.isEmpty
                    ? (authMode == "tailscale" && proof.isEmpty)
                    : HandshakeV4.verify(secret: proofSecret, side: "client", transcript: transcript, proof: proof)
                guard ok else {
                    rejectAndClose(authMode == "code" ? "wrong pairing code" : "this device isn't paired with this Mac")
                    return
                }
                install(HandshakeV4.sessionKeys(shared: shared, secret: proofSecret, transcript: transcript), host: true)
            } else {
                guard Handshake.verify(legacyKey, side: "client", clientNonce: clientNonce, hostNonce: hostNonce, proof: proof) else {
                    rejectAndClose("wrong pairing key")
                    return
                }
                install(Handshake.sessionKeys(legacyKey, clientNonce: clientNonce, hostNonce: hostNonce), host: true)
            }

        case (.client(_, let deviceId, let auth, let askCode), .hostHello(let hello)):
            hostNonce = hello.nonce
            peerName = hello.name
            peerVersion = hello.version
            peerBuild = hello.appBuild ?? 0
            peerId = hello.hostId ?? ""
            guard hello.version >= 4, let hostPub = hello.ecdhPub, let eph = ephemeral,
                  let hostKey = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: hostPub),
                  let shared = try? eph.sharedSecretFromKeyAgreement(with: hostKey) else {
                // An older host: only the old pairing key works.
                guard case .legacyKey(let key) = auth else {
                    close(.rejected("the host runs an older bb1; update it"))
                    return
                }
                guard Handshake.verify(key, side: "host", clientNonce: clientNonce, hostNonce: hostNonce, proof: hello.proof) else {
                    close(.rejected("the host didn't accept this pairing key"))
                    return
                }
                writePlain(.clientProof(Handshake.proof(key, side: "client", clientNonce: clientNonce, hostNonce: hostNonce)))
                install(Handshake.sessionKeys(key, clientNonce: clientNonce, hostNonce: hostNonce), host: false)
                return
            }
            self.shared = shared
            transcript = HandshakeV4.transcript(clientNonce: clientNonce, hostNonce: hostNonce,
                                                clientPub: eph.publicKey.rawRepresentation, hostPub: hostPub,
                                                deviceId: deviceId, hostId: peerId, mode: auth.mode)
            let finish: (Data) -> Void = { [weak self] secret in
                guard let self, !self.closed else { return }
                if !secret.isEmpty {
                    guard HandshakeV4.verify(secret: secret, side: "host", transcript: self.transcript, proof: hello.proof) else {
                        self.close(.rejected(auth.mode == "code" ? "That code doesn't match. Try again for a new code."
                                                                 : "the host didn't recognise this Mac"))
                        return
                    }
                }
                self.proofSecret = secret
                self.writePlain(.clientProof(secret.isEmpty ? Data()
                                             : HandshakeV4.proof(secret: secret, side: "client", transcript: self.transcript)))
                self.install(HandshakeV4.sessionKeys(shared: shared, secret: secret, transcript: self.transcript), host: false)
            }
            switch auth {
            case .legacyKey(let key): finish(key.bytes)
            case .device(let secret): finish(secret)
            case .tailscale: finish(Data())
            case .code:
                awaitingCode = true
                askCode { [weak self] code in
                    guard let self else { return }
                    self.queue.async {
                        self.awaitingCode = false
                        guard let code, !code.isEmpty else { self.close(.rejected("pairing cancelled")); return }
                        finish(Data(code.filter(\.isNumber).utf8))
                    }
                }
            }

        case (_, .authFailed(let reason)):
            close(.rejected(reason))

        default:
            close(.closed("unexpected message during handshake"))
        }
    }

    /// Host, version 4: send our half of the key exchange and our proof, once the app decided.
    private func hostContinue(_ decision: AuthDecision, name: String, hostId: String, clientPub: Data) {
        guard !closed else { return }
        let secret: Data
        switch decision {
        case .reject(let reason): rejectAndClose(reason); return
        case .secret(let s): secret = s
        case .tailscaleVerified: secret = Data()
        case .code(let code): secret = Data(code.utf8)
        }
        let eph = Curve25519.KeyAgreement.PrivateKey()
        guard let clientKey = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: clientPub),
              let shared = try? eph.sharedSecretFromKeyAgreement(with: clientKey) else {
            rejectAndClose("bad key exchange")
            return
        }
        self.shared = shared
        proofSecret = secret
        transcript = HandshakeV4.transcript(clientNonce: clientNonce, hostNonce: hostNonce, clientPub: clientPub,
                                            hostPub: eph.publicKey.rawRepresentation, deviceId: peerId,
                                            hostId: hostId, mode: authMode)
        let proof = secret.isEmpty ? Data() : HandshakeV4.proof(secret: secret, side: "host", transcript: transcript)
        writePlain(.hostHello(HostHello(version: BB1.protocolVersion, nonce: hostNonce, proof: proof, name: name,
                                        hostId: hostId, ecdhPub: eph.publicKey.rawRepresentation, appBuild: BB1.build)))
    }

    private func install(_ k: SessionKeys, host: Bool) {
        sendSealer = StreamSealer(key: host ? k.hostToClient : k.clientToHost)
        recvSealer = StreamSealer(key: host ? k.clientToHost : k.hostToClient)
        keys = k
        onAuthenticated?(k)
    }

    private func rejectAndClose(_ reason: String) {
        writePlain(.authFailed(reason))
        queue.asyncAfter(deadline: .now() + 0.3) { [weak self] in self?.close(.rejected(reason)) }
    }
}
