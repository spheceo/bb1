# bb1 for Windows

Connect to a Mac running bb1 from a Windows 10 or 11 PC. In testing.

## Use it

1. Run `bb1-setup.exe`. It isn't signed, so Windows says "Windows protected your PC": click **More info → Run anyway**, then **Yes** to let it make changes. It installs to `C:\Program Files\bb1`, adds a Start menu entry, and (box ticked by default) tells Microsoft Defender to leave that folder alone, so Defender doesn't delete bb1. If Windows won't allow that (another antivirus, or a work-managed PC), it says so; add the folder by hand under Windows Security → Virus & threat protection → Manage settings → Exclusions. Running a newer `bb1-setup.exe` updates in place; uninstall from Settings → Apps, which removes the exclusion too.
2. Pick the Mac and press **Connect** (or type its Tailscale IP or name). On the same Tailscale account it signs in by itself; otherwise the Mac shows a 6-digit code to type in. After that it's remembered.
3. The Mac needs a build with Windows support (nightly-14 or later): it then sends H.264, which Windows decodes on the GPU.

In the stream window:

- **Control follows the pointer.** With the window in front and the pointer on the picture (or full screen), the keyboard goes to the Mac, Windows key and Alt+Tab included. Move the pointer off the picture and the keyboard is the PC's again.
- **Ctrl+Alt+Enter**: full screen.
- **Right-click the title bar** for: full screen, which PC key is ⌘ (Windows key, Alt for Mac key positions, or Ctrl so Ctrl+C copies), sound, clipboard sharing, refresh picture, disconnect.
- The title bar shows frame rate, bitrate, round trip and how far behind the picture is.

**Updates without an admin prompt:** the installer registers the task "bb1 Update" (runs `bb1.exe --apply-update` as SYSTEM; any signed-in user may start it). bb1 writes the wanted build to `%ProgramData%\bb1\update-request.json` and starts it; the task installs only that bb1 release from GitHub, after checking its installer against the release's SHA256SUMS, and logs to `%ProgramData%\bb1\update.log`. That's how another computer can update this PC. `win/selfupdate.rs`.

**Updates:** when a newer nightly is on GitHub, Your Macs shows **Update to nightly-N** at the top right. It updates your Macs that are behind, then this PC (Windows asks once for admin), and bb1 opens again as the new build. `"nightly": false` in `%APPDATA%\bb1\settings.json` sticks to releases.

**bb1 by the clock:** bb1's icon in the notification area (maybe under the ^ arrow; drag it onto the taskbar to keep it in sight) opens Your Macs on a click. Right-click it to **share this PC**, see who's watching, disconnect them, or quit. Closing Your Macs leaves bb1 running there.

**Sharing this PC** (needs an NVIDIA graphics card): turn on **Share this PC** in the icon's menu. Macs and PCs connect to it from their Your Macs like to a Mac: by themselves on the same Tailscale account, otherwise with a 6-digit code that pops up as a notification. Each viewer has its own encoder, bitrate and pacing (docs/08-gaming-host.md). `bb1.exe --host` shares straight away, for that run only.

