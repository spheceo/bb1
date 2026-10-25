//! The version 4 handshake and the AEADs, matching BB1Core/Crypto.swift and Audio.swift.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct SessionKeys {
    pub client_to_host: [u8; 32],
    pub host_to_client: [u8; 32],
    pub video: [u8; 32],
}

pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

/// A fresh Curve25519 key pair for one connection.
pub struct Ephemeral {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl Ephemeral {
    pub fn new() -> Self {
        Self::from_bytes(crate::core::random_bytes::<32>())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret).to_bytes();
        Ephemeral { secret, public }
    }

    pub fn shared(&self, peer: &[u8]) -> Option<[u8; 32]> {
        let peer: [u8; 32] = peer.try_into().ok()?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(peer));
        // CryptoKit refuses an all-zero result (a bad peer key); so do we.
        if shared.as_bytes().iter().all(|&b| b == 0) { None } else { Some(*shared.as_bytes()) }
    }
}

pub fn transcript(client_nonce: &[u8], host_nonce: &[u8], client_pub: &[u8], host_pub: &[u8],
                  device_id: &str, host_id: &str, mode: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    for part in [b"bb1 v4".as_slice(), client_nonce, host_nonce, client_pub, host_pub,
                 device_id.as_bytes(), host_id.as_bytes(), mode.as_bytes()] {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part);
    }
    h.finalize().into()
}

pub fn proof(secret: &[u8], side: &str, transcript: &[u8]) -> [u8; 32] {
    hmac(secret, &[format!("bb1 v4 {side}").as_bytes(), transcript])
}

/// Constant-time comparison of a proof.
pub fn verify(secret: &[u8], side: &str, transcript: &[u8], proof: &[u8]) -> bool {
    let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(format!("bb1 v4 {side}").as_bytes());
    mac.update(transcript);
    mac.verify_slice(proof).is_ok()
}

pub fn session_keys(shared: &[u8; 32], secret: &[u8], transcript: &[u8]) -> SessionKeys {
    let mut ikm = shared.to_vec();
    ikm.extend_from_slice(secret);
    let hk = Hkdf::<Sha256>::new(Some(transcript), &ikm);
    let derive = |label: &str| {
        let mut out = [0u8; 32];
        hk.expand(format!("bb1 v4 {label}").as_bytes(), &mut out).expect("32 bytes is a valid HKDF length");
        out
    };
    SessionKeys { client_to_host: derive("c2h"), host_to_client: derive("h2c"), video: derive("video") }
}

fn cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(&Key::from(*key))
}

fn nonce(prefix: [u8; 4], counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&prefix);
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

/// An ordered stream of sealed messages; the nonce is a counter, so every message must be processed in order.
pub struct StreamSealer {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl StreamSealer {
    pub fn new(key: &[u8; 32]) -> Self {
        StreamSealer { cipher: cipher(key), counter: 0 }
    }

    fn next(&mut self) -> [u8; 12] {
        let n = nonce([0; 4], self.counter);
        self.counter += 1;
        n
    }

    /// ciphertext + 16-byte tag
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let n = self.next();
        self.cipher.encrypt(&Nonce::from(n), plain).expect("sealing can't fail")
    }

    pub fn open(&mut self, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 16 {
            return None;
        }
        let n = self.next();
        self.cipher.decrypt(&Nonce::from(n), sealed).ok()
    }
}

pub struct FrameCrypto;

impl FrameCrypto {
    pub fn seal(plain: &[u8], key: &[u8; 32], frame_id: u32, aad: &[u8]) -> Vec<u8> {
        let n = nonce(*b"FRM\0", frame_id as u64);
        cipher(key).encrypt(&Nonce::from(n), Payload { msg: plain, aad }).expect("sealing can't fail")
    }

    pub fn open(sealed: &[u8], key: &[u8; 32], frame_id: u32, aad: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 16 {
            return None;
        }
        let n = nonce(*b"FRM\0", frame_id as u64);
        cipher(key).decrypt(&Nonce::from(n), Payload { msg: sealed, aad }).ok()
    }
}

/// Sound: 48 kHz stereo 16-bit PCM, 240 frames (5 ms) per datagram.
pub const AUDIO_TYPE: u8 = 0x03;
pub const AUDIO_HEADER: usize = 14;
pub const AUDIO_FRAMES_PER_PACKET: usize = 240;

