# Idea: bb1 for iPhone

**Status:** idea, not started (2026-10-20)

## The point

Glance at a Mac from your phone, and step in when it needs you. The main case: an agent is working on the Mac (for example through the [MCP computer-use idea](mcp-computer-use.md)) and reaches a password field. Instead of opening a laptop, you get a notification on your phone, type the password, press Enter, and the agent carries on.

## What it does

- **See the screen:** the iPhone version of the bb1 client, using the same protocol, sign-in and encryption.
- **Touch as a trackpad:** drag to move the cursor, tap to click, two fingers to scroll, pinch to zoom in on part of the screen.
- **A keyboard:** the iPhone keyboard sends text to the Mac. There are no Cmd shortcuts, so the app needs a small row of extra keys (Return, Esc, Tab, arrows, Cmd).
- **"You're needed" alerts:** the host notices when a text field or **password field** gets focus and sends a notification, e.g. "Password needed on Siphesihle's MacBook Air". Tapping it opens the app straight into a text box. You type, press Enter, and the text goes to that field.

## How "you're needed" works

1. The host already has Accessibility permission. It asks macOS which element has keyboard focus (`AXFocusedUIElement`) and what kind it is: `AXTextField`, `AXTextArea`, or `AXSecureTextField` for passwords.
2. When a **password field** gets focus, or a text field does while an agent is in control, the host sends an alert. Each field triggers it once (not on every keystroke), and alerts are rate-limited.
3. The phone sends the typed text over the encrypted control channel, as keystrokes into the focused field.
4. **Passwords are never logged or stored.** The log records only "secure text entered (N characters)". The phone's text box is a secure field: no autocorrect, no keyboard history.

## Network

- **At home:** the phone finds the Mac through Bonjour, like the Mac client does.
- **Away:** through the Tailscale iPhone app. Tailscale sign-in works exactly as it does for Macs: it's your account, so you're let in.
- **First time:** pair with the 6-digit code, or sign in with Tailscale. After that it's remembered.

## Code to reuse

The iPhone app can reuse almost everything in `BB1Core`, which uses no Mac-only APIs except the clipboard: the protocol, encryption, sign-in, packet resending and reassembly. The app adds:

| Part | Mac client today | iPhone |
|---|---|---|
| Decode | VideoToolbox | same |
| Display | `AVSampleBufferDisplayLayer` | same |
| Sound | AVAudioEngine jitter buffer | same, plus an iOS audio session |
| Input | NSEvent, event tap | touch gestures, keyboard, extra-keys row |
| Clipboard | NSPasteboard | UIPasteboard (paste only when you tap) |
| UI | AppKit | SwiftUI |

Host-side additions: watching keyboard focus, the alert message, and "type this text" (a message carrying a whole string, instead of a key at a time).

## What it costs

- **Full Xcode.** iPhone apps can't be built with the Command Line Tools alone. This would be the first part of bb1 that needs Xcode.
- **Apple Developer Program** (you have it): needed to install on your phone and for push notifications.
- **Push notifications need a small relay.** Apple delivers pushes only from a server holding the push key, so the Mac can't send them to the phone directly. A tiny Cloudflare Worker would do: the host sends "notify device X: password needed", and the Worker forwards it to Apple. It's free tier and stateless, apart from the push key.
  - **Without the relay,** alerts only work while the app is open, or as Live Activities started from inside the app.
- **Getting it onto your phone:** run it from Xcode, or use TestFlight.

## Effort

| Piece | Rough size |
|---|---|
| iPhone viewer (video, sound, touch, keyboard) | 2–3 days |
| Host: focus watching + "type this text" | ½ day |
| Push relay (Cloudflare Worker) + notification handling | 1 day |
| Polish (extra-keys row, zoom, reconnects on mobile networks) | 1–2 days |

Suggested order: build the [MCP computer-use tool](mcp-computer-use.md) first (smaller, and it creates the "agent needs your password" moment), then the viewer, then alerts.

## Open questions

- Alert only for password fields, or for any text field while an agent is driving?
- Should the phone be able to approve or deny agent actions too ("The agent wants to click Buy, allow?")?
- iPad as well? It's nearly free once the iPhone app exists, and an iPad with a keyboard works well as a full client.
