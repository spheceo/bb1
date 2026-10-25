# Building and running

## No Xcode

Everything builds with the Command Line Tools that are already installed (Swift 6.2, macOS SDK). Checked on 2026-10-20: ScreenCaptureKit, VideoToolbox, CoreGraphics, Network and AppKit all compile with plain `swiftc`.

- **Build:** Swift Package Manager (`swift build`), from `apps/mac` (one `Package.swift` there). The repo is a monorepo; the Windows client is in `apps/windows` (Rust, see its README).
- **Editor:** anything. VS Code / Cursor with the Swift extension works well.
- **Debugger:** `lldb`, included with the Command Line Tools.
- **Signing:** published builds use **Developer ID Application: SIPHESIHLE GODSGIFT MBUYISA (5PLDN9BDV4)** (paid Apple Developer Program), with the hardened runtime and a secure timestamp. Published builds and their disk image are notarized by Apple and stapled, so a new Mac opens them without a warning. Without the Developer ID installed, `bundle.sh` falls back to **bb1 Local Signing**, a self-signed certificate that `scripts/signing-identity.sh` creates once.

  Both live in bb1's own keychain, `~/Library/Application Support/bb1/signing/bb1-signing.keychain-db` (password in `keychain-password` next to it, so builds never prompt). The Developer ID private key was made on the M1 (`developer-id/`); an encrypted `.p12` copy is stored as a GitHub Actions secret for release builds; the certificate expires 2027-11-20. To renew: make a new request from that key (or a new one), create a Developer ID Application certificate at developer.apple.com, import it into that keychain. The updater trusts any bb1 signed by team 5PLDN9BDV4, so renewals don't break updates or permissions.

  Local notarization uses the keychain profile `bb1-notary` (an app-specific password). GitHub uses the existing team API key also used by Edit, with its Key ID and Issuer ID stored as repository secrets. Rotating that shared API key means updating both repositories. Both the app and disk image are notarized and stapled before a GitHub release becomes visible.

  > Why not the Apple Development certificate: as of 2026-10-20 all of this account's Apple Development certificates are **revoked**, and Development certificates aren't for distribution anyway.

What we give up without Xcode: Interface Builder, SwiftUI previews and Instruments' GUI. None of them matter much for this project. Xcode can be installed later if Instruments becomes useful for profiling.

## Repo layout

```
bb1/
  apps/mac/         everything below is relative to this folder; run Mac commands from here
  Package.swift
  Sources/
    BB1Core/        protocol types, packetizer, transport (shared)
    bb1/            the app (App/, Host/, Client/); `bb1 host` / `bb1 client` from a terminal
    bb1-selftest/   protocol/crypto checks; `--vectors` writes ../../protocol/vectors.json
  Resources/
    Info.plist
    AppIcon.icns
  scripts/
    bundle.sh       wraps a built binary into a signed .app
    release.sh      .app → .dmg (install by hand) + .zip (in-app updates); --publish / --release put them on GitHub
    make-dmg.sh     the drag-to-Applications disk image (dmgbuild + make-dmg-background.swift)
    install.sh      development: build, install into /Applications, open
apps/windows/       the Windows client (Rust); builds on a Mac too (see its README)
protocol/           test vectors shared by both clients
docs/
```

Moving the Mac app into `apps/mac` didn't change its signature: the designated requirement is still `identifier "com.bb1.host"` + team `5PLDN9BDV4`, so permissions and updates carry on. GitHub Actions runs the Mac release from `apps/mac` and the Windows build (`.github/workflows/windows.yml`) only when `apps/windows` or `protocol` change.

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

Permissions are granted once to `BB1.app` (bundle ID `com.bb1.host`, kept from the old host app). Because every build has the same signature identity (bundle ID + Developer ID team), they survive updates. Moving from the local certificate to the Developer ID asks for them once more. Install the nightly-5 bridge before the first Developer ID update.

Don't open the apps straight from `build/`. `~/Documents` is a privacy-protected folder, and a signed app inside it hangs at launch waiting for permission to read its own files. Both Macs keep bb1 in `/Applications`.

An `.app` is just a folder (`BB1.app/Contents/MacOS/bb1` + `Contents/Info.plist`). `bundle.sh` makes it with `mkdir` and `cp` and then signs it. No Xcode project.

## Getting the client onto another Mac

Use the published disk image from GitHub Releases. Its app and disk image are signed with the Developer ID, notarized, and stapled. Keep local development builds for debugging; they are not notarized unless explicitly requested.


## GitHub builds and publishing

The public repository uses a standard Apple Silicon `macos-26` runner. Routine builds do not need either personal Mac online. Real Screen Recording, Accessibility, multi-window input, streaming performance, and Tailscale checks still happen on the M1 and M4.

- **Local development package:** `scripts/release.sh` makes a signed `.app`, `.dmg`, and update `.zip` in `build/` and `dist/`, without publishing or notarizing. For an explicit local notarization check: `BB1_NOTARIZE=1 scripts/release.sh`.
- **Test the full GitHub pipeline:** Actions → Release → Run workflow (or `gh workflow run release.yml`). By default it signs and notarizes, verifies Gatekeeper and stapled tickets, and keeps the files as seven-day Actions artifacts. It never creates a release. Untick notarization only to diagnose compilation/packaging while Apple is slow; those files are not publishable.
- **Publish a nightly:** commit changes and the changelog on `main`, then `scripts/release.sh --publish`. This pushes main and its `nightly-N` tag. GitHub builds and publishes the DMG and updater ZIP together. It no longer compiles on the local Mac.
- **Publish a stable version:** only at the owner's request, bump `VERSION`, add its `## v<VERSION>` changelog entry, commit, then `scripts/release.sh --release`. GitHub stamps `BB1Release` and publishes the normal latest release.
- **Failure:** a failed build, signature, notarization, or verification publishes nothing. If an upload fails, the release stays a draft. Fix the problem and rerun as appropriate; already public releases are never overwritten. Notarization submission IDs and results are retained separately even on failure.

The workflow fetches full history so the commit count and existing updater filenames remain unchanged. The ZIP always has `bb1.app` at its top level; nightly ZIPs remain `bb1-<VERSION>-build<N>.zip`, and stable ZIPs remain `bb1-v<VERSION>-build<N>.zip`. Nightly number = build count − 25. Nightlies remain pre-releases and do not replace the latest stable release.

Repository secrets (never commit these files):

- `MACOS_CERTIFICATE`: base64 of bb1's Developer ID certificate/private key as an encrypted `.p12` (not Edit's signing certificate).
- `MACOS_CERTIFICATE_PASSWORD`: that export's password.
- `NOTARY_KEY`: base64 of the team notarization API `.p8` key.
- `NOTARY_KEY_ID` and `NOTARY_ISSUER`: the API identifiers.

The runner imports signing material into a temporary keychain and deletes it and the API key even after a failed job. Missing credentials stop the job; published builds cannot fall back to local or ad-hoc signing. Build steps have read-only repository permission; only the final publishing job can write a release.
