# Building and running

## No Xcode

Everything builds with the Command Line Tools that are already installed (Swift 6.2, macOS SDK). Checked on 2026-10-20: ScreenCaptureKit, VideoToolbox, CoreGraphics, Network and AppKit all compile with plain `swiftc`.

- **Build:** Swift Package Manager (`swift build`). One `Package.swift` at the repo root.
- **Editor:** anything. VS Code / Cursor with the Swift extension works well.
- **Debugger:** `lldb`, included with the Command Line Tools.
- **Signing:** from nightly-6, **Developer ID Application: SIPHESIHLE GODSGIFT MBUYISA (5PLDN9BDV4)** (paid Apple Developer Program), with the hardened runtime and a secure timestamp. Published builds and their disk image are notarized by Apple and stapled, so a new Mac opens them without a warning. Without the Developer ID installed, `bundle.sh` falls back to **bb1 Local Signing**, a self-signed certificate that `scripts/signing-identity.sh` creates once.

  Both live in bb1's own keychain, `~/Library/Application Support/bb1/signing/bb1-signing.keychain-db` (password in `keychain-password` next to it, so builds never prompt). The Developer ID private key was made on the M1 (`developer-id/`), never uploaded; the certificate expires 2027-11-20. To renew: make a new request from that key (or a new one), create a Developer ID Application certificate at developer.apple.com, import it into that keychain. The updater trusts any bb1 signed by team 5PLDN9BDV4, so renewals don't break updates or permissions.

  Notarization uses the keychain profile `bb1-notary` (`xcrun notarytool store-credentials bb1-notary --apple-id … --team-id 5PLDN9BDV4`, with an app-specific password). `release.sh --publish` / `--release` submit the app, staple it, then sign, submit and staple the `.dmg`.

  > Why not the Apple Development certificate: as of 2026-10-20 all of this account's Apple Development certificates are **revoked**, and Development certificates aren't for distribution anyway.

What we give up without Xcode: Interface Builder, SwiftUI previews and Instruments' GUI. None of them matter much for this project. Xcode can be installed later if Instruments becomes useful for profiling.

## Repo layout

```
bb1/
  Package.swift
  Sources/
    BB1Core/        protocol types, packetizer, transport (shared)
    bb1/            the app (App/, Host/, Client/); `bb1 host` / `bb1 client` from a terminal
  Resources/
    Host-Info.plist
    Client-Info.plist
  scripts/
    bundle.sh       wraps a built binary into a signed .app
    release.sh      .app → .dmg (install by hand) + .zip (in-app updates); --publish / --release put them on GitHub
    make-dmg.sh     the drag-to-Applications disk image (dmgbuild + make-dmg-background.swift)
    install.sh      development: build, install into /Applications, open
  docs/
```

## What the host is

**A menu bar app that starts when you log in.** No Dock icon and no window, just an icon in the menu bar.

| Option | Verdict |
|---|---|
| Plain CLI in a Terminal window | Fine for development. Not for daily use: closing the terminal kills it, and macOS gives the permissions to Terminal, not to bb1. |
| Background daemon (runs as root at boot) | Doesn't work. Screen capture and input injection have to run inside your logged-in session. |
| **Menu bar app, started at login, restarted if it crashes** | ✅ This one. |

The menu bar icon shows:

- status: idle / client connected / something wrong
- missing permissions, with a button that opens the right System Settings pane
- the pairing code when a new device connects
- stats (resolution, fps, bitrate, latency) and the fault-injection menu in debug builds
- Quit

**Starting and staying up:** a LaunchAgent (`~/Library/LaunchAgents/com.bb1.host.plist`) starts the app at login and has `KeepAlive` set, so launchd restarts it within seconds if it crashes. This is the outermost step of the [recovery ladder](04-reliability.md).

## What the client is

A normal Mac app with a window: a list of hosts (found via Bonjour, or a typed address), then the remote screen in a window or fullscreen.

## Two ways to run

**Development (fast loop):**

```sh
swift run bb1 host      # from a terminal
swift run bb1 client    # from a terminal on the client Mac
```

macOS attributes permissions to the terminal app here, so grant Screen Recording and Accessibility to the terminal once.

**Updates:** once installed, bb1 updates itself from GitHub releases (see [decision 0005](decisions/0005-self-update.md)). `scripts/release.sh --publish` publishes a nightly (`bb1 nightly-<N>`); the app offers it as an update, and the client can update the host too.

**Daily use:**

Install from the `.dmg` (drag to Applications), or from source with `scripts/install.sh`. The app sets itself up: it offers to move into Applications if opened from the disk image, quits older copies, and writes the login item (`~/Library/LaunchAgents/com.bb1.host.plist`, start at login + restart after a crash) when you share the Mac. To take it off a Mac, turn off Open at login in Settings, quit it and drag it to the Trash.

Permissions are granted once to `BB1.app` (bundle ID `com.bb1.host`, kept from the old host app). Because every build has the same signature identity (bundle ID + Developer ID team), they survive updates. Moving from the local certificate to the Developer ID (nightly-6) asked for them once more.

Don't open the apps straight from `build/`. `~/Documents` is a privacy-protected folder, and a signed app inside it hangs at launch waiting for permission to read its own files. Both Macs keep bb1 in `/Applications`.

An `.app` is just a folder (`BB1.app/Contents/MacOS/bb1` + `Contents/Info.plist`). `bundle.sh` makes it with `mkdir` and `cp` and then signs it. No Xcode project.

## Getting the client onto another Mac

Same bundle script. The M4 doesn't trust the local certificate, which is fine for running it. Because it isn't notarized, the copied app needs its quarantine flag removed once (see [TESTING.md](../TESTING.md)). Sharing with other people would need a paid Developer ID and notarization; out of scope for now.
