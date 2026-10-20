import CryptoKit
import Foundation

/// The shared secret between host and client. 120 random bits, shown as 6 groups of 4 characters.
public struct PairingKey: Equatable {
    public let bytes: Data

    private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ") // Crockford base32

    public static func generate() -> PairingKey {
        var bytes = Data(count: 15)
        _ = bytes.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 15, $0.baseAddress!) }
        return PairingKey(bytes: bytes)
    }

    public init(bytes: Data) { self.bytes = bytes }

    /// Accepts any case, with or without dashes/spaces. I/L read as 1, O as 0.
    public init?(string: String) {
        var values: [UInt8] = []
        for ch in string.uppercased() {
            if ch == "-" || ch == " " { continue }
            let c: Character = ch == "I" || ch == "L" ? "1" : ch == "O" ? "0" : ch
            guard let index = PairingKey.alphabet.firstIndex(of: c) else { return nil }
            values.append(UInt8(index))
        }
        guard values.count == 24 else { return nil }
        var out = Data()
        var buffer = 0, bits = 0
        for v in values {
            buffer = (buffer << 5) | Int(v)
            bits += 5
            if bits >= 8 {
                bits -= 8
                out.append(UInt8((buffer >> bits) & 0xFF))
            }
        }
        self.bytes = out
    }

    public var display: String {
        var chars: [Character] = []
        var buffer = 0, bits = 0
        for b in bytes {
            buffer = (buffer << 8) | Int(b)
            bits += 8
            while bits >= 5 {
                bits -= 5
                chars.append(PairingKey.alphabet[(buffer >> bits) & 0x1F])
            }
        }
        return stride(from: 0, to: chars.count, by: 4)
            .map { String(chars[$0..<min($0 + 4, chars.count)]) }
            .joined(separator: "-")
    }

    var symmetricKey: SymmetricKey { SymmetricKey(data: bytes) }

    /// Host side: the key lives in ~/Library/Application Support/bb1/host-key (mode 0600).
    public static func loadOrCreateHostKey() -> PairingKey {
        let url = BB1.supportDirectory().appendingPathComponent("host-key")
        if let text = try? String(contentsOf: url, encoding: .utf8),
           let key = PairingKey(string: text.trimmingCharacters(in: .whitespacesAndNewlines)) {
            return key
        }
        let key = generate()
        try? key.display.write(to: url, atomically: true, encoding: .utf8)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        return key
    }
}

public struct SessionKeys {
    public let clientToHost: SymmetricKey
    public let hostToClient: SymmetricKey
    public let video: SymmetricKey
}

public enum Handshake {
    public static func randomNonce() -> Data {
        var d = Data(count: 32)
        _ = d.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 32, $0.baseAddress!) }
        return d
    }

    /// HMAC proving knowledge of the pairing key, bound to both nonces and the side ("host"/"client").
    public static func proof(_ key: PairingKey, side: String, clientNonce: Data, hostNonce: Data) -> Data {
        var msg = Data("bb1 proof \(side)".utf8)
        msg.append(clientNonce)
        msg.append(hostNonce)
        return Data(HMAC<SHA256>.authenticationCode(for: msg, using: key.symmetricKey))
    }

    public static func verify(_ key: PairingKey, side: String, clientNonce: Data, hostNonce: Data, proof: Data) -> Bool {
        var msg = Data("bb1 proof \(side)".utf8)
        msg.append(clientNonce)
        msg.append(hostNonce)
        return HMAC<SHA256>.isValidAuthenticationCode(proof, authenticating: msg, using: key.symmetricKey)
    }

    public static func sessionKeys(_ key: PairingKey, clientNonce: Data, hostNonce: Data) -> SessionKeys {
        let salt = clientNonce + hostNonce
        func derive(_ label: String) -> SymmetricKey {
            HKDF<SHA256>.deriveKey(inputKeyMaterial: key.symmetricKey, salt: salt,
                                   info: Data("bb1 \(label)".utf8), outputByteCount: 32)
        }
        return SessionKeys(clientToHost: derive("c2h"), hostToClient: derive("h2c"), video: derive("video"))
    }
}

// MARK: - Version 4 handshake

/// Each connection does a fresh Curve25519 key exchange (forward secrecy), bound to whatever proves
/// the client is allowed in: a remembered pairing secret, a 6-digit code, the old pairing key,
/// or (for Tailscale) nothing extra, because Tailscale has already proven who the device belongs to.
public enum HandshakeV4 {
    public static func transcript(clientNonce: Data, hostNonce: Data, clientPub: Data, hostPub: Data,
                                  deviceId: String, hostId: String, mode: String) -> Data {
        var h = SHA256()
        for part in [Data("bb1 v4".utf8), clientNonce, hostNonce, clientPub, hostPub,
                     Data(deviceId.utf8), Data(hostId.utf8), Data(mode.utf8)] {
            var len = UInt32(part.count).littleEndian
            h.update(data: Data(bytes: &len, count: 4))
            h.update(data: part)
        }
        return Data(h.finalize())
    }

