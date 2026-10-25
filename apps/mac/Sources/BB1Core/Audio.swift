import CryptoKit
import Foundation

/// Sound: 48 kHz stereo, 16-bit PCM, 5 ms (240 frames) per UDP datagram, sealed like video.
/// Uncompressed is ~1.5 Mbps: nothing next to the video, and no codec delay.
public enum AudioFormat {
    public static let sampleRate = 48_000.0
    public static let channels = 2
    public static let framesPerPacket = 240
    public static let bytesPerPacket = framesPerPacket * channels * 2
}

/// Header (14 bytes): type, flags, sequence number, capture time. The header is authenticated.
public enum AudioPacket {
    public static let type: UInt8 = 0x03
    public static let headerSize = 14

    private static func nonce(_ seq: UInt32) throws -> ChaChaPoly.Nonce {
        var bytes = Data([0x41, 0x55, 0x44, 0x00]) // "AUD" — distinct from video frame nonces
        bytes.appendLE(UInt64(seq))
        return try ChaChaPoly.Nonce(data: bytes)
    }

    public static func make(seq: UInt32, captureUs: UInt64, pcm: Data, key: SymmetricKey) -> Data? {
        var header = Data([type, 0])
        header.appendLE(seq)
        header.appendLE(captureUs)
        guard let box = try? ChaChaPoly.seal(pcm, using: key, nonce: nonce(seq), authenticating: header) else { return nil }
        return header + box.ciphertext + box.tag
    }

    /// Returns (seq, captureUs, pcm) if the packet is genuine.
    public static func open(_ datagram: Data, key: SymmetricKey) -> (UInt32, UInt64, Data)? {
        let d = Data(datagram)
        guard d.count > headerSize + 16, d.first == type else { return nil }
        let seq = d.readLE(2, as: UInt32.self)
        let captureUs = d.readLE(6, as: UInt64.self)
        let header = d.prefix(headerSize)
        let body = d.dropFirst(headerSize)
        guard let box = try? ChaChaPoly.SealedBox(nonce: nonce(seq), ciphertext: body.dropLast(16), tag: body.suffix(16)),
              let pcm = try? ChaChaPoly.open(box, using: key, authenticating: header) else { return nil }
        return (seq, captureUs, pcm)
    }
}
