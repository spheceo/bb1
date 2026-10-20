# Changelog

Version numbers only change the last digit, and only when the owner says to make a proper release.
Everything in between is a test build: a GitHub pre-release `build-<N>` (`bb1-<version>-build<N>.zip`),
offered in the app when "Include test builds" is on.

## Test builds since v0.0.0

Nothing yet.

## v0.0.0 — 2026-10-20 (first release)

Everything so far.

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
- **Caps Lock** on the client now works on the host: its state is synced, and the host's real Caps Lock follows it.
- Fixed: two cursors on first connect when using this Mac's cursor. The setting could arrive while capture was starting, and got lost.
- The host's capture-freeze check needs 3+ clicks or keys with no new frame for 4 s, so a single quiet click can't trigger it.
- The app version is shown in Settings → About and logged at startup.
- **Updates from inside the app:** bb1 checks GitHub every hour. When a newer build is out, a banner on Your Macs offers it; one click updates your other Macs that are behind (you see each one download, install, restart and come back on its card), then this Mac, which restarts into the new build. Settings → Updates shows the version and build, has Check for Updates, and "Include test builds". Downloads are only installed if they're signed with bb1's own certificate, so permissions carry over.
- Fixed: opening Settings froze the app (builds 6 and 7).
- **New look, matched to the icon:** flat, near-black greys (no gradients or shadows), white buttons and switches instead of system blue, and each Mac drawn with the bb1 mark on its screen (white when online, dim when offline or not shared).
