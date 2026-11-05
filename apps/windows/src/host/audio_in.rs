//! What this PC plays (WASAPI loopback of the default output device), as 48 kHz stereo 16-bit PCM
//! in 5 ms chunks: the format the Mac host sends, so clients play it unchanged.
//!
//! `Loopback::start(on_chunk)` opens the default render device (eRender, eConsole) in shared mode
//! with AUDCLNT_STREAMFLAGS_LOOPBACK on its own thread (COM multithreaded) and calls `on_chunk` with
//! exactly 960 bytes (240 frames × 2 channels × i16 LE) and the capture time of the chunk's first
//! sample on `crate::core::now_us()`'s clock (from the packet's QPC timestamp).
//!
//! - Format: whatever the device mixes in (usually 32-bit float, 48 kHz, 2 or more channels; 16/24/
//!   32-bit integer too). More than two channels are mixed down to stereo (centre and surrounds at
//!   −3 dB, LFE dropped, scaled so a full-scale mix can't clip); other rates go through a linear
//!   resampler.
//! - Polling, not events: event-driven loopback doesn't signal on older Windows. A high-resolution
//!   waitable timer wakes the thread every 2.5 ms.
//! - Silence: when nothing plays, Windows delivers no packets at all. After 30 ms without any, the
//!   thread sends silent chunks in real time, so the client's jitter buffer doesn't run dry and grow.
//! - Devices: if the device goes away (AUDCLNT_E_DEVICE_INVALIDATED) or the default output changes
//!   (checked every second), the new default is opened.
//!
//! `start` fails only if no output device can be opened at all; after that it keeps trying by
//! itself. Dropping the `Loopback` stops and joins the thread.

use crate::core::now_us;
use crate::logi;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, GetCurrentThread, SetThreadPriority, SetWaitableTimer,
    THREAD_PRIORITY_HIGHEST, TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::core::PCWSTR;

const OUT_RATE: u32 = 48_000;
const CHUNK_FRAMES: usize = 240;
const CHUNK_SAMPLES: usize = CHUNK_FRAMES * 2;
const CHUNK_BYTES: usize = CHUNK_SAMPLES * 2;
const CHUNK_US: u64 = CHUNK_FRAMES as u64 * 1_000_000 / OUT_RATE as u64;
/// No packets for this long = nothing is playing: start sending silence.
const IDLE_AFTER: Duration = Duration::from_millis(30);
const POLL_HNS: i64 = 25_000; // 2.5 ms in 100 ns units

type OnChunk = Box<dyn FnMut(&[u8], u64) + Send>;

