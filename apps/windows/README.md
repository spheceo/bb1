# bb1 for Windows

Connect to a Mac running bb1 from a Windows 10 or 11 PC. In testing.

## Use it

1. Run `bb1.exe`. It isn't signed, so the first time Windows says "Windows protected your PC": click **More info → Run anyway**. That's once per download.
2. Pick the Mac and press **Connect** (or type its Tailscale IP or name). On the same Tailscale account it signs in by itself; otherwise the Mac shows a 6-digit code to type in. After that it's remembered.
3. The Mac needs a build with Windows support (nightly-14 or later): it then sends H.264, which Windows decodes on the GPU.

In the stream window:

- **Control follows the pointer.** With the window in front and the pointer on the picture (or full screen), the keyboard goes to the Mac, Windows key and Alt+Tab included. Move the pointer off the picture and the keyboard is the PC's again.
- **Ctrl+Alt+Enter**: full screen.
- **Right-click the title bar** for: full screen, which PC key is ⌘ (Windows key, Alt for Mac key positions, or Ctrl so Ctrl+C copies), sound, clipboard sharing, refresh picture, disconnect.
- The title bar shows frame rate, bitrate, round trip and how far behind the picture is.

Logs: `%LOCALAPPDATA%\bb1\Logs\client.log`. While connected, every line also goes to the Mac's log (`~/Library/Logs/bb1/host.log`, prefixed `[windows]`). Pairings and settings: `%APPDATA%\bb1`.

## How it's built

Rust. `src/core` is the protocol, crypto and connection logic (any platform); `src/win` is the Windows app:

| Part | Windows API |
|---|---|
| Decode | Media Foundation H.264 decoder, on the GPU through Direct3D 11 (falls back to the CPU) |
| Display | Direct3D 11 flip-model swap chain, NV12 → RGB in a pixel shader, letterboxed |
| Keyboard | Low-level keyboard hook; scan codes → Mac key codes (`src/core/keymap.rs`) |
| Sound | WASAPI shared mode, behind the same adaptive jitter buffer as the Mac client |
| Clipboard | Text and images (PNG/DIB); password-manager copies are skipped |
| Finding Macs | `tailscale status --json`, Bonjour (`_bb1._tcp`), and remembered pairings |

`cargo test` checks the protocol against `protocol/vectors.json`, which the Mac app writes (`cd apps/mac && swift run bb1-selftest --vectors ../../protocol/vectors.json`), so both sides agree on every byte.

### Build on a Mac

```sh
rustup target add x86_64-pc-windows-gnu && brew install mingw-w64   # once
cargo test                                                           # core checks
cargo build --release --target x86_64-pc-windows-gnu                 # → target/x86_64-pc-windows-gnu/release/bb1.exe
```

`bb1-probe` is a headless test client for any platform: it signs in to a host, receives video and checks it decodes. Against a test host on the same Mac:

```sh
cd ../mac && swift build && BB1_SUPPORT_DIR=/tmp/h BB1_TEST_PAIRING_CODE_FILE=/tmp/code .build/debug/bb1 host --headless --synthetic --port 47960 &
cd ../windows && BB1_SUPPORT_DIR=/tmp/pc cargo run --bin bb1-probe -- --host 127.0.0.1:47960 --code-file /tmp/code --out /tmp/s.h264
ffmpeg -v error -i /tmp/s.h264 -f null -   # no output = every frame decoded
```

GitHub Actions builds `bb1.exe` on Windows when this folder changes (`.github/workflows/windows.yml`, artifact `bb1-windows-x64`).

## Not yet

- Self-update (copy a new `bb1.exe` over the old one for now).
- Signing: unsigned on purpose for now (see the "Run anyway" step above).
- Macs with a screen wider than 4096 pixels: Windows' H.264 decoder stops at 4096×2304.
