# Testing bb1: M1 host → M4 client

## What this first version does

- Streams the host's main display (the Dell, 1920×1080) at full resolution, 60 fps, HEVC in Apple's low-latency encoder mode, 30 Mbps.
- Mouse, clicks, scrolling (with trackpad momentum) and keyboard, including Cmd shortcuts.
- Fixes stalls by itself: if capture, encoder, decoder, video packets or the connection get stuck, it recovers without you reconnecting ([docs/04-reliability.md](docs/04-reliability.md)).
- Encrypted, and only devices with the pairing key can connect.
- Finds the host on your home network automatically.

## What's been tested, and what hasn't

Tested on the M1, with host and client on the same machine:

- Real screen capture → encode → network → decode → window. About 3 ms encode and 2–3 ms decode per frame, ~7–11 ms from capture to the frame being handed to the display.
- Every recovery path, broken on purpose with `scripts/recovery-test.sh`: capture dying, the encoder stalling silently, the host freezing for 3 s, the host crashing, 3% packet loss. Each one recovered without a manual reconnect.

**Not tested yet:**

- The real M1 → M4 path over Wi-Fi.
- Mouse/keyboard control. The test session didn't have Accessibility permission, so input injection couldn't run.
- Cmd+Tab capture on the client.
- The installed login item.

That's what this round of testing is for.

---

## 1. The host (the M1)

Already installed: **bb1.app** in `~/Applications`, in **Host** mode, started at login and restarted if it crashes. Screen Recording and Accessibility are allowed.

To reinstall after code changes:

```sh
cd ~/Documents/code/bb1
scripts/install-host.sh
```

Don't double-click the app inside `build/`: macOS blocks signed apps launched from inside `~/Documents`.

## 2. The client (the M4)