/// Returns (seq, captureUs, pcm) if the packet is genuine.
pub fn open_audio(datagram: &[u8], key: &[u8; 32]) -> Option<(u32, u64, Vec<u8>)> {
    if datagram.len() <= AUDIO_HEADER + 16 || datagram[0] != AUDIO_TYPE {
        return None;
    }
    let seq = u32::from_le_bytes(datagram[2..6].try_into().ok()?);
    let capture_us = u64::from_le_bytes(datagram[6..14].try_into().ok()?);
    let n = nonce(*b"AUD\0", seq as u64);
    let pcm = cipher(key)
        .decrypt(&Nonce::from(n), Payload { msg: &datagram[AUDIO_HEADER..], aad: &datagram[..AUDIO_HEADER] })
        .ok()?;
    Some((seq, capture_us, pcm))
}

/// The client's UDP "here I am": type 0x02, session ID, and the first 16 bytes of an HMAC.
pub fn udp_hello(session_id: u32, video_key: &[u8; 32]) -> Vec<u8> {
    let mut d = vec![0x02u8];
    d.extend_from_slice(&session_id.to_le_bytes());
    let tag = hmac(video_key, &[&d]);
    d.extend_from_slice(&tag[..16]);
    d
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    pub fn vectors() -> serde_json::Value {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../protocol/vectors.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn h(v: &serde_json::Value, k: &str) -> Vec<u8> {
        hex(v[k].as_str().unwrap())
    }

    fn arr(v: Vec<u8>) -> [u8; 32] {
        v.try_into().unwrap()
    }

    #[test]
    fn handshake_matches_swift() {
        let v = vectors();
        let client = Ephemeral::from_bytes(arr(h(&v, "clientPriv")));
        let host = Ephemeral::from_bytes(arr(h(&v, "hostPriv")));
        assert_eq!(client.public.to_vec(), h(&v, "clientPub"));
        assert_eq!(host.public.to_vec(), h(&v, "hostPub"));
        let shared = client.shared(&host.public).unwrap();
        assert_eq!(shared.to_vec(), h(&v, "shared"));
        let t = transcript(&h(&v, "clientNonce"), &h(&v, "hostNonce"), &client.public, &host.public,
                           v["deviceId"].as_str().unwrap(), v["hostId"].as_str().unwrap(), v["mode"].as_str().unwrap());
        assert_eq!(t.to_vec(), h(&v, "transcript"));
        let secret = h(&v, "secret");
        assert_eq!(proof(&secret, "client", &t).to_vec(), h(&v, "clientProof"));
        assert!(verify(&secret, "host", &t, &h(&v, "hostProof")));
        assert!(!verify(&secret, "client", &t, &h(&v, "hostProof")));
        let keys = session_keys(&shared, &secret, &t);
        assert_eq!(keys.client_to_host.to_vec(), h(&v, "c2h"));
        assert_eq!(keys.host_to_client.to_vec(), h(&v, "h2c"));
        assert_eq!(keys.video.to_vec(), h(&v, "video"));
        assert_eq!(session_keys(&shared, &[], &t).video.to_vec(), h(&v, "tailscaleVideo"));

        let mut sealer = StreamSealer::new(&keys.client_to_host);
        assert_eq!(sealer.seal(b"first"), h(&v, "sealed0"));
        assert_eq!(sealer.seal(b"second"), h(&v, "sealed1"));
        let mut opener = StreamSealer::new(&keys.client_to_host);
        assert_eq!(opener.open(&h(&v, "sealed0")).unwrap(), b"first");
        assert_eq!(opener.open(&h(&v, "sealed1")).unwrap(), b"second");

        let sealed = FrameCrypto::seal(&h(&v, "framePlain"), &keys.video, 1234, &h(&v, "frameAad"));
        assert_eq!(sealed, h(&v, "frameSealed"));
        assert_eq!(FrameCrypto::open(&sealed, &keys.video, 1234, &h(&v, "frameAad")).unwrap(), h(&v, "framePlain"));
        assert!(FrameCrypto::open(&sealed, &keys.video, 1235, &h(&v, "frameAad")).is_none());

        let (seq, capture, pcm) = open_audio(&h(&v, "audioPacket"), &keys.video).unwrap();
        assert_eq!((seq, capture), (99, 555));
        assert_eq!(pcm, h(&v, "audioPcm"));

        assert_eq!(udp_hello(5, &keys.video), h(&v, "udpHello"));
    }
}
