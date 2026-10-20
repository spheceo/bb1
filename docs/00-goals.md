# Goals

## What bb1 is

A remote desktop for one home Mac, used by its owner, from their own devices.

- **Host:** a Mac on Ethernet at home. Always the Mac.
- **Clients:** Mac (priority), Windows (later), browser (nice to have, not required).
- **Network:** home LAN first. Away from home over Tailscale second.

## Must have (v1)

1. **One monitor, no audio.**
2. **Parsec-level image quality.** Text should be sharp at native resolution — no smeared colored text, no blocky scrolling.
3. **Parsec-level input latency.** Clicking a button should feel immediate on the LAN.
4. **Runs for hours without a manual reconnect.** This is the main thing Parsec failed at on a Mac host. If anything stalls, bb1 recovers by itself within about a second.
5. **Mouse and keyboard, including Cmd shortcuts** (Cmd+Tab, Cmd+Space, Cmd+Q go to the remote Mac, not the local one, when the client is focused).
6. **Only our devices can connect.**

## Should have (after v1)

- Clipboard sync (text first)
- Connect from outside the house via Tailscale
- Adaptive bitrate for weaker networks
- Windows client

## Not now

- Audio
- Multiple monitors
- File transfer
- Multiple people connected at once
- Controlling the login window / reaching the Mac before someone logs in (see [05-macos-limits](05-macos-limits.md))
- Browser client

## How we'll know it's good enough

| Check | Target |
|---|---|
| Soak test on LAN, Mac → Mac | 4 hours, zero manual reconnects |
| Click-to-photon latency on LAN | ≤ 35 ms median, measured (see [03](03-latency-and-quality.md)) |
| Recovery after a stall or network blip | picture back within 1 s, no user action |
| Text sharpness | small colored text readable at 100% zoom, side-by-side with Parsec |
