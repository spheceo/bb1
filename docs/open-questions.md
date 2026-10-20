# Open questions

## For the owner

1. ~~Which Mac is the host?~~ **M1 MacBook Air (8 GB, macOS 26.3)** on Ethernet, with a 1920×1080 Dell as its main display.
2. ~~Desktop or laptop?~~ **Laptop.** It needs to stay on power with the lid closed.
3. ~~Client on Wi-Fi or Ethernet?~~ **M4 Mac on Wi-Fi.** It's still worth trying Parsec once over Ethernet to see whether the freeze follows the Wi-Fi.
4. ~~Did the Parsec freeze also stop input, or only the picture?~~ **Only the picture.** Input kept reaching the Mac. The video path stalled, not the connection. See [04](04-reliability.md#what-we-know-about-the-parsec-freeze-2026-10-20).
5. **Is FileVault on, and how often does the host reboot?** Decides how much the login-window limit matters.
6. ~~Okay with installing full Xcode?~~ Not needed. The Command Line Tools build everything, and a local self-signed certificate signs it (the Apple Development certificates turned out to be revoked). See [07](07-building-and-running.md).

## Technical, answered in M0

7. ~~Does low-latency rate control work with HEVC on the host?~~ **Yes** on the M1 (2026-10-20).
8. Does the host's hardware encoder do HEVC 4:4:4 in real time?
9. QUIC datagrams (Network.framework) or plain UDP + our own reliable layer? See [0003](decisions/0003-udp-transport.md).
10. `AVSampleBufferDisplayLayer` vs Metal for client display latency.
11. Best way to get the current cursor image on the host for client-side cursor rendering.

## Later

12. Windows client language: native C++ or Rust (Media Foundation + Direct3D, lowest latency) vs. TypeScript in Electron with WebCodecs (shares code with a future browser client, a bit slower). Decide at M6 with real latency numbers from the Mac client to compare against.
