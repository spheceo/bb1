# 0001 — Host is written in Swift

**Status:** accepted

## Context

The host needs ScreenCaptureKit, VideoToolbox, CGEvent, IOKit power assertions, Bonjour and the macOS permission prompts. All are Apple frameworks with Swift/Objective-C APIs. Options considered: Swift, Go, TypeScript.

## Decision

Swift.

## Why not the others

- **Go:** every framework call goes through cgo plus Objective-C glue. Passing video frames between Go and native code adds copies and complexity in the part of the app where latency matters most.
- **TypeScript (Node/Bun):** needs native add-ons for capture, encode and input, which would be written in Swift or C++ anyway. The JS layer would sit in the middle of the frame path.

## Consequences

- One language for the host and the Mac client, sharing a `Core` module.
- Needs a signed app bundle for stable permissions. SwiftPM + `codesign` handle this without Xcode ([07](../07-building-and-running.md)).
- TypeScript stays an option for clients that aren't on a Mac (browser, possibly Windows via Electron).
