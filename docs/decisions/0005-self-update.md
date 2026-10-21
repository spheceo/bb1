# 0005 — Self-update from GitHub releases, host updated through the client

**Status:** accepted (2026-10-20)

## Context

Every change meant building a zip, sending it to the M4 over Taildrop, and reinstalling the host by hand. The owner wants to click Update on the client and have both Macs move to the new build, watching the host update from the client.

## Decision

- **Source:** releases on `github.com/spheceo/bb1` (public). Real releases are normal GitHub releases (tag `v<VERSION>`); nightlies are pre-releases titled `bb1 nightly-<M>` (tag `nightly-<M>`, M = build − 25; before nightly-1 they were "test builds", tag `build-<N>`). Updates compare build numbers; the zip keeps its `bb1-<VERSION>-build<N>.zip` name, which installed copies parse. The app reads the releases list, no login needed.
- **What's "newer":** the build number (`CFBundleVersion`, the commit count), taken from the zip name `bb1-…-build<N>.zip`. VERSION stays frozen, so the version string can't be used.
- **Channel:** Settings → Updates → Channel. Stable offers only releases, Nightly only nightlies (the default; it was called Test builds, and "Include test builds" before that). Never a downgrade: a nightly newer than the latest release waits for the next one. The app calls itself "bb1 nightly-<M>", or "bb1 <VERSION>" when it's a release (`BB1Release` in Info.plist).
- **Leftovers:** a cut-short update can leave `.bb1-update` (with the previous bb1.app) next to the app or a zip in the temp folder; the next start removes them (15 s in, after the installer's own clean-up).
- **Install:** download, unzip next to the app, check the bundle ID, the build number, and that it satisfies this copy's designated requirement (same bundle ID and the same `bb1 Local Signing` certificate). Then a small script, started in its own session so launchd doesn't kill it, waits for the app to exit, swaps the bundles, and starts the new one: `launchctl kickstart` if bb1 is the `com.bb1.host` login item, `open` otherwise.
- **Host:** the client opens a short control connection marked `purpose: "update"` (it doesn't touch a running stream) and sends `updateHost(build)`. The host downloads that build from GitHub itself and reports `updateStatus` (downloading with progress, installing, restarting). The client then reconnects every 2 s until the host answers with the new build.
- **One button:** Update on the client updates every online Mac that's behind first, then this Mac.

## Why

- GitHub releases cost nothing, already hold the zips, and need no server of our own. Making the repo public avoids storing a token on each Mac.
- Checking the signature against the running app's own requirement means a tampered or foreign download is refused, without needing an Apple Developer ID.
- The same certificate also keeps Screen Recording and Accessibility permissions across updates.
- The host downloading the build itself (instead of the client pushing 1 MB over the control channel) keeps the protocol small and works the same whether the client is next to it or away over Tailscale.

## Consequences

- Hello messages carry `appBuild` (and the client's `purpose`). A host that reports no build is older than self-update: it has to be updated by hand once, and is never sent update messages (it would treat an update connection as a stream).
- Publishing: `scripts/release.sh --publish` (nightly) or `--release` (real release, only when the owner says). Both push the branch and the tag first.
- Test hooks: `BB1_UPDATE_FEED` (a local releases JSON) and `BB1_TEST_AUTO_UPDATE`.
