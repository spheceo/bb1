# Building and running

## No Xcode

Everything builds with the Command Line Tools that are already installed (Swift 6.2, macOS SDK). Checked on 2026-10-20: ScreenCaptureKit, VideoToolbox, CoreGraphics, Network and AppKit all compile with plain `swiftc`.

- **Build:** Swift Package Manager (`swift build`). One `Package.swift` at the repo root.
- **Editor:** anything. VS Code / Cursor with the Swift extension works well.
- **Debugger:** `lldb`, included with the Command Line Tools.
- **Signing:** `codesign` with **bb1 Local Signing**, a self-signed certificate that `scripts/signing-identity.sh` creates once in its own keychain (`~/Library/Application Support/bb1/signing/`).

  > Why not the Apple Development certificate: as of 2026-10-20 all of this account's Apple Development certificates are **revoked**. macOS reports apps signed with a revoked certificate as malware and moves them to the Trash. A local certificate can't be revoked, and macOS ties permissions to it just as well.

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
    install-host.sh copies BB1.app to ~/Applications (Host mode), installs the LaunchAgent
  launchd/
    com.bb1.host.plist
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

**Daily use:**

```sh
scripts/bundle.sh           # swift build -c release → BB1.app → codesign
scripts/install-host.sh     # copy to ~/Applications, load the LaunchAgent
```

Permissions are granted once to `BB1.app` (bundle ID `com.bb1.host`, kept from the old host app). Because every build is signed with the same certificate, they survive rebuilds.

Don't open the apps straight from `build/`. `~/Documents` is a privacy-protected folder, and a signed app inside it hangs at launch waiting for permission to read its own files. `install-host.sh` copies the host to `~/Applications`, and the client goes to `/Applications` on the M4.

An `.app` is just a folder (`BB1.app/Contents/MacOS/bb1` + `Contents/Info.plist`). `bundle.sh` makes it with `mkdir` and `cp` and then signs it. No Xcode project.

## Getting the client onto another Mac

Same bundle script. The M4 doesn't trust the local certificate, which is fine for running it. Because it isn't notarized, the copied app needs its quarantine flag removed once (see [TESTING.md](../TESTING.md)). Sharing with other people would need a paid Developer ID and notarization; out of scope for now.
