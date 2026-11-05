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
//!
//! **Leaving apps out** (`Loopback::set_excluded`, docs/08-gaming-host.md "Sound: leave chosen apps
//! out"): with a non-empty list of executable names the thread switches from the loopback of the
//! whole device to WASAPI *process loopback* (Windows 10 2004 / build 19041 or later). Windows can
//! exclude only one process tree per stream, so instead every other app that has an audio session on
//! the default output is captured on its own (INCLUDE_TARGET_PROCESS_TREE, one stream per app's
//! process tree: Chrome-style apps play from a child process, so the tree's top process of the same
//! name is used) and the streams are mixed. A process is left out when it, or a process above it,
//! has a left-out name. Sessions are looked at again every second, so an app that starts playing is
//! picked up within about a second. Each stream is asked for 48 kHz stereo float (Windows converts),
//! placed on a common timeline by its packets' capture times, and summed (clipped when converted to
//! 16 bit) into a chunk every 5 ms of captured audio, silence where no app has data. System sounds
//! (the session without a process) are lost while apps are left out. If process loopback can't be
//! opened (older Windows), it says so once in the log and shares all sound as before.

use crate::core::now_us;
use crate::logi;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, S_OK};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, GetCurrentProcessId, GetCurrentThread, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, SetThreadPriority, SetWaitableTimer, THREAD_PRIORITY_HIGHEST, TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, PCWSTR};

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
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// The apps to leave out, set from any thread and read by the capture thread.
struct Shared {
    excluded: Mutex<Vec<String>>,
    /// Bumped on every change of `excluded`, so the capture thread can look without locking.
    generation: AtomicU64,
}

impl Shared {
    fn get(&self) -> (Vec<String>, u64) {
        let list = self.excluded.lock().unwrap_or_else(|e| e.into_inner());
        (list.clone(), self.generation.load(Acquire))
    }

    fn generation(&self) -> u64 {
        self.generation.load(Acquire)
    }
}

