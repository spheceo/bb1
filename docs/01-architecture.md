# Architecture

```
HOST (home Mac, Swift, runs at login)              CLIENT (Mac, Swift)
─────────────────────────────────────              ───────────────────
ScreenCaptureKit ─ frames ─┐                       ┌─ VideoToolbox decode
                           ▼                       │        │
                   VideoToolbox encode             │        ▼
                   (HEVC, low latency)             │  Metal / AVSampleBufferDisplayLayer
                           │                       │
                           ▼                       │
                   Packetizer ── UDP datagrams ──▶ Reassembler
                                                   │
CGEvent injection ◀── input + control (reliable) ◀─┴─ Input capture
                                                      (NSEvent + CGEventTap for Cmd shortcuts)
Watchdogs on every stage ──── heartbeats both ways ──── Watchdogs on every stage
```

## Host

One signed macOS app with a menu bar icon. Started at login.

| Piece | API | Job |
|---|---|---|
| Capture | ScreenCaptureKit (`SCStream`) | Grab the display at native pixel size, up to 60 fps |
| Encode | VideoToolbox (`VTCompressionSession`) | Hardware HEVC (H.264 fallback), low-latency settings |
| Send | Network.framework | Split frames into packets, send, handle keyframe requests |
| Input | `CGEvent` posted to the HID event tap | Replay mouse and keyboard from the client |
| Stay awake | `ProcessInfo.beginActivity`, IOKit power assertions | No App Nap, no idle sleep while a client is connected |
| Health | Watchdogs | Notice when capture or encode stops producing and restart that stage |

## Mac client

A normal windowed or fullscreen app.

| Piece | API | Job |
|---|---|---|
| Receive | Network.framework | Reassemble frames, detect loss, ask for keyframes |
| Decode | VideoToolbox (`VTDecompressionSession`) | Hardware decode |
| Display | `AVSampleBufferDisplayLayer` (first), Metal (if we need more control) | Show each frame as soon as it's decoded |
| Input | `NSEvent` + `CGEventTap` | Capture everything, including system shortcuts, while focused |
| Health | Watchdogs | Notice frozen video and climb the recovery ladder ([04](04-reliability.md)) |

## Transport

Video goes over **UDP**, not TCP, so one lost packet doesn't hold up later frames. Input and control need to arrive, so they go over a reliable channel. See [decision 0003](decisions/0003-udp-transport.md).

- **At home:** client connects to the host's LAN address directly.
- **Away:** same protocol over the host's Tailscale address. Tailscale handles getting through routers; when it gets a direct path, it adds very little latency. The client shows whether the path is direct or relayed.

## Discovery and pairing

- On the LAN the host advertises itself with Bonjour (`_bb1._udp`), so the client lists it without typing an IP.
- First connection: the host shows a 6-digit code, you type it on the client, and both sides save each other's key. After that, only paired devices can connect.

## Windows and browser clients

They implement the same protocol from [`spec/`](02-protocol.md). They don't change the host. Windows comes after the Mac client is reliable. A browser client would need a small bridge on the host (WebTransport or WebRTC) and is not planned yet.