pub struct Loopback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    pub fn start(on_chunk: impl FnMut(&[u8], u64) + Send + 'static) -> Result<Loopback, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let s = stop.clone();
        let on_chunk: OnChunk = Box::new(on_chunk);
        let thread = std::thread::Builder::new()
            .name("bb1 loopback".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                unsafe {
                    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
                }
                run(Chunker::new(on_chunk), &s, ready_tx);
                if com {
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|e| format!("sound capture thread: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Loopback { stop, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // Hung opening the device (or the thread died): don't wait for it.
                stop.store(true, Relaxed);
                Err("sound capture didn't start within 10 s".into())
            }
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Opens the default device and captures until stopped, opening it again whenever it goes away.
fn run(mut sink: Chunker<OnChunk>, stop: &AtomicBool, ready: mpsc::Sender<Result<(), String>>) {
    let mut ready = Some(ready);
    let mut failures = 0u32;
    let timer = Timer::new();
    while !stop.load(Relaxed) {
        let mut session = match unsafe { Session::open() } {
            Ok(s) => s,
            Err(e) => {
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Err(e));
                    return;
                }
                failures += 1;
                if failures <= 3 {
                    logi!("sound capture: {e}; trying again");
                }
                sleep_unless(stop, Duration::from_millis(if failures > 3 { 2000 } else { 300 }));
                continue;
            }
        };
        if let Some(tx) = ready.take() {
            let _ = tx.send(Ok(()));
        }
        let opened = Instant::now();
        match unsafe { session.capture(&mut sink, stop, &timer) } {
            Ok(()) => {}
            Err(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED => logi!("sound capture: the output device went away; opening the new default"),
            Err(e) => logi!("sound capture stopped ({e}); starting again"),
        }
        drop(session);
        sink.flush_padded();
        if opened.elapsed() > Duration::from_secs(5) {
            failures = 0;
        } else {
            // Failing right after opening: don't spin.
            failures += 1;
            sleep_unless(stop, Duration::from_millis(if failures > 3 { 2000 } else { 100 }));
        }
    }
}

fn sleep_unless(stop: &AtomicBool, total: Duration) {
    let until = Instant::now() + total;
    while !stop.load(Relaxed) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A periodic wake-up, 2.5 ms when Windows has high-resolution timers (Windows 10 1803+).
struct Timer(Option<HANDLE>);

impl Timer {
    fn new() -> Timer {
        unsafe {
            match CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0) {
                Ok(h) => Timer(Some(h)),
                Err(e) => {
                    logi!("sound capture: no high-resolution timer ({e}); polling at the system tick");
                    Timer(CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0).ok())
                }
            }
        }
    }

    fn wait(&self) {
        match self.0 {
            Some(h) => unsafe {
                let due = -POLL_HNS;
                if SetWaitableTimer(h, &due, 0, None, None, false).is_ok() {
                    WaitForSingleObject(h, 50);
                } else {
                    std::thread::sleep(Duration::from_millis(2));
                }
            },
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

struct Session {
    enumerator: IMMDeviceEnumerator,
    audio: IAudioClient,
    capture: IAudioCaptureClient,
    id: String,
    format: Format,
    weights: Vec<(f32, f32)>,
    resampler: Option<Resampler>,
}

impl Session {
    unsafe fn open() -> Result<Session, String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("device list: {e}"))?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| format!("no sound output device ({e})"))?;
            let id = device_id(&device);
            let audio: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| format!("output device: {e}"))?;
            let mix = audio.GetMixFormat().map_err(|e| format!("output format: {e}"))?;
            let format = read_format(mix);
            // 1 s of buffer: we read every few ms, this only matters if the thread is starved.
            let init = audio.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 10_000_000, 0, mix, None);
            CoTaskMemFree(Some(mix as _));
            let format = format?;
            init.map_err(|e| format!("loopback: {e}"))?;
            let capture: IAudioCaptureClient = audio.GetService().map_err(|e| format!("loopback: {e}"))?;
            audio.Start().map_err(|e| format!("loopback start: {e}"))?;
            logi!(
                "sound capture: {} Hz, {} channels, {:?}{}",
                format.rate,
                format.channels,
                format.kind,
                if format.rate != OUT_RATE { " (resampled to 48 kHz)" } else { "" }
            );
            let weights = downmix_weights(format.channels, format.mask);
            let resampler = (format.rate != OUT_RATE).then(|| Resampler::new(format.rate, OUT_RATE));
            Ok(Session { enumerator, audio, capture, id, format, weights, resampler })
        }
    }

    /// Reads packets until stopped, the device fails, or the default device changes (Ok).
    unsafe fn capture(&mut self, sink: &mut Chunker<OnChunk>, stop: &AtomicBool, timer: &Timer) -> windows::core::Result<()> {
        let mut last_data = Instant::now();
        let mut last_check = Instant::now();
        let mut silence_next: Option<u64> = None;
        let mut decoded: Vec<[f32; 2]> = Vec::new();
        let mut resampled: Vec<[f32; 2]> = Vec::new();
        let qpc_freq = qpc_frequency();
        loop {
            if stop.load(Relaxed) {
                return Ok(());
            }
            if last_check.elapsed() > Duration::from_secs(1) {
                last_check = Instant::now();
                let now_default = unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map(|d| device_id(&d)).unwrap_or_default();
                if now_default != self.id {
                    logi!("sound capture: the default output device changed");
                    return Ok(());
                }
            }
            loop {
                let size = unsafe { self.capture.GetNextPacketSize()? };
                if size == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))? };
                let now = now_us();
                let packet_us = frames as u64 * 1_000_000 / self.format.rate as u64;
                // The QPC position is when the first frame was captured, in 100 ns units.
                let age_us = match qpc_now_hns(qpc_freq) {
                    Some(t) if qpc != 0 && flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && t >= qpc => ((t - qpc) / 10).min(1_000_000),
                    _ => packet_us,
                };
                decoded.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    decoded.resize(frames as usize, [0.0; 2]);
                } else {
                    let bytes = unsafe { std::slice::from_raw_parts(data, frames as usize * self.format.block_align) };
                    decode(&self.format, &self.weights, bytes, &mut decoded);
                }
                unsafe { self.capture.ReleaseBuffer(frames)? };
                let out = match &mut self.resampler {
                    Some(r) => {
                        resampled.clear();
                        r.process(&decoded, &mut resampled);
                        &resampled
                    }
                    None => &decoded,
                };
                silence_next = None;
                sink.push(out, now.saturating_sub(age_us));
                last_data = Instant::now();
            }
            if last_data.elapsed() > IDLE_AFTER {
                let now = now_us();
                let next = silence_next.get_or_insert_with(|| {
                    // Finish what was playing, then cover the gap since the last packet.
                    sink.flush_padded();
                    sink.end_us().unwrap_or(0).max(now.saturating_sub(IDLE_AFTER.as_micros() as u64 + 10_000))
                });
                while *next + CHUNK_US <= now {
                    sink.silence(*next);
                    *next += CHUNK_US;
                }
            }
            timer.wait();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = self.audio.Stop();
        }
    }
}

