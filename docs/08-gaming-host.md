# Gaming: a Windows host for playing together

**Status:** design, 2026-11-05. Step 1 in progress.

The owner's gaming PC (Windows, NVIDIA) hosts; friends join from their own Windows PCs over Tailscale and play with controllers, Parsec-style. Friends' PCs can be weak, so the client must stay light. Everything here builds on the protocol the Mac host and both clients already speak (`docs/02-protocol.md`), so the existing Windows and Mac clients can join a Windows host from day one.

## What went wrong with Parsec, and why

Two problems, both seen for months:

### 1. A second friend joins: resolution drops and everyone lags

**Most likely cause: one encoder for everybody.** Parsec encodes the host's screen once and sends the same stream to every guest, so the bitrate (and with it, resolution) has to suit the *weakest* guest. When a friend on a worse connection joins, the stream drops for everyone. On top of that the host's **upload** carries one copy per guest: two guests at 40 Mbps is 80 Mbps up, more than many home lines in South Africa have, and once the upload queue fills every guest sees lag at the same moment.

**bb1's answer:**
- **One encoder per viewer.** NVIDIA GPUs have a dedicated encoder block (NVENC) that runs beside the 3D engine and handles several 1080p60 sessions at once (consumer cards allow 8 sessions). Each friend gets their own resolution, frame rate and bitrate; one bad connection affects only that friend.
- **An upload budget.** The host measures how much its line can actually send and splits it between viewers, instead of each viewer's stream growing until the line chokes.

### 2. Launching a GPU-heavy game (FC 26): one friend's connection "freaks out"

Several things happen at once when the GPU hits 100%:

| Cause | Effect | Fix in bb1 |
|---|---|---|
| The game takes all the GPU's 3D time; screen capture and the colour conversion before encoding wait behind it | Frames come out late and in clumps, so the friend's picture stutters and the network sees bursts | Run bb1's GPU work at the highest GPU priority (Windows lets a process ask for it; Sunshine does the same). Cap the game's frame rate a little below the monitor's (NVIDIA Control Panel) so the GPU is never pinned at 100% |
| A fast, detailed game (grass, crowds, camera pans) needs far more bits than a desktop; the encoder fills its bitrate and **keyframes** get 5–10× bigger than normal frames | Bursts larger than the friend's line can absorb: his router's queue fills ("bufferbloat"), so not only the stream but *everything* on his internet lags, then packets drop | **Congestion control** that watches delay, not just loss: when packets start arriving later than they were sent, the queue is filling, so lower the bitrate *before* anything drops. **Pacing**: spread each frame's packets across the frame interval instead of sending them in one burst. **No big keyframes**: intra-refresh (a moving stripe of fresh picture) instead of whole keyframes |
| Possibly: the stream goes through a relay instead of directly | Extra delay and a bandwidth ceiling | bb1 shows "Tailscale · direct" or "relayed via jnb" in the stream window, and the host logs it |

The friend's PC being weak probably doesn't matter for this one: decoding happens on the graphics card in bb1's client, and 1080p60 H.264 decoding is easy for any GPU from the last decade.

## Design

### Host (Rust, Windows) — new, in `apps/windows`

Same crate as the Windows client (shares `src/core`: protocol, crypto, video framing). Pipeline per viewer, GPU memory end to end, no copies through the CPU:

```
Desktop Duplication (DXGI) ──► D3D11 texture ──► NVENC session per viewer ──► seal + packetize ──► paced UDP
        (one capture)              (shared)          (own size/fps/bitrate)        (per viewer)      (per viewer)
```

| Part | How | Why |
|---|---|---|
| Capture | DXGI Desktop Duplication | Works with fullscreen games, gives a GPU texture, lowest latency on Windows. Windows.Graphics.Capture as a fallback for some multi-GPU laptops |
| Scaling | D3D11 video processor (on the GPU) | A friend on a small line gets 720p without the host's screen changing |
| Encode | **NVENC directly** (`nvEncodeAPI64.dll`, ships with the NVIDIA driver; header is MIT-licensed) | Lowest latency (no extra frame of buffering), intra-refresh, bitrate changes without a keyframe, per-frame control. Media Foundation's NVIDIA encoder hides most of this. AMD/Intel later through Media Foundation |
| Codec | H.264 by default; HEVC when the viewer's PC can decode it (about 30% fewer bits for the same picture, which helps a friend on a slow line) | Every Windows PC decodes H.264 for free; HEVC needs the HEVC Video Extensions |
| Sound | WASAPI loopback (what the PC plays), sent like the Mac host's sound | Same jitter buffer on the client |
| GPU priority | `IDXGIDevice::SetGPUThreadPriority(7)` for the capture/encode device, and the process's GPU scheduling class raised when bb1 runs as admin | Capture and encode don't wait behind the game |
| Keyboard, mouse | `SendInput`. Key codes arrive as Mac key codes (both clients send those), mapped back with the inverse of `keymap.rs`. **Relative mouse** for games (raw mouse deltas from the client while the pointer is captured) | Games that use mouse-look need relative movement, not positions |
| Controllers | A virtual controller per viewer through **ViGEmBus** (Xbox 360 or DualShock 4 to the game). Rumble goes back to the viewer | Each friend is a separate player in the game |

