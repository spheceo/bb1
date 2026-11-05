# Connecting over the internet without Tailscale: the bb1 relay

**Status:** design, 2026-11-05. Being built.

A friend in another city, without Tailscale (it kept failing to start on his PC), connects to the owner's PC through a small public server, **bb1-relay**. Nothing to configure on either router: the host and the viewer only make outgoing connections, so two routers (the owner's PC is behind a mesh behind the main router) and carrier-grade NAT don't matter.

Later, the relay also becomes the matchmaker for direct connections (hole punching), with relaying only as the fallback (see the end).

## What the relay sees

Only ciphertext. bb1's sign-in (pairing code, then a remembered pairing) and every message and video frame are end-to-end encrypted between host and viewer, exactly as on a home network or Tailscale. The relay joins connections and forwards packets; it can't read or change them, and a stranger who guesses a room code still has to pair with a code shown on the host's screen.

## How it works

Ports on the relay: **TCP 47810** (connections), and a **UDP range 47820–47899** (one port per relayed viewer).

1. **Host registers.** With "Share over the internet" on, the host opens a TCP connection to the relay and sends `BB1RELAY HOST <room>\n`; the relay answers `OK\n`. The room code is random, made once per host and remembered (8 letters/digits, shown as `ABCD-1234`). The host keeps this connection (a `PING`/`PONG` line every 20 s) and reconnects by itself if it drops.
2. **Viewer joins.** The viewer connects to the relay and sends `BB1RELAY JOIN <room>\n`.
3. **The relay asks the host for a connection** over its registration: `CONN <id> <udp-port>\n`, where `udp-port` is a UDP port the relay just set aside for this viewer.
4. **The host opens a second TCP connection** to the relay: `BB1RELAY ACCEPT <room> <id>\n`. The relay answers the viewer `OK\n` and from then on copies bytes both ways between the two connections. Both sides now speak the normal bb1 protocol over it, from the client's hello on.
5. **Video and sound (UDP).** The host remembers `udp-port` for this connection and puts it in `STREAM_CONFIG.udpPort` instead of its own video port. The viewer sends its UDP hello to the relay's address (the address of its TCP connection) at that port, as it always does. Before that, as soon as it gets `CONN`, the host sends a few small `BB1RELAY-PUNCH <id>` datagrams **from its video socket** to the relay at `udp-port` (and one every 15 s while the viewer is connected): that opens the host's routers from the inside and tells the relay where the host is. The relay then forwards every datagram that arrives on `udp-port`: from the host's address → to the viewer's (the last address a non-punch datagram came from), from anyone else → to the host. The host sees the viewer's hello arrive from the relay's port, so it sends video back there: no change to the video path at all.
6. **Ending.** When either TCP side closes, the relay closes the other and frees the UDP port after 30 s.

## In the apps

| Side | Change |
|---|---|
| Relay | New: `apps/relay` (Rust, std only, one binary). Logs connections and bytes per room, never content. Runs on any Linux server with a public IP; `systemd` unit and a short how-to |
| Windows host | Settings → Sharing → **Share over the internet** (off by default). Shows the code; a relay link thread (register, CONN → ACCEPT → hand the stream to the host like any other connection, punch, keep-alive, reconnect). `core/host.rs`: a connection can carry a relay UDP port, used in its `STREAM_CONFIG` |
| Windows client | `+` accepts a room code (`ABCD-1234`, or `relay:ABCD-1234`): connect to the relay, send `JOIN`, wait for `OK`, then the normal protocol. Remembered like any address |
| Mac client | The same in the "connect by address" field |
| Mac host | Later |

The relay address is a setting (`relay`, default the owner's server), so anyone can run their own.

## Where to run it

A small Linux VM in **Johannesburg**, with a public IPv4 address and those ports open. Bandwidth is the cost: one viewer at 30 Mbps is about 13 GB an hour, so prefer a host with included traffic (e.g. Oracle Cloud's free tier has a Johannesburg region and 10 TB a month), then Azure/Google/AWS (Johannesburg/Cape Town).

## Later: direct first

The relay already knows each side's public address (from the punch and the viewer's hello). Telling each side the other's address and having both send at once (hole punching) gives a direct path most of the time; the relay then only carries connections where that fails.
