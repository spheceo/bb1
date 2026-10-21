# bb1

A personal, low-latency remote desktop for macOS. One Mac at home is the **host**; you connect to it from a **client** (Mac first, Windows later, browser maybe).

The bar is Parsec: crisp image, input that feels local. The thing Parsec got wrong for us on a Mac host — the stream freezing every ~5 minutes until you reconnect — is the thing bb1 has to get right.

## Status

**Version 0.0.0 (first release), in daily testing.** Nightlies (bb1 nightly-1, nightly-2, …) arrive as in-app updates. One app, **BB1.app**, runs as host or client. The host streams its screen with mouse/keyboard control; lost packets are resent, the clipboard is shared both ways, both sides recover from stalls by themselves, and all logs end up on the host. To try it, follow **[TESTING.md](TESTING.md)**.

Built so far: roadmap M1 (video), M2 (input), most of M3 (watchdogs, recovery, fault injection) and part of M5 (pairing key, encryption). Not yet tested on the real M1 → M4 Wi-Fi path.

## Docs

| Doc | What's in it |
|---|---|
| [00-goals](docs/00-goals.md) | What we're building, what we're not, what "done" means |
| [01-architecture](docs/01-architecture.md) | Host, client, transport, and how they fit |
| [02-protocol](docs/02-protocol.md) | Wire format: video, input, control |
| [03-latency-and-quality](docs/03-latency-and-quality.md) | Latency budget, encoder settings, how we measure |
| [04-reliability](docs/04-reliability.md) | The freeze problem and how bb1 recovers on its own |
| [05-macos-limits](docs/05-macos-limits.md) | Permissions, sleep, lock screen, FileVault |
| [06-roadmap](docs/06-roadmap.md) | Milestones in build order |
| [07-building-and-running](docs/07-building-and-running.md) | Tooling (no Xcode), repo layout, how the host runs |
| [open-questions](docs/open-questions.md) | Things we still need to find out |
| [decisions/](docs/decisions/) | One note per decision, with the reasoning |
| [ideas/phone-app](docs/ideas/phone-app.md) | iPhone app: glance at your Mac, type a password when an agent needs it |
| [ideas/mcp-computer-use](docs/ideas/mcp-computer-use.md) | Let AI agents use a shared Mac through MCP tools |

## Shape of the code

One Swift package, built with the Command Line Tools (no Xcode). Details in [07](docs/07-building-and-running.md).

```
bb1/
  Package.swift
  Sources/
    BB1Core/      protocol, crypto, transport, clipboard sync (shared)
    bb1/          the app: App/ (menu bar, modes, settings), Host/ (capture, encode, input), Client/ (decode, display, input)
    bb1-selftest/ protocol and crypto checks (swift run bb1-selftest)
  scripts/        signed .app, .dmg and update zip; dev install; recovery and sign-in tests
  docs/
```

A Windows client comes later and is built against [02-protocol](docs/02-protocol.md).