impl Loopback {
    pub fn start(on_chunk: impl FnMut(&[u8], u64) + Send + 'static) -> Result<Loopback, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared { excluded: Mutex::new(Vec::new()), generation: AtomicU64::new(0) });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let s = stop.clone();
        let sh = shared.clone();
        let on_chunk: OnChunk = Box::new(on_chunk);
        let thread = std::thread::Builder::new()
            .name("bb1 loopback".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                unsafe {
                    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
                }
                run(Chunker::new(on_chunk), &sh, &s, ready_tx);
                if com {
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|e| format!("sound capture thread: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Loopback { stop, shared, thread: Some(thread) }),
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

    /// Leave these apps out of the shared sound: executable file names such as "discord.exe"
    /// (any case; a full path works too). Empty = share everything. Takes effect within about a
    /// second; callable from any thread.
    pub fn set_excluded(&self, exe_names: Vec<String>) {
        let list = normalize_exe_names(exe_names);
        let mut cur = self.shared.excluded.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != list {
            *cur = list;
            self.shared.generation.fetch_add(1, Release);
        }
    }
}

/// Lowercase file names, sorted, without duplicates or empties.
pub(crate) fn normalize_exe_names(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .map(|n| n.trim().rsplit(['\\', '/']).next().unwrap_or("").trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Whether this Windows can capture single apps (process loopback).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PerApp {
    Unknown,
    Available,
    Unavailable,
}

/// Opens the default device and captures until stopped, opening it again whenever it goes away.
/// With apps left out (and process loopback working), mixes the other apps instead.
fn run(mut sink: Chunker<OnChunk>, shared: &Shared, stop: &AtomicBool, ready: mpsc::Sender<Result<(), String>>) {
    let mut ready = Some(ready);
    let mut failures = 0u32;
    let mut per_app = PerApp::Unknown;
    let mut mixing = false;
    let timer = Timer::new();
    while !stop.load(Relaxed) {
        let (excluded, generation) = shared.get();
        if !excluded.is_empty() && per_app != PerApp::Unavailable {
            if let Some(tx) = ready.take() {
                let _ = tx.send(Ok(()));
            }
            mixing = true;
            let opened = Instant::now();
            let result = unsafe { capture_mixed(&mut sink, shared, stop, &timer, &mut per_app) };
            sink.flush_padded();
            match result {
                Ok(()) => failures = 0,
                Err(e) => {
                    failures += 1;
                    if failures <= 3 {
                        logi!("sound capture (apps left out) stopped ({e}); starting again");
                    }
                    if opened.elapsed() < Duration::from_secs(5) {
                        sleep_unless(stop, Duration::from_millis(if failures > 3 { 2000 } else { 300 }));
                    }
                }
            }
            continue;
        }
        if mixing {
            mixing = false;
            if excluded.is_empty() {
                logi!("sound capture: no apps left out; sharing all sound");
            }
        }
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
        // While leaving apps out is possible, a change of the list ends this capture.
        let watch = (per_app != PerApp::Unavailable).then_some((shared, generation));
        match unsafe { session.capture(&mut sink, stop, &timer, watch) } {
            Ok(()) => {}
            Err(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED => logi!("sound capture: the output device went away; opening the new default"),
            Err(e) => logi!("sound capture stopped ({e}); starting again"),
        }
        drop(session);
        sink.flush_padded();
        if watch.is_some() && shared.generation() != generation {
            failures = 0;
        } else if opened.elapsed() > Duration::from_secs(5) {
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
    /// Also returns (Ok) when `watch`'s generation changes: the list of left-out apps changed.
    unsafe fn capture(&mut self, sink: &mut Chunker<OnChunk>, stop: &AtomicBool, timer: &Timer, watch: Option<(&Shared, u64)>) -> windows::core::Result<()> {
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
            if watch.is_some_and(|(shared, generation)| shared.generation() != generation) {
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
                let age_us = packet_age_us(qpc, flags, frames, self.format.rate, qpc_freq);
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

// ---------------------------------------------------------------------------------------------
// Leaving apps out: one process-loopback stream per other app, mixed.

/// Chunks are mixed this long after their capture time, so every stream's packet for it is in.
const MIX_DELAY_FRAMES: i64 = 720; // 15 ms
/// A packet more than this far from where its stream left off moves the stream (clock drift,
/// a pause in the app's sound).
const REALIGN_FRAMES: i64 = 960; // 20 ms
/// A gap longer than this starts the stream afresh instead of filling it with silence.
const GAP_RESET_FRAMES: i64 = 24_000; // 0.5 s
/// At most this much is held per stream.
const TRACK_MAX_FRAMES: usize = 24_000;
/// The mixer more than this behind real time (thread starved) skips ahead.
const MIX_BEHIND_FRAMES: i64 = 12_000; // 250 ms
const SESSION_REFRESH: Duration = Duration::from_secs(1);
/// After failing to open an app's stream, try again this much later.
const RETRY_AFTER: Duration = Duration::from_secs(10);

/// Microseconds on `now_us`'s clock ↔ 48 kHz frame positions.
fn us_to_pos(us: u64) -> i64 {
    (us as i128 * OUT_RATE as i128 / 1_000_000) as i64
}

fn pos_to_us(pos: i64) -> u64 {
    (pos.max(0) as i128 * 1_000_000 / OUT_RATE as i128) as u64
}

/// Captures the apps that aren't left out, mixed, until stopped, the list empties, or process
/// loopback turns out not to work here (then `per_app` = Unavailable and Ok).
unsafe fn capture_mixed(sink: &mut Chunker<OnChunk>, shared: &Shared, stop: &AtomicBool, timer: &Timer, per_app: &mut PerApp) -> Result<(), String> {
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| format!("device list: {e}"))?;
    if *per_app == PerApp::Unknown {
        // Our own process: opening it works wherever process loopback does.
        match unsafe { ProcessStream::open(GetCurrentProcessId()) } {
            Ok(test) => {
                trace("own process opened; closing it");
                drop(test);
                trace("own process closed");
                *per_app = PerApp::Available
            }
            Err(e) => {
                logi!("sound capture: this Windows can't leave apps out (process loopback: {e}; needs Windows 10 2004 or later); sharing all sound");
                *per_app = PerApp::Unavailable;
                return Ok(());
            }
        }
    }
    let (mut excluded, mut generation) = shared.get();
    if excluded.is_empty() {
        return Ok(());
    }
    let mut mixer = Mixer { enumerator, sessions: None, sources: Vec::new(), failed: HashMap::new(), logged: None, session_error: false };
    let qpc_freq = qpc_frequency();
    let mut next = us_to_pos(now_us()) - MIX_DELAY_FRAMES - CHUNK_FRAMES as i64;
    if let Some(end) = sink.end_us() {
        next = next.max(us_to_pos(end));
    }
    let mut refreshed: Option<Instant> = None;
    let mut mix = vec![[0.0f32; 2]; CHUNK_FRAMES];
    let mut decoded: Vec<[f32; 2]> = Vec::new();
    loop {
        if stop.load(Relaxed) {
            return Ok(());
        }
        if shared.generation() != generation {
            (excluded, generation) = shared.get();
            if excluded.is_empty() {
                return Ok(());
            }
            refreshed = None;
        }
        if refreshed.is_none_or(|t| t.elapsed() >= SESSION_REFRESH) {
            refreshed = Some(Instant::now());
            unsafe { mixer.refresh(&excluded) };
        }
        mixer.sources.retain_mut(|s| match unsafe { s.read(&mut decoded, qpc_freq) } {
            Ok(()) => true,
            Err(e) => {
                logi!("sound capture: stopped capturing {} (pid {}): {e}", s.name, s.pid);
                false
            }
        });
        let now = us_to_pos(now_us());
        if now - next > MIX_BEHIND_FRAMES {
            next = now - MIX_DELAY_FRAMES - CHUNK_FRAMES as i64;
        }
        while next + CHUNK_FRAMES as i64 + MIX_DELAY_FRAMES <= now {
            mix.fill([0.0; 2]);
            for s in &mut mixer.sources {
                s.track.take_into(next, &mut mix);
            }
            // The chunker is empty here, so this is exactly one chunk at `next`.
            sink.push(&mix, pos_to_us(next));
            next += CHUNK_FRAMES as i64;
        }
        timer.wait();
    }
}

struct Mixer {
    enumerator: IMMDeviceEnumerator,
    /// The default output's id and its session manager.
    sessions: Option<(String, IAudioSessionManager2)>,
    sources: Vec<Source>,
    /// (pid, creation time) → when opening its stream last failed.
    failed: HashMap<(u32, u64), Instant>,
    /// The last "leaving out … mixing …" line logged.
    logged: Option<String>,
    session_error: bool,
}

struct Source {
    pid: u32,
    created: u64,
    name: String,
    stream: ProcessStream,
    track: Track,
}

impl Source {
    unsafe fn read(&mut self, decoded: &mut Vec<[f32; 2]>, qpc_freq: i64) -> windows::core::Result<()> {
        let s = &self.stream;
        loop {
            let size = unsafe { s.capture.GetNextPacketSize()? };
            if size == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            unsafe { s.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))? };
            let now = now_us();
            let age_us = packet_age_us(qpc, flags, frames, OUT_RATE, qpc_freq);
            decoded.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                decoded.resize(frames as usize, [0.0; 2]);
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(data, frames as usize * s.format.block_align) };
                decode(&s.format, &s.weights, bytes, decoded);
            }
            unsafe { s.capture.ReleaseBuffer(frames)? };
            self.track.push(us_to_pos(now.saturating_sub(age_us)), decoded);
        }
    }
}

impl Mixer {
    /// Looks at who has sound now and opens / closes streams to match.
    unsafe fn refresh(&mut self, excluded: &[String]) {
        trace("listing apps with sound");
        let pids = match unsafe { self.session_pids() } {
            Ok(p) => {
                self.session_error = false;
                p
            }
            Err(e) => {
                if !self.session_error {
                    logi!("sound capture: couldn't list the apps playing sound ({e})");
                    self.session_error = true;
                }
                self.sessions = None;
                return;
            }
        };
        // bb1's own sound (a stream this PC is watching) never goes back out.
        let own = unsafe { GetCurrentProcessId() };
        let pids: Vec<u32> = pids.into_iter().filter(|&p| p != own).collect();
        trace(&format!("{} apps with sound: {pids:?}", pids.len()));
        let procs = process_table();
        trace(&format!("{} processes", procs.len()));
        let plan = plan_capture(&procs, &pids, excluded);
        trace(&format!("plan: include {:?}", plan.include.iter().map(|t| (t.pid, t.name.clone())).collect::<Vec<_>>()));
        let want: HashSet<(u32, u64)> = plan.include.iter().map(|t| (t.pid, t.created)).collect();
        self.sources.retain(|s| want.contains(&(s.pid, s.created)));
        for t in &plan.include {
            let key = (t.pid, t.created);
            if self.sources.iter().any(|s| (s.pid, s.created) == key) {
                continue;
            }
            let failed_before = self.failed.get(&key).copied();
            if failed_before.is_some_and(|at| at.elapsed() < RETRY_AFTER) {
                continue;
            }
            match unsafe { ProcessStream::open(t.pid) } {
                Ok(stream) => {
                    self.failed.remove(&key);
                    self.sources.push(Source { pid: t.pid, created: t.created, name: t.name.clone(), stream, track: Track::default() });
                }
                Err(e) => {
                    if failed_before.is_none() {
                        logi!("sound capture: couldn't capture {} (pid {}): {e}", t.name, t.pid);
                    }
                    self.failed.insert(key, Instant::now());
                }
            }
        }
        self.failed.retain(|k, _| want.contains(k));
        let line = describe_plan(excluded, &plan, &self.sources.iter().map(|s| (s.pid, s.name.as_str())).collect::<Vec<_>>());
        if self.logged.as_deref() != Some(line.as_str()) {
            logi!("{line}");
            self.logged = Some(line);
        }
    }

    /// Processes with an audio session on the default output (not expired; not system sounds).
    unsafe fn session_pids(&mut self) -> windows::core::Result<Vec<u32>> {
        unsafe {
            let device = self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let id = device_id(&device);
            if self.sessions.as_ref().is_none_or(|(have, _)| *have != id) {
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                self.sessions = Some((id, manager));
            }
            let Some((_, manager)) = &self.sessions else { return Ok(Vec::new()) };
            let list = manager.GetSessionEnumerator()?;
            let mut pids = Vec::new();
            for i in 0..list.GetCount()? {
                let Ok(session) = list.GetSession(i) else { continue };
                if session.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                let Ok(session) = session.cast::<IAudioSessionControl2>() else { continue };
                if session.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                if let Ok(pid) = session.GetProcessId()
                    && pid != 0
                {
                    pids.push(pid);
                }
            }
            Ok(pids)
        }
    }
}

/// "sound capture: leaving out chrome.exe, discord.exe (playing: discord.exe); mixing …".
fn describe_plan(excluded: &[String], plan: &CapturePlan, mixing: &[(u32, &str)]) -> String {
    let mut playing: Vec<&str> = plan.left_out.iter().map(|(_, n)| n.as_str()).collect();
    playing.sort();
    playing.dedup();
    let mut line = format!("sound capture: leaving out {}", excluded.join(", "));
    if !playing.is_empty() {
        line += &format!(" (playing: {})", playing.join(", "));
    }
    if mixing.is_empty() {
        line += "; nothing else is playing";
    } else {
        let list: Vec<String> = mixing.iter().map(|(pid, n)| format!("{n} (pid {pid})")).collect();
        line += &format!("; mixing {}", list.join(", "));
    }
    if !plan.skipped.is_empty() {
        let list: Vec<String> = plan.skipped.iter().map(|(pid, n)| format!("{n} (pid {pid})")).collect();
        line += &format!("; not shared, a left-out app runs under it: {}", list.join(", "));
    }
    line
}

/// Every process: id, parent, lowercase exe name, creation time (FILETIME, 0 = unknown).
fn process_table() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
            out.push(Proc { pid: e.th32ProcessID, ppid: e.th32ParentProcessID, name, created: process_created(e.th32ProcessID) });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn process_created(pid: u32) -> u64 {
    if pid == 0 {
        return 0;
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return 0 };
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let t = if GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user).is_ok() {
            (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64
        } else {
            0
        };
        let _ = CloseHandle(h);
        t
    }
}

/// A process, as far as choosing what to capture goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// Lowercase exe file name.
    pub name: String,
    /// Creation time (any monotonic unit), 0 = unknown. Guards against a reused parent id.
    pub created: u64,
}

/// A process tree to capture: its top process.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Target {
    pub pid: u32,
    pub created: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CapturePlan {
    /// Process trees to capture (INCLUDE_TARGET_PROCESS_TREE), none inside another.
    pub include: Vec<Target>,
    /// Processes with sound that are left out (pid, name).
    pub left_out: Vec<(u32, String)>,
    /// Processes with sound that can't be shared without a left-out app: a left-out app was
    /// started by them, and its sound would come along with theirs.
    pub skipped: Vec<(u32, String)>,
}

struct Tree<'a> {
    by_pid: HashMap<u32, &'a Proc>,
}

impl<'a> Tree<'a> {
    fn parent(&self, p: &Proc) -> Option<&'a Proc> {
        if p.ppid == 0 || p.ppid == p.pid {
            return None;
        }
        let q = *self.by_pid.get(&p.ppid)?;
        // A parent created after its child is a later process that got the same id.
        (q.created == 0 || p.created == 0 || q.created <= p.created).then_some(q)
    }

    /// `p`, its parent, grandparent, … (at most 64, no repeats).
    fn chain(&self, p: &'a Proc) -> Vec<&'a Proc> {
        let mut out = vec![p];
        let mut cur = p;
        while out.len() < 64 {
            match self.parent(cur) {
                Some(q) if !out.iter().any(|x| x.pid == q.pid) => {
                    out.push(q);
                    cur = q;
                }
                _ => break,
            }
        }
        out
    }

    /// The top of `p`'s run of same-named processes (Chrome's browser process for its audio
    /// process).
    fn app_root(&self, p: &'a Proc) -> &'a Proc {
        let mut root = p;
        for q in self.chain(p).into_iter().skip(1) {
            if q.name != p.name {
                break;
            }
            root = q;
        }
        root
    }
}

/// Which process trees to capture so that everything but the `excluded` apps is heard, given
/// the processes that have audio sessions.
pub(crate) fn plan_capture(procs: &[Proc], session_pids: &[u32], excluded: &[String]) -> CapturePlan {
    let tree = Tree { by_pid: procs.iter().map(|p| (p.pid, p)).collect() };
    let is_excluded = |name: &str| excluded.iter().any(|e| e == name);
    let left_out_procs: Vec<&Proc> = procs.iter().filter(|p| is_excluded(&p.name)).collect();
    // Does capturing `root`'s tree bring in a left-out app?
    let holds_excluded = |root: &Proc| left_out_procs.iter().any(|e| tree.chain(e).iter().any(|q| q.pid == root.pid));
    let mut plan = CapturePlan::default();
    let mut seen = HashSet::new();
    let mut targets: Vec<&Proc> = Vec::new();
    for &pid in session_pids {
        if pid == 0 || !seen.insert(pid) {
            continue;
        }
        let Some(&p) = tree.by_pid.get(&pid) else { continue };
        if tree.chain(p).iter().any(|q| is_excluded(&q.name)) {
            plan.left_out.push((p.pid, p.name.clone()));
            continue;
        }
        let mut root = tree.app_root(p);
        if holds_excluded(root) {
            root = p;
        }
        if holds_excluded(root) {
            plan.skipped.push((p.pid, p.name.clone()));
            continue;
        }
        if !targets.iter().any(|t| t.pid == root.pid) {
            targets.push(root);
        }
    }
    // A tree inside another would be heard twice.
    let inside_another = |t: &Proc| tree.chain(t).iter().skip(1).any(|q| targets.iter().any(|o| o.pid == q.pid));
    let mut include: Vec<Target> =
        targets.iter().filter(|t| !inside_another(t)).map(|t| Target { pid: t.pid, created: t.created, name: t.name.clone() }).collect();
    include.sort_by_key(|t| t.pid);
    plan.include = include;
    plan
}

/// One app's stream, placed on the 48 kHz timeline by its packets' capture times.
#[derive(Default)]
pub(crate) struct Track {
    frames: VecDeque<[f32; 2]>,
    /// Timeline position of `frames[0]` (= just past the last frame when empty).
    start: i64,
    started: bool,
}

impl Track {
    fn end(&self) -> i64 {
        self.start + self.frames.len() as i64
    }

    /// `input` captured from timeline position `at` on.
    pub(crate) fn push(&mut self, at: i64, input: &[[f32; 2]]) {
        if input.is_empty() {
            return;
        }
        let end = self.end();
        if !self.started || at > end + GAP_RESET_FRAMES {
            self.frames.clear();
            self.start = at;
            self.started = true;
        } else if at > end + REALIGN_FRAMES {
            self.frames.extend(std::iter::repeat_n([0.0; 2], (at - end) as usize));
        } else if at < end - REALIGN_FRAMES {
            let cut = (end - at) as usize;
            if cut >= self.frames.len() {
                self.frames.clear();
                self.start = at;
            } else {
                self.frames.truncate(self.frames.len() - cut);
            }
        }
        self.frames.extend(input.iter().copied());
        if self.frames.len() > TRACK_MAX_FRAMES {
            let d = self.frames.len() - TRACK_MAX_FRAMES;
            self.frames.drain(..d);
            self.start += d as i64;
        }
    }

    /// Adds this stream's frames for positions `pos..pos + out.len()` into `out` and lets go of
    /// them (and of anything older, which came too late).
    pub(crate) fn take_into(&mut self, pos: i64, out: &mut [[f32; 2]]) {
        if self.start < pos {
            let late = ((pos - self.start) as usize).min(self.frames.len());
            self.frames.drain(..late);
            self.start += late as i64;
        }
        if self.frames.is_empty() || self.start < pos {
            return;
        }
        let offset = (self.start - pos) as usize;
        if offset >= out.len() {
            return;
        }
        let n = (out.len() - offset).min(self.frames.len());
        for (o, f) in out[offset..offset + n].iter_mut().zip(self.frames.drain(..n)) {
            o[0] += f[0];
            o[1] += f[1];
        }
        self.start += n as i64;
    }
}

/// A process-loopback capture of one process tree, 48 kHz stereo.
struct ProcessStream {
    audio: IAudioClient,
    capture: IAudioCaptureClient,
    format: Format,
    weights: Vec<(f32, f32)>,
    /// Signalled by Windows per packet (process loopback wants event mode); we poll instead.
    _event: OwnedEvent,
}

impl ProcessStream {
    /// Captures process `pid` and everything it started.
    unsafe fn open(pid: u32) -> Result<ProcessStream, String> {
        // Float first (no clipping before the mix); 16-bit as in Microsoft's sample otherwise.
        // A client that failed Initialize can't be used again, so each try activates anew.
        let tries: [(u32, u16, i64); 3] = [(TAG_FLOAT, 32, 10_000_000), (TAG_FLOAT, 32, 2_000_000), (TAG_PCM, 16, 2_000_000)];
        let mut last = String::new();
        for (tag, bits, buffer_hns) in tries {
            let audio = unsafe { activate_process_loopback(pid)? };
            trace("activation returned");
            let block = 2 * bits / 8;
            let wf = WAVEFORMATEX {
                wFormatTag: tag as u16,
                nChannels: 2,
                nSamplesPerSec: OUT_RATE,
                nAvgBytesPerSec: OUT_RATE * block as u32,
                nBlockAlign: block,
                wBitsPerSample: bits,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            trace(&format!("initializing pid {pid}: tag {tag} bits {bits} buffer {buffer_hns}"));
            let init = unsafe { audio.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buffer_hns, 0, &wf, None) };
            trace(&format!("initialized pid {pid}: {init:?}"));
            if let Err(e) = init {
                last = format!("{e}");
                continue;
            }
            let event = OwnedEvent(unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(|e| format!("event: {e}"))?);
            unsafe { audio.SetEventHandle(event.0) }.map_err(|e| format!("event: {e}"))?;
            let capture: IAudioCaptureClient = unsafe { audio.GetService() }.map_err(|e| format!("capture: {e}"))?;
            unsafe { audio.Start() }.map_err(|e| format!("start: {e}"))?;
            trace(&format!("started pid {pid}"));
            let format = Format::new(tag, bits, 2, OUT_RATE, block as usize, 0x3)?;
            let weights = downmix_weights(2, 0x3);
            return Ok(ProcessStream { audio, capture, format, weights, _event: event });
        }
        Err(last)
    }
}

impl Drop for ProcessStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.audio.Stop();
        }
    }
}

struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Step-by-step lines while finding out what goes wrong (BB1_SOUND_TRACE=1).
fn trace(line: &str) {
    if std::env::var_os("BB1_SOUND_TRACE").is_some() {
        logi!("sound trace: {line}");
    }
}

/// An IAudioClient for process loopback of `pid`'s tree (ActivateAudioInterfaceAsync, waited for).
unsafe fn activate_process_loopback(pid: u32) -> Result<IAudioClient, String> {
    // The activation parameters live in COM-allocated memory that Rust never frees: Windows takes
    // the PROPVARIANT's blob over (freeing it with Rust's allocator afterwards corrupted the heap, on
    // a real PC). If Windows doesn't free it, that's 16 bytes once per app stream opened.
    let params = unsafe { CoTaskMemAlloc(std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()) } as *mut AUDIOCLIENT_ACTIVATION_PARAMS;
    if params.is_null() {
        return Err("activate: out of memory".into());
    }
    unsafe {
        params.write(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
    }
    let blob = BLOB { cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: params as *mut u8 };
    // Leaked on purpose for the same reason (it's a few bytes).
    let prop: &'static PROPVARIANT = Box::leak(Box::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { blob },
            }),
        },
    }));
    let (tx, rx) = mpsc::channel();
    let handler = Completion::new(tx);
    trace(&format!("activating process loopback for pid {pid}"));
    let op = unsafe { ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(prop as *const PROPVARIANT), &handler) }
        .map_err(|e| format!("activate: {e}"))?;
    let waited = rx.recv_timeout(Duration::from_secs(3));
    trace(&format!("activation for pid {pid}: {}", if waited.is_ok() { "answered" } else { "timed out" }));
    if waited.is_err() {
        return Err("Windows didn't open it within 3 s".into());
    }
    let mut hr = HRESULT(0);
    let mut activated: Option<IUnknown> = None;
    unsafe { op.GetActivateResult(&mut hr, &mut activated) }.map_err(|e| format!("activate: {e}"))?;
    trace(&format!("activation result for pid {pid}: {hr:?}, interface {}", activated.is_some()));
    hr.ok().map_err(|e| format!("activate: {e}"))?;
    let activated = activated.ok_or("activate: no interface")?;
    trace("casting to IAudioClient");
    let client = activated.cast::<IAudioClient>().map_err(|e| format!("activate: {e}"));
    trace(&format!("cast: {}", client.is_ok()));
    drop(activated);
    trace("released the activation's interface");
    drop(op);
    trace("released the operation");
    drop(handler);
    trace("released the handler");
    client
}