    public static func proof(secret: Data, side: String, transcript: Data) -> Data {
        Data(HMAC<SHA256>.authenticationCode(for: Data("bb1 v4 \(side)".utf8) + transcript, using: SymmetricKey(data: secret)))
    }

    public static func verify(secret: Data, side: String, transcript: Data, proof: Data) -> Bool {
        HMAC<SHA256>.isValidAuthenticationCode(proof, authenticating: Data("bb1 v4 \(side)".utf8) + transcript,
                                               using: SymmetricKey(data: secret))
    }

    public static func sessionKeys(shared: SharedSecret, secret: Data, transcript: Data) -> SessionKeys {
        let ikm = shared.withUnsafeBytes { Data($0) } + secret
        func derive(_ label: String) -> SymmetricKey {
            HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: ikm), salt: transcript,
                                   info: Data("bb1 v4 \(label)".utf8), outputByteCount: 32)
        }
        return SessionKeys(clientToHost: derive("c2h"), hostToClient: derive("h2c"), video: derive("video"))
    }

    /// A 6-digit code for pairing, shown on the host.
    public static func pairingCode() -> String {
        String(format: "%06d", Int.random(in: 0..<1_000_000))
    }
}

public enum CryptoFailure: Error { case badLength, authentication }

/// Seals an ordered stream of messages. The nonce is a counter, so both sides must process every message in order.
public final class StreamSealer {
    private let key: SymmetricKey
    private var counter: UInt64 = 0

    public init(key: SymmetricKey) { self.key = key }

    private func nextNonce() throws -> ChaChaPoly.Nonce {
        var bytes = Data(count: 4)
        bytes.appendLE(counter)
        counter += 1
        return try ChaChaPoly.Nonce(data: bytes)
    }

    public func seal(_ plaintext: Data) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: nextNonce())
        return box.ciphertext + box.tag
    }

    public func open(_ sealed: Data) throws -> Data {
        guard sealed.count >= 16 else { throw CryptoFailure.badLength }
        let box = try ChaChaPoly.SealedBox(nonce: nextNonce(),
                                           ciphertext: sealed.dropLast(16), tag: sealed.suffix(16))
        return try ChaChaPoly.open(box, using: key)
    }
}

/// One AEAD per video frame. Frame IDs never repeat within a session, so they make unique nonces.
public enum FrameCrypto {
    private static func nonce(_ frameId: UInt32) throws -> ChaChaPoly.Nonce {
        var bytes = Data([0x46, 0x52, 0x4D, 0x00])
        bytes.appendLE(UInt64(frameId))
        return try ChaChaPoly.Nonce(data: bytes)
    }

    public static func seal(_ plaintext: Data, key: SymmetricKey, frameId: UInt32, aad: Data) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: nonce(frameId), authenticating: aad)
        return box.ciphertext + box.tag
    }

    public static func open(_ sealed: Data, key: SymmetricKey, frameId: UInt32, aad: Data) throws -> Data {
        guard sealed.count >= 16 else { throw CryptoFailure.badLength }
        let box = try ChaChaPoly.SealedBox(nonce: nonce(frameId),
                                           ciphertext: sealed.dropLast(16), tag: sealed.suffix(16))
        return try ChaChaPoly.open(box, using: key, authenticating: aad)
    }
}

/// The client's UDP "here I am" packet: type, session ID, and a tag proving it belongs to the session.
public enum UDPHello {
    public static let type: UInt8 = 0x02

    public static func make(sessionId: UInt32, key: SymmetricKey) -> Data {
        var d = Data([type])
        d.appendLE(sessionId)
        d.append(Data(HMAC<SHA256>.authenticationCode(for: d, using: key)).prefix(16))
        return d
    }

    public static func parse(_ data: Data) -> UInt32? {
        guard data.count == 21, data.first == type else { return nil }
        return data.readLE(1, as: UInt32.self)
    }

    public static func verify(_ data: Data, key: SymmetricKey) -> Bool {
        guard data.count == 21 else { return false }
        let body = data.prefix(5)
        let expected = Data(HMAC<SHA256>.authenticationCode(for: body, using: key)).prefix(16)
        return expected == data.suffix(16)
    }
}
