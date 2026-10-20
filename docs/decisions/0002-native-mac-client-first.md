# 0002 — Native Mac client first, browser later or never

**Status:** accepted

## Context

The main client is a Mac. Windows comes later. A browser client is a nice-to-have. Target is Parsec-level latency.

## Decision

Build a native Swift Mac client first. The browser client is not planned for v1.

## Why

A browser client is worse on the three things Parsec does well:

1. **Keyboard:** a browser tab can't capture Cmd+Tab, Cmd+Space, Cmd+Q and other system shortcuts. A native app with an event tap can.
2. **Transport:** browsers can't send raw UDP. WebSockets are TCP (a lost packet stalls everything behind it); WebRTC adds its own buffering; WebTransport is the best option but less proven in Safari.
3. **Display:** drawing to a canvas follows the browser's own frame timing, usually about one extra frame of delay.

A native client uses hardware decode and shows frames immediately.

## Consequences

- Windows needs its own client, built against [`02-protocol`](../02-protocol.md).
- The protocol stays plain and documented, so a browser bridge can be added later without changing the host's core.
