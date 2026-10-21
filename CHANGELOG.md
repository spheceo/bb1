# Changelog

Version numbers only change the last digit, and only when the owner says to make a proper release.
Everything in between is a test build: a GitHub pre-release `build-<N>` (`bb1-<version>-build<N>.zip`),
offered in the app when "Include test builds" is on.

## Test builds since v0.0.0

- Menu bar menu: "Not connected" and "Connect…" are gone (Open bb1 does the same); the connection lines only appear while you're connected.
- **Control follows the pointer:** on the host's picture, keyboard and mouse go to the host; off it (title bar, black bars, outside the window) they're this Mac's again, and back on gives the host control again. No more releasing input by hand.
- **Simpler shortcuts:** the ⌃⌥⇧ chords are gone. ⌘⎋ gives control back without moving the pointer (the way out of full screen); the Stream menu uses normal Mac shortcuts while this Mac has control (⌃⌘F full screen, ⌘I stats).
- **Readable stream overlay:** stats and messages sit on a solid dark card with padding and a shadow, so they read over any picture; stats wrap between items, never inside one; warnings are amber.
- **Updating from the menu bar stays in the menu bar:** "Update to build N" updates your other Macs and then this one without opening the window, and the menu shows the progress. After any update bb1 comes back the way it was (window open, or just the menu bar icon). The menu always shows the version ("bb1 0.0.0 · build 21").
- Every menu bar item that does something has an icon (Settings and Quit had one, the rest didn't).

- **Faster recovery when macOS stops screen capture:** the host restarts capture as soon as macOS signals it (about 3 s sooner than before). On a nearly full disk, macOS's space clean-up (`replayd`) stops every screen capture; when that happens, or at connect, the client is told the host is low on disk space (under 5 GB free). Host notices on the stream now fade after 10 s, apart from "The host can't…" warnings.
- **Remove bb1 is gone from Settings** (turn off Open at login and drag bb1 to the Trash instead).
- A Mac too old to send its screen now says so on its tile instead of just "Online": "Online · update to see its screen", or for a bb1 from before updates, "Online · old bb1, update it there" (it has to be installed by hand once). Such a Mac is re-checked every 15 minutes, so once it's updated by hand its screen shows up without connecting to it first.
- **New Settings page:** five sections (Sharing, Who can connect, App, Updates, Help), each a short title and one line on the left with its settings grouped in one card on the right, divided by hairlines. Shorter wording, small buttons, and a segmented picker for the Dock icon instead of a macOS pop-up menu.
- **Header polish:** a taller title bar, so the header has room; the bb1 mark, gear, avatar and Settings' back button line up exactly with the window buttons (measured from them, not guessed); the gear is a true circle with no border (just a soft circle on hover); the right side mirrors the window buttons' inset; the back arrow is a proper chevron. Offline Macs' last screens are now properly greyed out (they rendered as black specks).
- **New Your Macs page (direction A):** each of your other Macs is a tile showing its actual screen, refreshed every few seconds while the window is open ("Online · 9 ms"); offline Macs show their last screen, greyed ("Offline · seen 2 h ago"). Click a tile to connect. This Mac is a dashed tile with the bb1 mark and the sharing switch. No sidebar: Settings is behind the gear, with a back button. The host sends the pictures over a quiet side connection that doesn't touch a stream (hosts need build 15+).
- Checking a host's build now happens at most every 5 minutes (it ran every 2 s while the window was open), and also for newly paired Macs.
- **Update pill:** a new build shows up by itself as a pill in the Your Macs header ("Update to build 13"). Click it to update; the pill shows the progress, then "Updated to build 13", which fades away after a few seconds. bb1 checks every 5 minutes and whenever you bring it to the front (no need for Check for Updates).
- **Installs from a disk image:** a `.dmg` whose window is the bb1 mark (half light, half graphite); an arc carries bb1 across the split into Applications. The zip stays for in-app updates.
- **No more install scripts:** bb1 sets itself up. Opened from the disk image or Downloads, it offers to move into Applications. A new copy quits older ones, points the login item at itself and puts the old `~/Applications` copy in the Trash. Turning on Share This Mac also turns on start at login (with restart after a crash). Settings → Remove bb1 takes it off the Mac. `install-host.sh` and `uninstall-host.sh` are gone.

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
