# Reliability: no more freezing

## The problem

With a Mac host, Parsec froze after about 5 minutes and only a disconnect + reconnect fixed it. Windows host → Windows client ran for hours without this.

We don't know Parsec's root cause. Things that can make a Mac streaming session stall:

| Possible cause | What it looks like |
|---|---|
| Capture stops sending frames (system stopped the stream, display change, permission re-check) | Picture freezes; input may still work |
| Encoder session becomes invalid (sleep, GPU change, encoder error) | Picture freezes; host gets errors |
| Client decoder waits forever for a keyframe after one lost packet | Picture freezes; host is fine |
| App Nap or timer throttling slows the host app | Gradual lag, then a stall |
| Client Mac on Wi-Fi: AirDrop/AirPlay radio (AWDL) briefly switches channels | Regular lag spikes; usually not a full freeze |
| Reconnect logic that only runs when the user reconnects | A fixable stall turns into a permanent one |

Whatever the cause, bb1 is built so **any stage can stall and the session fixes itself.** Reconnecting by hand fixed Parsec, which suggests the problem is a stuck state that a restart clears. bb1 makes that restart automatic and narrow.

### What we know about the Parsec freeze (2026-10-20)

**Only the picture froze. Input kept working.** Clicks and typing during the freeze reached the Mac; after reconnecting, their results were on screen.

So the connection and the input path were fine. Something in the **video path** got stuck: host capture, host encoder, the video packets, or the client's decoder/display. The connection-level heartbeat (step 4 below) would **not** have caught this, because the connection never died. That's why bb1 checks video separately from the connection.

### Finding which video stage stalled

Each heartbeat (`PING`/`PONG`, every 250 ms) carries frame counters from both sides:

| Counter | Side |
|---|---|
| `captured` | host: frames from ScreenCaptureKit |
| `encoded` | host: frames out of the encoder |
| `sent` | host: frames fully sent |
| `received` | client: frames fully reassembled |
| `decoded` | client: frames out of the decoder |
| `displayed` | client: frames shown |

When the screen is changing (for example, input just arrived on the host), the first counter that stops increasing shows where the stall is:

| Stops increasing | Stalled stage | Fix (ladder step) |
|---|---|---|
| `captured` | Host capture | Restart the capture stream (3) |
| `encoded` | Host encoder | Rebuild the encoder (3) |
| `received` while `sent` keeps going | Video packets lost on the way | Keyframe; if it continues, reconnect video only (1, 4) |
| `decoded` | Client decoder | Reset the decoder and ask for a keyframe (2) |
| `displayed` | Client display layer | Rebuild the display layer (2) |

**Main trigger on the host:** input arrived but no new frame was captured within ~300 ms. Clicking or typing almost always changes the screen, so this catches a capture stall within a fraction of a second of you using the Mac. It also covers the case we saw with Parsec.

## Rules

1. **Every stage reports that it's alive.** Capture, encode, send, receive, decode, display each record "last produced at."
2. **An idle screen is not a stall.** When nothing on screen changes, capture sends no frames. The host still sends a heartbeat every 250 ms, so the client can tell "nothing changed" from "something broke."
3. **Recover from the smallest step upward.** Don't drop the connection when a keyframe would do.
4. **Log every recovery** with the reason, so we find root causes instead of hiding them.

## Recovery ladder

| Step | Trigger | Action | Expected time |
|---|---|---|---|
| 1 | Client missing packets for a frame | `RECOVER` → host sends keyframe | < 50 ms |
| 2 | Client `decoded` or `displayed` stalls while `received` keeps going | Client resets its decoder (or display layer), asks for a keyframe | < 200 ms |
| 3 | Host `captured` or `encoded` stalls while the screen is changing (input arrived, no frame within ~300 ms), or encoder error | Host rebuilds the encoder (new `stream_epoch`), then the capture stream if needed | < 500 ms |
| 4 | No heartbeat for 2 s, **or** video still stalled 2 s after steps 1–3 | Client reconnects the transport, same pairing, no user action | < 1 s on LAN |
| 5 | Still broken after 3 reconnects | Client shows a clear message with the last error; host restarts its pipeline | — |

The user should only ever see steps 4–5 as a brief "Reconnecting…" badge.

## Host settings that keep it awake

- `ProcessInfo.beginActivity` with latency-critical and user-initiated options while a client is connected (stops App Nap).
- IOKit assertions to prevent idle system sleep and display sleep while connected. Display sleep matters: a sleeping display can stop capture.
- Watch for display reconfiguration and system wake notifications and rebuild capture on purpose, instead of waiting for it to fail.

## Testing

- **Soak test:** leave a session running for 4+ hours with a script that moves windows and scrolls on the host. Count recoveries per step. Target: zero step 4–5 events on LAN.
- **Fault injection:** a debug menu that kills capture, invalidates the encoder, drops 5% of packets, or blocks the network for 3 s. Each must recover without a hand on the keyboard.
- **Test the Wi-Fi spike separately:** run the client on Ethernet once. If spikes disappear, it's the client's Wi-Fi, not bb1.