fn device_id(device: &IMMDevice) -> String {
    unsafe {
        match device.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

fn qpc_frequency() -> i64 {
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    f
}

/// QueryPerformanceCounter in 100 ns units (what IAudioCaptureClient's QPC position uses).
fn qpc_now_hns(freq: i64) -> Option<u64> {
    let mut c = 0i64;
    if freq <= 0 || unsafe { QueryPerformanceCounter(&mut c) }.is_err() || c < 0 {
        return None;
    }
    Some((c as u128 * 10_000_000 / freq as u128) as u64)
}

unsafe fn read_format(p: *const WAVEFORMATEX) -> Result<Format, String> {
    unsafe {
        let wf = std::ptr::read_unaligned(p);
        let (mut tag, mut mask) = (wf.wFormatTag as u32, 0u32);
        if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22 {
            let ext = std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE);
            mask = ext.dwChannelMask;
            let sub = ext.SubFormat;
            // KSDATAFORMAT_SUBTYPE_xxx = {0000000x-0000-0010-8000-00AA00389B71}
            if sub.data2 == 0 && sub.data3 == 0x10 && sub.data4 == [0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71] {
                tag = sub.data1;
            } else {
                tag = 0;
            }
        }
        Format::new(tag, wf.wBitsPerSample, wf.nChannels as usize, wf.nSamplesPerSec, wf.nBlockAlign as usize, mask)
    }
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const TAG_PCM: u32 = 1;
const TAG_FLOAT: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleKind {
    F32,
    I16,
    I24,
    /// 32-bit integer (also 24 valid bits in a 32-bit container: they're left-justified).
    I32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Format {
    pub kind: SampleKind,
    pub channels: usize,
    pub rate: u32,
    pub block_align: usize,
    pub mask: u32,
}

impl Format {
    pub(crate) fn new(tag: u32, bits: u16, channels: usize, rate: u32, block_align: usize, mask: u32) -> Result<Format, String> {
        let kind = match (tag, bits) {
            (TAG_FLOAT, 32) => SampleKind::F32,
            (TAG_PCM, 16) => SampleKind::I16,
            (TAG_PCM, 24) => SampleKind::I24,
            (TAG_PCM, 32) => SampleKind::I32,
            _ => return Err(format!("output device format not supported (tag {tag}, {bits} bit)")),
        };
        let bytes = bits as usize / 8;
        if channels == 0 || !(8_000..=768_000).contains(&rate) || block_align < channels * bytes {
            return Err(format!("output device format not supported ({channels} channels, {rate} Hz, block {block_align})"));
        }
        Ok(Format { kind, channels, rate, block_align, mask })
    }

    fn sample_bytes(&self) -> usize {
        match self.kind {
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::F32 | SampleKind::I32 => 4,
        }
    }
}

/// (left, right) gain for each channel, for a mix down to stereo.
pub(crate) fn downmix_weights(channels: usize, mask: u32) -> Vec<(f32, f32)> {
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    if channels == 1 {
        return vec![(1.0, 1.0)];
    }
    let mask = if mask.count_ones() as usize >= channels {
        mask
    } else {
        // No (or too short a) channel mask: the usual layouts for this many channels.
        match channels {
            2 => 0x3,
            3 => 0x7,
            4 => 0x33,
            5 => 0x37,
            6 => 0x3F,
            7 => 0x13F,
            _ => 0x63F,
        }
    };
    let mut speakers = (0..32).filter(|b| mask & (1 << b) != 0);
    let mut w: Vec<(f32, f32)> = (0..channels)
        .map(|_| match speakers.next().map(|b| 1u32 << b) {
            Some(0x1) | Some(0x40) => (1.0, 0.0),     // front left, front left of centre
            Some(0x2) | Some(0x80) => (0.0, 1.0),     // front right, front right of centre
            Some(0x4) => (H, H),                      // centre
            Some(0x8) => (0.0, 0.0),                  // LFE
            Some(0x10) | Some(0x200) => (H, 0.0),     // back left, side left
            Some(0x20) | Some(0x400) => (0.0, H),     // back right, side right
            Some(0x1000) | Some(0x8000) => (H, 0.0),  // top front / back left
            Some(0x4000) | Some(0x20000) => (0.0, H), // top front / back right
            Some(_) => (0.5, 0.5),                    // back centre, top centres
            None => (0.0, 0.0),
        })
        .collect();
    let (l, r) = w.iter().fold((0.0f32, 0.0f32), |(l, r), &(a, b)| (l + a, r + b));
    let peak = l.max(r);
    if peak > 1.0 {
        for x in &mut w {
            x.0 /= peak;
            x.1 /= peak;
        }
    }
    w
}

/// Device bytes (whole frames) → stereo f32 frames, appended to `out`.
pub(crate) fn decode(f: &Format, weights: &[(f32, f32)], bytes: &[u8], out: &mut Vec<[f32; 2]>) {
    let size = f.sample_bytes();
    for frame in bytes.chunks_exact(f.block_align) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (ch, &(wl, wr)) in weights.iter().enumerate().take(f.channels) {
            let b = &frame[ch * size..ch * size + size];
            let s = match f.kind {
                SampleKind::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                SampleKind::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                SampleKind::I24 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                SampleKind::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            };
            l += s * wl;
            r += s * wr;
        }
        out.push([l, r]);
    }
}

/// Linear-interpolation resampler, stereo, streaming (keeps the last input frame between calls).
pub(crate) struct Resampler {
    step: f64,
    /// Position of the next output frame in the current input block; -1 = `prev`.
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Resampler {
        Resampler { step: from as f64 / to as f64, pos: 0.0, prev: [0.0; 2] }
    }

    pub(crate) fn process(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        let Some(&last) = input.last() else { return };
        let n = input.len() as f64;
        while self.pos < n - 1.0 {
            let i = self.pos.floor();
            let t = (self.pos - i) as f32;
            let i = i as isize;
            let a = if i < 0 { self.prev } else { input[i as usize] };
            let b = input[(i + 1) as usize];
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = last;
    }
}

/// Collects 48 kHz stereo frames and hands them on in 960-byte chunks with their capture time.
pub(crate) struct Chunker<F: FnMut(&[u8], u64)> {
    on_chunk: F,
    pending: Vec<i16>,
    /// Capture time of `pending[0]`.
    start_us: u64,
    /// Capture time just past the last chunk sent.
    end_us: Option<u64>,
    bytes: [u8; CHUNK_BYTES],
}

impl<F: FnMut(&[u8], u64)> Chunker<F> {
    pub(crate) fn new(on_chunk: F) -> Self {
        Chunker { on_chunk, pending: Vec::with_capacity(CHUNK_SAMPLES * 4), start_us: 0, end_us: None, bytes: [0; CHUNK_BYTES] }
    }

    /// `frames` start at capture time `first_us`.
    pub(crate) fn push(&mut self, frames: &[[f32; 2]], first_us: u64) {
        // Anchored on every packet, so the device clock and ours can't drift apart.
        let held = (self.pending.len() / 2) as u64 * 1_000_000 / OUT_RATE as u64;
        self.start_us = first_us.saturating_sub(held);
        for f in frames {
            self.pending.push(to_i16(f[0]));
            self.pending.push(to_i16(f[1]));
        }
        let mut done = 0;
        while self.pending.len() - done >= CHUNK_SAMPLES {
            for (i, s) in self.pending[done..done + CHUNK_SAMPLES].iter().enumerate() {
                self.bytes[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
            }
            (self.on_chunk)(&self.bytes, self.start_us);
            self.start_us += CHUNK_US;
            self.end_us = Some(self.start_us);
            done += CHUNK_SAMPLES;
        }
        self.pending.drain(..done);
    }

    /// Sends what's left, padded to a whole chunk with silence.
    pub(crate) fn flush_padded(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let missing = CHUNK_FRAMES - self.pending.len() / 2;
        let start = self.start_us;
        self.push(&vec![[0.0; 2]; missing], start + (self.pending.len() / 2) as u64 * 1_000_000 / OUT_RATE as u64);
    }

    /// One chunk of silence captured at `at_us`.
    pub(crate) fn silence(&mut self, at_us: u64) {
        self.pending.clear();
        self.bytes = [0; CHUNK_BYTES];
        (self.on_chunk)(&self.bytes, at_us);
        self.end_us = Some(at_us + CHUNK_US);
    }

    pub(crate) fn end_us(&self) -> Option<u64> {
        self.end_us
    }
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix() {
        assert_eq!(downmix_weights(1, 0), vec![(1.0, 1.0)]);
        assert_eq!(downmix_weights(2, 0), vec![(1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(downmix_weights(2, 0x3), vec![(1.0, 0.0), (0.0, 1.0)]);
        // 5.1: FL FR FC LFE BL BR, normalized so a full-scale mix can't clip.
        let w = downmix_weights(6, 0x3F);
        let (l, r) = w.iter().fold((0.0, 0.0), |(l, r), &(a, b)| (l + a, r + b));
        assert!((l - 1.0f32).abs() < 1e-5 && (r - 1.0f32).abs() < 1e-5);
        assert_eq!(w[3], (0.0, 0.0)); // LFE dropped
        assert!(w[0].0 > w[2].0 && w[2].0 == w[2].1 && w[0].1 == 0.0);
        // 7.1 with no mask behaves like the 7.1 mask.
        assert_eq!(downmix_weights(8, 0), downmix_weights(8, 0x63F));
        // A stereo signal in a 6-channel stream decodes to L/R only.
        let f = Format::new(TAG_FLOAT, 32, 6, 48_000, 24, 0x3F).unwrap();
        let mut bytes = Vec::new();
        for s in [1.0f32, -1.0, 0.0, 0.0, 0.0, 0.0] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        decode(&f, &w, &bytes, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0][0] > 0.0 && out[0][1] < 0.0 && (out[0][0] + out[0][1]).abs() < 1e-6);
    }

    #[test]
    fn integer_formats() {
        let w = downmix_weights(2, 0);
        let f = Format::new(TAG_PCM, 16, 2, 48_000, 4, 0).unwrap();
        let mut out = Vec::new();
        decode(&f, &w, &[0x00, 0x40, 0x00, 0xC0], &mut out);
        assert_eq!(out, vec![[0.5, -0.5]]);
        let f = Format::new(TAG_PCM, 24, 2, 48_000, 6, 0).unwrap();
        out.clear();
        decode(&f, &w, &[0, 0, 0x40, 0, 0, 0xC0], &mut out);
        assert_eq!(out, vec![[0.5, -0.5]]);
        let f = Format::new(TAG_PCM, 32, 2, 48_000, 8, 0).unwrap();
        out.clear();
        decode(&f, &w, &[0, 0, 0, 0x40, 0, 0, 0, 0xC0], &mut out);
        assert_eq!(out, vec![[0.5, -0.5]]);
        assert!(Format::new(TAG_PCM, 8, 2, 48_000, 2, 0).is_err());
        assert!(Format::new(TAG_FLOAT, 64, 2, 48_000, 16, 0).is_err());
    }

    #[test]
    fn resampler_rates() {
        for from in [44_100u32, 96_000, 32_000] {
            let mut r = Resampler::new(from, 48_000);
            let mut out = Vec::new();
            // One second in odd-sized packets.
            let mut fed = 0;
            for size in [441usize, 100, 1, 7, 1000].iter().cycle() {
                if fed >= from as usize {
                    break;
                }
                let n = (*size).min(from as usize - fed);
                r.process(&vec![[0.25, -0.25]; n], &mut out);
                fed += n;
            }
            assert!((out.len() as i64 - 48_000).abs() <= 2, "{from}: {}", out.len());
            // A constant stays constant (after the first frame, which ramps from silence).
            assert!(out[1..].iter().all(|f| (f[0] - 0.25).abs() < 1e-6 && (f[1] + 0.25).abs() < 1e-6));
        }
    }

    #[test]
    fn resampler_is_continuous_across_packets() {
        // A ramp resampled in pieces equals the ramp resampled at once.
        let input: Vec<[f32; 2]> = (0..4410).map(|i| [i as f32, -(i as f32)]).collect();
        let mut whole = Vec::new();
        Resampler::new(44_100, 48_000).process(&input, &mut whole);
        let mut pieces = Vec::new();
        let mut r = Resampler::new(44_100, 48_000);
        for c in input.chunks(37) {
            r.process(c, &mut pieces);
        }
        assert_eq!(whole.len(), pieces.len());
        for (a, b) in whole.iter().zip(&pieces) {
            assert!((a[0] - b[0]).abs() < 1e-3);
        }
        // Output k sits at input position k × 44100/48000.
        assert!((pieces[480][0] - 441.0).abs() < 1e-3);
    }

    #[test]
    fn chunks_of_960_bytes_with_times() {
        let mut got: Vec<(Vec<u8>, u64)> = Vec::new();
        {
            let mut c = Chunker::new(|b: &[u8], t| got.push((b.to_vec(), t)));
            c.push(&vec![[1.0, -1.0]; 100], 1_000_000);
            c.push(&vec![[0.5, 0.0]; 400], 1_000_000 + 100 * 1_000_000 / 48_000);
            c.flush_padded();
            c.silence(2_000_000);
            assert_eq!(c.end_us(), Some(2_005_000));
        }
        assert_eq!(got.len(), 4);
        assert!(got.iter().all(|(b, _)| b.len() == 960));
        assert_eq!(&got[0].0[0..4], &[0xFF, 0x7F, 0x01, 0x80]); // 32767, -32767
        assert_eq!(got[0].1, 1_000_000);
        assert!((got[1].1 as i64 - 1_005_000).abs() <= 21);
        // Chunk 3: frames 480..500 real, then padding.
        assert_eq!(&got[2].0[0..4], &to_i16(0.5).to_le_bytes().iter().chain(&0i16.to_le_bytes()).copied().collect::<Vec<_>>()[..]);
        assert!(got[2].0[20 * 4..].iter().all(|&b| b == 0));
        assert!(got[3].0.iter().all(|&b| b == 0));
        assert_eq!(got[3].1, 2_000_000);
    }
}