**Game controllers.** Plug a controller into the PC you're watching from (DualSense, DualShock 4, Xbox, Switch Pro; a DualShock 3 needs Sony's driver or [DsHidMini](https://github.com/nefarius/DsHidMini)). While the stream window is in front it plays on the host as an Xbox 360 controller, one per controller (up to four), with rumble. In the background, the host sees it centred with nothing pressed. The host's owner turns on **Controller** for you in their Sharing panel. A Windows host needs the ViGEmBus driver: Settings → Sharing → Game controllers → **Install driver** downloads its official installer (github.com/nefarius/ViGEmBus). A Mac host ignores controllers.

Logs: `%LOCALAPPDATA%\bb1\Logs\client.log` (sharing logs there too), also readable from your other devices on the same Tailscale account: `scripts/bb1-logs.sh pc client.log` (port 47802). While connected, every line also goes to the Mac's log (`~/Library/Logs/bb1/host.log`, prefixed `[windows]`). Pairings and settings: `%APPDATA%\bb1`.

## How it's built

Rust. `src/core` is the protocol, crypto and connection logic (any platform); `src/win` is the Windows app:

| Part | Windows API |
|---|---|
| Decode | Media Foundation H.264 decoder, on the GPU through Direct3D 11 (falls back to the CPU) |
| Display | Direct3D 11 flip-model swap chain, NV12 → RGB in a pixel shader, letterboxed |
| Keyboard | Low-level keyboard hook; scan codes → Mac key codes (`src/core/keymap.rs`) |
| Sound | WASAPI shared mode, behind the same adaptive jitter buffer as the Mac client |
| Clipboard | Text and images (PNG/DIB); password-manager copies are skipped |
| Controllers | SDL3 (`SDL3.dll` next to `bb1.exe`, loaded at run time; `src/win/gamepad.rs`). Host: a ViGEmBus Xbox 360 controller per viewer's pad (`vigem-client`, `src/host/input.rs`) |
| Finding Macs | `tailscale status --json`, Bonjour (`_bb1._tcp`), and remembered pairings (with each Mac's last home address). A paired Mac is reached over the home network when it's there, else Tailscale, and the connection switches between them when one stops answering |

`cargo test` checks the protocol against `protocol/vectors.json`, which the Mac app writes (`cd apps/mac && swift run bb1-selftest --vectors ../../protocol/vectors.json`), so both sides agree on every byte.

### Build on a Mac

```sh
rustup target add x86_64-pc-windows-gnu && brew install mingw-w64   # once
cargo test                                                           # core checks
cargo build --release --target x86_64-pc-windows-gnu                 # → target/x86_64-pc-windows-gnu/release/bb1.exe
brew install makensis && scripts/installer.sh                        # → …/release/bb1-setup.exe (installer, from installer/bb1.nsi)
```

`bb1-probe` is a headless test client for any platform: it signs in to a host, receives video and checks it decodes. Against a test host on the same Mac:

```sh
cd ../mac && swift build && BB1_SUPPORT_DIR=/tmp/h BB1_TEST_PAIRING_CODE_FILE=/tmp/code .build/debug/bb1 host --headless --synthetic --port 47960 &
cd ../windows && BB1_SUPPORT_DIR=/tmp/pc cargo run --bin bb1-probe -- --host 127.0.0.1:47960 --code-file /tmp/code --out /tmp/s.h264
ffmpeg -v error -i /tmp/s.h264 -f null -   # no output = every frame decoded
```

GitHub Actions builds `bb1.exe` and `bb1-setup.exe` on Windows when this folder changes (`.github/workflows/windows.yml`, artifact `bb1-windows-x64`), and for every nightly and release, which then carry `bb1-windows-nightly-<M>-setup.exe` (or `bb1-windows-v<VERSION>-setup.exe`) next to the Mac files.

The host (`src/host`, Windows only) is DXGI Desktop Duplication → NVENC (one encoder per viewer) → the protocol host in `src/core/host.rs`, which builds anywhere: `bb1-testhost --video stream.h264` runs it on a Mac with a recorded stream instead of a GPU, so both clients can be tested against it.

### Third-party files

`SDL3.dll` is SDL 3.2.30's official Windows build (`SDL3-3.2.30-win32-x64.zip` from github.com/libsdl-org/SDL/releases), under the zlib licence (free to redistribute; its `LICENSE.txt` is cached next to it in `target/sdl3-3.2.30/`). `scripts/installer.sh` and the Windows workflow download it and check its SHA-256; to update it, change the version and checksum in both. Without it bb1 runs fine, just without controllers (copying `bb1.exe` alone to a PC means no controllers until `SDL3.dll` is beside it).

## Not yet

- Signing: unsigned on purpose for now (see the "Run anyway" step above).
- Macs with a screen wider than 4096 pixels: Windows' H.264 decoder stops at 4096×2304.