### Network: per viewer

- **Congestion control:** each viewer reports arrival times; the host compares send and arrival spacing (the idea behind WebRTC's delay-based control). Rising delay → lower the bitrate quickly; steady → creep up. Loss alone is too late, since by then the friend's router queue is already full.
- **Pacing:** packets of a frame are spread over about 80% of a frame interval, at a rate a bit above the target bitrate.
- **Loss recovery:** resend on NACK (what bb1 does today) when the round trip is short; intra-refresh means a lost frame heals within a few frames without a keyframe. Forward error correction is a later option for lossy Wi-Fi.
- **Upload budget:** the sum of viewers' bitrates stays under what the host's line delivered recently (measured from the viewers' reports).
- Over Tailscale: packets stay ≤ 1200 bytes (Tailscale's MTU is 1280).

### Client (Windows) — the existing app, kept light

Already decodes on the GPU (Media Foundation + Direct3D 11) and draws with a flip-model swap chain; CPU use is small. Additions:
- **Controllers:** read with SDL3 (DualSense, DualShock 4, Xbox; DualShock 3 with the DsHidMini driver installed), send state at up to 250 Hz over UDP (newest wins), rumble back.
- **Relative mouse** while the pointer is captured (raw input).
- Arrival timestamps in its reports, for the host's congestion control.

### Sound: leave chosen apps out

So a friend doesn't hear Discord (or a call) twice, once directly and once through the stream, the host can leave apps out of the sound it shares. **The list:** Discord, Chrome and Helium. Each is shown only when it's installed (detected by its process name or install folder), with its icon. Browsers can only be left out whole: every tab's sound comes from one browser process, so "only Google Meet" isn't possible; use one browser for calls and leave that one out.

- **Windows:** WASAPI process loopback. With nothing left out, one loopback of everything (as now). With apps left out, capture each other app that's playing sound on its own (include mode, per process tree) and mix them; Windows can exclude only one process tree per capture, so mixing is how several are left out.
- **Mac:** ScreenCaptureKit's content filter leaves the chosen apps' sound out directly.
- **Picker:** a dropdown in the host window; its button shows the left-out apps' icons slightly overlapping. Clicking an app in the open list toggles it (several can be on); clicking away or on the button closes it. Remembered per host.

## Steps

Each step ships on its own and is measured against Parsec on the same games.

1. **Windows host, screen and sound.** Capture, NVENC (H.264), one encoder per viewer, pacing, GPU priority, sound, keyboard and mouse. The existing clients (Windows and Mac) connect to it like to a Mac. *Done when:* a friend plays FC 26 with the owner's keyboard mouse and the picture holds at 1080p60 with the game at full GPU load.
2. **Controllers.** SDL3 on the client, ViGEmBus on the host, a player per viewer, rumble. *Done when:* two friends play FC 26 against each other with their own controllers.
3. **Congestion control and upload budget.** Delay-based bitrate per viewer, upload budget, intra-refresh. *Done when:* a second friend joining doesn't change the first friend's picture, and a friend on a slow line doesn't lag his own internet.
4. **Polish.** Leaving apps out of the shared sound (above), HEVC where supported, relative mouse, per-viewer resolution, a "Share this PC" switch in the Windows app, and what each viewer is allowed to do (controller only, or keyboard and mouse too).

## Open questions

- ViGEmBus is no longer maintained (still installs and works on Windows 11). Alternatives to watch: a newer virtual gamepad driver, or Windows' own `GameInput` virtual devices if Microsoft opens them up.
- Desktop Duplication can't capture some protected content and stops during UAC prompts and the lock screen (the Mac host has the same limits on its lock screen).
- Measure the owner's real upload speed and each friend's download speed to set starting bitrates.