/// IActivateAudioInterfaceCompletionHandler (agile: Windows calls it on its own thread). Says on a
/// channel when activation is done; the result is read from the operation after that. Built with
/// the windows crate's `#[implement]`, which gets the COM reference counting right.
// (#[implement] objects are agile already: Windows may call it from any thread.)
#[windows_core::implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion {
    done: Mutex<Option<mpsc::Sender<()>>>,
}

impl Completion {
    fn new(done: mpsc::Sender<()>) -> IActivateAudioInterfaceCompletionHandler {
        Completion { done: Mutex::new(Some(done)) }.into()
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self, _operation: windows_core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows_core::Result<()> {
        if let Some(tx) = self.done.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
        Ok(())
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

/// How long ago a packet's first frame was captured: from its QPC position (100 ns units) when
/// that's valid, else assume it just ended.
fn packet_age_us(qpc: u64, flags: u32, frames: u32, rate: u32, qpc_freq: i64) -> u64 {
    let packet_us = frames as u64 * 1_000_000 / rate.max(1) as u64;
    match qpc_now_hns(qpc_freq) {
        Some(t) if qpc != 0 && flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && t >= qpc => ((t - qpc) / 10).min(1_000_000),
        _ => packet_us,
    }
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
    fn excluded_names() {
        let n = normalize_exe_names(vec!["Discord.exe".into(), " C:\\Apps\\Chrome.EXE ".into(), "discord.exe".into(), "".into()]);
        assert_eq!(n, vec!["chrome.exe", "discord.exe"]);
    }

    fn proc(pid: u32, ppid: u32, name: &str, created: u64) -> Proc {
        Proc { pid, ppid, name: name.into(), created }
    }

    #[test]
    fn capture_plan() {
        let procs = vec![
            proc(4, 0, "system", 0),
            proc(100, 4, "explorer.exe", 10),
            // Chrome: browser process and its audio child.
            proc(200, 100, "chrome.exe", 20),
            proc(201, 200, "chrome.exe", 21),
            // Discord, started by Explorer; its voice process is a child.
            proc(300, 100, "discord.exe", 30),
            proc(301, 300, "discord.exe", 31),
            // Steam and a game it started: one tree.
            proc(400, 100, "steam.exe", 40),
            proc(401, 400, "game.exe", 41),
            // A parent id reused by a later process isn't the parent.
            proc(500, 999, "spotify.exe", 50),
            proc(999, 100, "discord.exe", 60),
            // A launcher that started Discord itself.
            proc(600, 100, "launcher.exe", 70),
            proc(601, 600, "discord.exe", 71),
            proc(602, 600, "launcher.exe", 72),
        ];
        let excluded = vec!["discord.exe".to_string()];
        let plan = plan_capture(&procs, &[201, 301, 401, 400, 500, 602, 0, 201, 12345], &excluded);
        let pids: Vec<u32> = plan.include.iter().map(|t| t.pid).collect();
        // Chrome by its browser process; Steam's tree holds the game; Spotify isn't under
        // Discord; launcher's child 602 alone (its parent's tree holds Discord).
        assert_eq!(pids, vec![200, 400, 500, 602]);
        assert_eq!(plan.left_out, vec![(301, "discord.exe".to_string())]);
        assert!(plan.skipped.is_empty());
        // Leaving out Chrome as well.
        let plan = plan_capture(&procs, &[201, 301, 500], &["chrome.exe".to_string(), "discord.exe".to_string()]);
        assert_eq!(plan.include.iter().map(|t| t.pid).collect::<Vec<_>>(), vec![500]);
        assert_eq!(plan.left_out.len(), 2);
        // Sound from a process a left-out app runs under can't be shared.
        let plan = plan_capture(&procs, &[600], &excluded);
        assert!(plan.include.is_empty());
        assert_eq!(plan.skipped, vec![(600, "launcher.exe".to_string())]);
        // Nothing left out: everyone by their tree.
        let plan = plan_capture(&procs, &[201, 301], &[]);
        assert_eq!(plan.include.iter().map(|t| t.pid).collect::<Vec<_>>(), vec![200, 300]);
    }

    #[test]
    fn track_timeline() {
        let mut t = Track::default();
        let mut out = vec![[0.0f32; 2]; 240];
        // Starts 100 frames into the chunk.
        t.push(1100, &vec![[0.5, -0.5]; 480]);
        t.take_into(1000, &mut out);
        assert!(out[..100].iter().all(|f| *f == [0.0, 0.0]));
        assert!(out[100..].iter().all(|f| *f == [0.5, -0.5]));
        // Next chunk continues where it left off.
        out.fill([0.0; 2]);
        t.take_into(1240, &mut out);
        assert!(out.iter().all(|f| *f == [0.5, -0.5]));
        // A small timestamp jitter keeps the stream contiguous.
        t.push(1580 + 50, &vec![[0.25, 0.25]; 240]);
        out.fill([0.0; 2]);
        t.take_into(1480, &mut out);
        assert!(out[..100].iter().all(|f| *f == [0.5, -0.5]));
        assert!(out[100..].iter().all(|f| *f == [0.25, 0.25]));
        // Data that arrives after its chunk went out is dropped.
        t.push(1820, &vec![[1.0, 1.0]; 240]);
        out.fill([0.0; 2]);
        t.take_into(2300, &mut out);
        assert!(out.iter().all(|f| *f == [0.0, 0.0]));
        // A long gap: starts afresh at the new time.
        t.push(100_000, &vec![[0.1, 0.1]; 240]);
        out.fill([0.0; 2]);
        t.take_into(100_000, &mut out);
        assert!(out.iter().all(|f| *f == [0.1, 0.1]));
        // A medium gap is filled with silence.
        t.push(100_240 + 2000, &vec![[0.2, 0.2]; 240]);
        out.fill([0.0; 2]);
        t.take_into(100_240 + 2000, &mut out);
        assert!(out.iter().all(|f| *f == [0.2, 0.2]));
    }

    #[test]
    fn mixing_sums_and_clips() {
        let mut a = Track::default();
        let mut b = Track::default();
        a.push(0, &vec![[0.75, -0.75]; 240]);
        b.push(0, &vec![[0.75, 0.25]; 240]);
        let mut mix = vec![[0.0f32; 2]; 240];
        a.take_into(0, &mut mix);
        b.take_into(0, &mut mix);
        assert_eq!(mix[0], [1.5, -0.5]);
        let mut got = Vec::new();
        Chunker::new(|c: &[u8], t| got.push((c.to_vec(), t))).push(&mix, pos_to_us(48_000));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, 1_000_000);
        assert_eq!(&got[0].0[0..2], &[0xFF, 0x7F]); // 32767: clipped
        assert_eq!(&got[0].0[2..4], &to_i16(-0.5).to_le_bytes());
        assert_eq!(us_to_pos(1_000_000), 48_000);
        assert_eq!(pos_to_us(us_to_pos(5_000)), 5_000);
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
