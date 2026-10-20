# Changelog

Versions only change the last number (v0.0.1, v0.0.2, …) unless decided otherwise.

## v0.0.2 — 2026-10-20

- **Fixed:** two cursors showed on first connect when using this Mac's cursor. The client's choice could arrive while the host was still starting screen capture, and got lost. It's now applied as soon as capture starts.
- The app version is shown in Settings → About and written to the log at startup.
- Releases are built as `dist/bb1-v<version>.zip` (`scripts/release.sh`).

## v0.0.1 — 2026-10-20

First version.

- **One app for both roles.** Share this Mac and/or connect to your other Macs. Parsec-style dashboard (Your Macs, Settings), menu bar icon, Dock icon options, keeps running in the menu bar.
- **Streaming:** native-resolution screen capture (ScreenCaptureKit), hardware HEVC in low-latency mode, 60 fps, UDP with packet resend on loss, ~10–15 ms capture-to-screen on a home network.
- **Control:** mouse, trackpad scrolling with momentum, keyboard including Cmd shortcuts (with Accessibility on the client). Input modes: keyboard & mouse, keyboard only, mouse only, view only. Release input with ⌃⌥⇧R.
- **This Mac's cursor by default:** instant, and it takes the host's shape (arrow, pointing hand, I-beam…).
- **Sound** from the host with an adaptive buffer. Silence isn't sent. Mute with ⌃⌥⇧M.
- **Clipboard** shared both ways (text and images). Password-manager copies are skipped.
- **Sign-in without keys:**
  - Tailscale (devices on your account are let in)
  - a one-time 6-digit pairing code
  - remembered pairings
  - the old pairing key is upgraded automatically
- **Away from home over Tailscale:** shows whether the route is direct or relayed.
- **Self-recovery:** watchdogs on every stage (capture, encoder, network, decoder, display), automatic reconnect, and a fault-injection test.
- **Logging:** both sides' per-second stats (including CPU and memory) in the host's log, plus stall, hitch and memory-pressure lines.
- **Stream options:** fullscreen, fill screen, scaling (fit / stretch / actual pixels).
- **Idle cost:** about 1% CPU on the client when the screen is still.
