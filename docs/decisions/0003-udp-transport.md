# 0003 — Video over UDP; exact framing decided in M0

**Status:** accepted. v1 uses option B (2026-10-20)

## Context

Parsec's low latency depends on UDP: a lost video packet is skipped and repaired instead of holding up every frame behind it like TCP does. We need the same at home and over Tailscale.

## Decision

Video and mouse moves go over UDP datagrams (≤ 1200 bytes each). Keys, clicks and control messages go over a reliable ordered channel.

Two ways to build it; M0 picks one:

| Option | Good | Bad |
|---|---|---|
| **A. QUIC** (Network.framework on Mac, msquic on Windows): datagrams for video, a stream for control | Encryption, reliable streams and connection handling built in; one connection | QUIC's congestion control may slow datagrams; less control over timing |
| **B. Plain UDP + small reliable layer** for control, Noise or TLS-PSK for encryption | Full control over timing; what Parsec-style tools usually do | We write and test retransmits and encryption ourselves |

Pick A if, at 80 Mbps on the LAN, it adds under 1 ms and doesn't throttle. Otherwise B.

## What v1 does

Option B, the simplest version of it:

- **Control:** TCP with Nagle off, port 47800. Pairing-key handshake (HMAC proofs both ways), then every message is sealed with ChaCha20-Poly1305 under per-session keys (HKDF).
- **Video:** UDP, port 47801. Each frame is sealed once and split into ≤1200-byte datagrams with a 20-byte header. Lost frames are not resent: the client asks for a keyframe. Big frames are paced (500 Mbps default) so a Wi-Fi hop doesn't drop the burst. Datagrams are marked as interactive video, so Wi-Fi gives them priority.
- **Mouse moves** go over TCP for now. On a LAN, loss is rare enough that this is fine. Move them to UDP if Wi-Fi testing shows stutter.

QUIC wasn't needed to get started. We can revisit it if loss over Tailscale/cellular calls for it.

## Consequences

- Same protocol at home and away; only the address changes (LAN IP vs Tailscale IP).
- `tailscale serve` is not used (it only forwards TCP/HTTP).
