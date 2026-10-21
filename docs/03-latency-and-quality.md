# Latency and quality

## Latency budget (LAN, 60 fps, Mac → Mac)

| Stage | Target | Notes |
|---|---|---|
| Capture | ≤ 8 ms avg | Up to one frame interval in the worst case. Lower queue depth where possible. |
| Encode | ≤ 5 ms | Hardware encoder, no frame reordering |
| Packetize + network | ≤ 3 ms | Gigabit Ethernet host, client on Ethernet or 5 GHz Wi-Fi |
| Reassemble + decode | ≤ 5 ms | Hardware decoder |
| Display | ≤ 10 ms | Show right away, no buffering for smoothness |
| **Total** | **≤ 35 ms median** | Click-to-photon |

These are targets until we measure. If the client display runs at 120 Hz (ProMotion), the display stage roughly halves.

**First measurements (2026-10-20, M1 host → client on the same Mac, 1080p real screen):** encode ~3 ms, decode ~2–3 ms, capture → handed to the display ~7–11 ms. This leaves out the network and the display's own refresh. M1 → M4 over Wi-Fi is still to measure: the client's stats overlay (⌘I) shows it live.

## Encoder settings (starting point)

| Setting | Value | Why |
|---|---|---|
| Codec | HEVC; H.264 fallback | Both Macs have hardware HEVC. Sharper text at the same bitrate. |
| Frame reordering (B-frames) | off | B-frames add whole frames of delay |
| Real-time mode | on | Encoder favors speed |
| Low-latency rate control | on where supported | Apple's low-latency encoder mode; check which codecs support it on our Macs |
| Keyframe interval | very long, keyframes on request | Regular keyframes cause bitrate spikes and stutter |
| Bitrate (LAN) | 40–80 Mbps, adjustable | Bitrate is cheap on a LAN; this is where crispness comes from |
| Resolution | Host's native pixel resolution | No scaling blur |
| Frame rate | 60 fps; send nothing when the screen doesn't change | Idle screen costs nothing |
| Chroma | 4:2:0 first; test 4:4:4 | 4:2:0 blurs small colored text. Whether the hardware encoder does HEVC 4:4:4 depends on the chip. To test in M0. |

Later: long-term reference frames, so a lost packet can be repaired with a small frame instead of a full keyframe.

## Cursor

Parsec draws the cursor on the client, so it moves at local speed even if video is a frame behind. bb1 does the same from M4:

- Host leaves the cursor out of the captured image.
- Host sends the current cursor image + hotspot when it changes.
- Client draws it at the local mouse position.

Until then, the cursor is part of the video, which is simpler but feels a little slower.

## How we measure

**Per-stage timings (always on):** each frame carries its capture time. The client estimates the clock offset from PING/PONG and logs, per frame: capture → encoded → sent → received → decoded → displayed. An overlay (toggle with a hotkey) shows these live, plus loss and bitrate.

**Click-to-photon (by hand, now and then):** film both screens with a phone in slow-motion mode (240 fps ≈ 4 ms per frame). Click on the client; count frames until the remote button changes. Do the same with Parsec as a baseline.
