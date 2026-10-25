//! Plays the Mac's sound through the default output device (WASAPI, shared mode). Windows converts
//! the 48 kHz stereo 16-bit stream to whatever the device runs at. Starts over on a new device
//! (headphones plugged in) by itself.

use crate::core::client::Client;
use crate::logi;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub struct AudioOut {
    stop: Arc<AtomicBool>,
}

impl AudioOut {
    pub fn start(client: Client) -> AudioOut {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::Builder::new()
            .name("bb1 sound".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let mut failures = 0;
                while !s.load(Relaxed) {
                    match play(&client, &s) {
                        Ok(()) => failures = 0,
                        Err(e) => {
                            failures += 1;
                            if failures <= 3 {
                                logi!("sound: playback stopped ({e}); starting again");
                            }
                            std::thread::sleep(std::time::Duration::from_millis(if failures > 3 { 2000 } else { 300 }));
                        }
                    }
                }
            })
            .unwrap();
        AudioOut { stop }
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

/// Plays until stopped or the device goes away.
fn play(client: &Client, stop: &AtomicBool) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let audio: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // 20 ms of buffer: the jitter buffer in front of it does the real cushioning.
        audio.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            200_000,
            0,
            &format,
            None,
        )?;
        let event = CreateEventW(None, false, false, None)?;
        audio.SetEventHandle(event)?;
        let render: IAudioRenderClient = audio.GetService()?;
        let frames = audio.GetBufferSize()?;
        audio.Start()?;
        logi!("sound: playing ({frames} frame device buffer)");
        let mut scratch = vec![0i16; frames as usize * 2];
        let result = loop {
            if stop.load(Relaxed) {
                break Ok(());
            }
            if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                continue;
            }
            let padding = match audio.GetCurrentPadding() {
                Ok(p) => p,
                Err(e) => break Err(e),
            };
            let free = frames - padding;
            if free == 0 {
                continue;
            }
            let data = match render.GetBuffer(free) {
                Ok(d) => d,
                Err(e) => break Err(e),
            };
            let out = &mut scratch[..free as usize * 2];
            client.audio().read(out);
            std::ptr::copy_nonoverlapping(out.as_ptr() as *const u8, data, out.len() * 2);
            if let Err(e) = render.ReleaseBuffer(free, 0) {
                break Err(e);
            }
        };
        let _ = audio.Stop();
        let _ = CloseHandle(event);
        result
    }
}
