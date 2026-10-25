# Roadmap

Each milestone ends with something you can run and judge.

**Status (2026-10-20):** M1 and M2 built. M3 built and tested on one Mac with fault injection; the 4-hour soak on M1 → M4 is still to do. From M5: pairing key and encryption done. M0 was folded into M1, results below.

## M0 — Spikes (answer the risky questions first)

- Capture the screen and encode with VideoToolbox; log encode time per frame at native resolution.
- Check what the host's encoder supports: HEVC low-latency mode, 4:4:4, max resolution at 60 fps.
- Send dummy 1200-byte packets at 80 Mbps over the LAN and over Tailscale; measure loss and jitter.
- Try QUIC datagrams in Network.framework vs. plain UDP (feeds [0003](decisions/0003-udp-transport.md)).
- Set up signing so permissions survive rebuilds.

**Done when:** we know the codec, transport, and signing setup, with numbers.

**Results (M1 MacBook Air, macOS 26.3):** HEVC with low-latency rate control works on the M1's hardware encoder. 1080p60 encodes in about 3 ms per frame, and decode is 2–3 ms. Signing works without Xcode, using a local self-signed certificate (the account's Apple Development certificates are revoked). Transport: TCP control + plain UDP video (see [0003](decisions/0003-udp-transport.md)). Not checked yet: HEVC 4:4:4, behavior over Tailscale.

## M1 — Video on the LAN

Host streams one display; Mac client shows it. Keyframes on request. Per-stage timing overlay.

**Done when:** the picture looks right and the overlay shows the stage timings.

## M2 — Input

Mouse moves, clicks, scroll (with trackpad momentum), keyboard including Cmd shortcuts. Release all keys on disconnect.

**Done when:** you can work on the remote Mac normally for 30 minutes. First slow-motion click-to-photon test against Parsec.

## M3 — Reliability

Watchdogs, recovery ladder, heartbeats, auto-reconnect, power assertions, fault-injection menu.

**Done when:** 4-hour soak test with zero manual reconnects. **This is the milestone that decides whether bb1 beats Parsec for us.**

## M4 — Quality

Client-side cursor, bitrate tuning, 4:4:4 if the hardware supports it, 120 Hz client display, side-by-side text sharpness check against Parsec.

## M5 — Away from home

Pairing with a code, saved device keys, Tailscale path, direct/relayed indicator, adaptive bitrate, clipboard (text).

## M6 — Windows client

In testing (2026-10-25): `apps/windows`, Rust, unsigned. Hardware H.264 decode (Media Foundation) and Direct3D 11 display, keyboard hook for Windows-key shortcuts, sound (WASAPI), clipboard, Tailscale and home-network discovery, pairing with the 6-digit code. The host sends H.264 to it (Windows has no free HEVC decoder). Not yet: self-update, signing (SmartScreen asks "Run anyway" once).

## Ideas

- [MCP computer-use tool](ideas/mcp-computer-use.md): agents use a shared Mac.
- [iPhone app](ideas/phone-app.md): glance at the Mac, type a password when an agent needs it.

## Later / maybe

Browser client, audio, multiple monitors, file transfer, virtual display.