1. The latest `bb1.zip` is sent over Tailscale and lands in **Downloads**. Unzip it.
2. Quit any old **BB1 Client**, delete it from Applications, and drag **bb1.app** in.
3. In Terminal: `xattr -dr com.apple.quarantine /Applications/bb1.app` (it isn't notarized yet).
4. Open bb1. The **Computers** page lists your Macs. There's no mode to pick: any Mac can connect, and **Share This Mac** turns on sharing.

## 3. Connect

The dashboard's Client tab lists **Your Macs**: hosts on your home network, Macs on your Tailscale account that are running bb1, and Macs you've paired with before. Press **Connect**.

How each Mac signs in, without a key:

- **On your Tailscale account:** the host asks Tailscale who the device belongs to. If it's your account, it's let in. Works at home and away (school, etc.).
- **Not on Tailscale (first time only):** the button says **Pair…**. A 6-digit code appears on the host's screen; type it on the client. You do this once.
- **Every time after that:** the two Macs remember each other, so it's just Connect, even on the home network without Tailscale.
- Your old pairing key still works once and is turned into a remembered pairing automatically.

On the host's dashboard (Host tab) you can see paired Macs, forget them, and turn Tailscale sign-in or code pairing off.

**Testing Tailscale lag at home:** when a Mac is reachable both ways, its row has a **Home network / Tailscale** picker. Pick Tailscale, connect, and press ⌃⌥⇧S: the stats show *Tailscale · direct* (fast path) or *Tailscale · relayed* (through Tailscale's servers, slower). Away from home, the M1 shows up the same way, over Tailscale.

Optional: System Settings → Privacy & Security → **Accessibility** → turn on **bb1** on the M4, so Cmd+Tab and Cmd+Space go to the M1.

## The dashboard and menu bar

**Computers** (laid out like Parsec): a card for **This Mac** (Share This Mac / Stop Sharing) and one per Mac you can reach, each with **Connect** (or **Pair** the first time). A Mac reachable both ways has a **Home / Tailscale** switch on its card. Your Tailscale account is shown top right; **Connect by address** is at the bottom.

**Settings:** Share this Mac, Sign in with Tailscale, Pair with a code, Paired Macs (Forget All), permissions and addresses (while sharing), Dock icon, Keep running in the menu bar, Open at login, logs.

**Menu bar icon** (the bb1 mark, dimmed while idle, ⚠️ if sharing needs a permission): Open bb1, status, Connect / Disconnect / Stream ▸, Share This Mac, Settings…, Quit bb1.

## Clipboard

Copy on either Mac, paste on the other. Text and images. Copies that password managers mark as private aren't sent. The log records only the size and kind of each copy, never the contents.

## Logs

Everything ends up on the sharing Mac: `~/Library/Logs/bb1/host.log` on the M1, including the client's lines (prefixed with its name), a stats line per second from each side, `STALL` lines when the picture pauses while the host is still sending, `HITCH` lines when the host Mac itself freezes, and memory-pressure warnings. Input is logged as counts only, never what was typed.

## 4. Stream options

In the **Stream** menu (top menu bar while bb1 is in front), in the bb1 icon's menu → Stream Options, or with these keys. The keys always stay on the M4 and are never sent to the host:

| Keys | Does |
|---|---|
| ⌃⌥⇧F | Fullscreen on/off |
| — | Fill Screen (windowed): the window covers the screen, again to go back |
| — | Scaling: Fit (black bars), Stretch to Fill, Actual Pixels (1:1) |
| — | Input: Keyboard & Mouse, Keyboard Only, Mouse Only, View Only |
| ⌃⌥⇧R | Release input: keyboard and mouse go back to the M4 (click the picture to take control again) |
| ⌃⌥⇧M | Mute / unmute the host's sound |
| ⌃⌥⇧S | Show/hide stats |
| ⌃⌥⇧C | This Mac's cursor (default: instant, and it takes the host's shape: arrow, pointing hand, I-beam…) vs the host's cursor drawn in the video |
| ⌃⌥⇧I | Flip scroll direction |
| ⌃⌥⇧D | Disconnect (bb1 stays in the menu bar) |

Input goes to the host only while the stream window is focused; your own cursor shows over the picture whenever bb1 isn't driving the host's mouse. Choices are remembered.

## Sound

The host's sound plays on the client: 48 kHz stereo, uncompressed (~1.5 Mbps), with an adaptive buffer (40 ms, growing up to 150 ms on a choppy network and shrinking back when it settles). Each packet is sent twice, 10 ms apart, so one lost packet doesn't cause a gap. Muting also stops the host from sending it. The host's own speakers keep playing.

## 5. What to try

1. **Feel:** use the M1 normally for a while: click around, type, scroll. Compare it with Parsec side by side if you can.
2. **The Parsec freeze test:** leave a session open for an hour or more and use it now and then. Check the **recoveries** count in the stats (⌃⌥⇧S). If it goes up, bb1 fixed something by itself, and the log says what.
3. **Sharpness:** look at small text. After you stop scrolling or typing, bb1 re-sends the still frame at higher quality, so text should sharpen within about half a second.
4. **Network drop:** turn the M4's Wi-Fi off for 5 seconds, then on. It should come back by itself.
5. **Break it on purpose:** host menu bar icon → Test Recovery → pick a fault. The picture should come back within about a second.

## 6. Tell me how it went

Most useful:

- A screenshot of the stats line (⌃⌥⇧S) while you're using it.
- Anything that felt laggy, blurry or wrong, and roughly when.
- The logs:
  - host (M1): `~/Library/Logs/bb1/host.log`, also in the menu: Open Log
  - client (M4): `~/Library/Logs/bb1/client.log`

## Known limitations of this version

- Caps Lock isn't forwarded.
- The host is 1080p. On the M4's Retina screen it's scaled up, so it won't be as sharp as a native Retina display. Parsec shows the same pixels, so compare like with like.
- Away from home: typing the M1's Tailscale IP into the Host box should work but hasn't been tried.

## For development

```sh
swift build                        # debug build, fast
swift run bb1-selftest             # protocol/crypto checks
scripts/recovery-test.sh           # break things on purpose, check recovery (test pattern)
scripts/recovery-test.sh --screen  # same, with real screen capture
swift run bb1 host --help          # host options
swift run bb1 client --help        # client options
```
