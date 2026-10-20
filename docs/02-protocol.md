# Protocol (draft)

Written so a Windows or browser client can be built from this page alone. All integers are little-endian.

## Channels

| Channel | Carries | Delivery |
|---|---|---|
| **Video** | Encoded frames, split into packets | Unreliable. Lost packets are not resent; the client asks for recovery instead. |
| **Control** | Handshake, keys, mouse buttons, clipboard, keyframe requests, stats | Reliable, in order |
| **Pointer** | Mouse moves, scroll | Unreliable, newest wins |

Whether these are one QUIC connection (streams + datagrams) or UDP plus a small reliable layer is decided in [0003](decisions/0003-udp-transport.md).

## Packet size

Video payload per packet: **≤ 1200 bytes**. Tailscale's MTU is 1280, so this fits both at home and away without the network splitting packets.

## Video packet header

```
u8   type          = 0x01
u8   flags         bit0 keyframe, bit1 last packet of frame
u16  stream_epoch  bumps every time the encoder restarts
u32  frame_id
u16  packet_index
u16  packet_count
u64  capture_time_us  host clock, for latency measurement
...  payload (HEVC/H.264 bytes, Annex B)
```

The client assembles a frame once all `packet_count` packets arrive. If a frame is missing packets after a short wait (about 1 frame interval), it's dropped and the client sends `RECOVER` (below). A frame from an older `stream_epoch` is thrown away.

## Control messages

| Message | Direction | Purpose |
|---|---|---|
| `HELLO` / `HELLO_ACK` | both | Version, device key, client screen size, codecs supported |
| `STREAM_CONFIG` | host → client | Codec, resolution, fps, parameter sets (VPS/SPS/PPS) |
| `KEY` | client → host | Key code, down/up, modifier state |
| `MOUSE_BUTTON` | client → host | Button, down/up, position |
| `RECOVER` | client → host | "I lost frame N." Host sends a keyframe (or later, a recovery frame from a long-term reference) |
| `PING` / `PONG` | both | Every 250 ms. Round-trip time, clock offset, heartbeat, and frame counters for stall detection: host `captured`/`encoded`/`sent`, client `received`/`decoded`/`displayed` (see [04](04-reliability.md)) |
| `STATS` | client → host | Loss, decode time, display time; used for bitrate decisions |
| `CLIPBOARD` | both | Later |
| `BYE` | both | Clean disconnect |

## Pointer messages

```
u8   type = 0x10 move | 0x11 scroll
u32  seq            client ignores older seq numbers on the host side
f32  x, y           0..1 relative to the remote display (moves)
f32  dx, dy         scroll deltas, with phase for trackpad momentum
```

Positions are relative (0..1) so the client window can be any size.

## Keyboard codes

- **Mac → Mac:** send macOS virtual key codes as-is. Nothing to translate.
- **Windows → Mac:** client sends USB HID usage codes; host maps them to Mac key codes. The Windows key maps to Cmd by default, and this can be changed.
- Modifier state is sent with every key, so a lost or stuck modifier fixes itself on the next keystroke.
- When the client loses focus or disconnects, the host releases every key and button it thinks is held. This prevents a stuck Cmd or Shift on the Mac.
