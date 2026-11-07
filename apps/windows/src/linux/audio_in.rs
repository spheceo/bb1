//! What this computer plays, for viewers: PipeWire captures the default output (its monitor) as
//! 48 kHz stereo 16-bit PCM, handed on in 5 ms chunks (960 bytes), the format the Mac host sends.
//!
//! When nothing plays, PipeWire may stop delivering (the output suspends); after 30 ms without data
//! silent chunks go out in real time, so the client's jitter buffer doesn't run dry and grow.

use crate::core::now_us;
use crate::logi;
use pipewire as pw;
use pw::spa;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::{Object, Pod, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const RATE: u32 = 48_000;
const CHUNK_FRAMES: usize = 240;
const CHUNK_BYTES: usize = CHUNK_FRAMES * 4;
const CHUNK_US: u64 = CHUNK_FRAMES as u64 * 1_000_000 / RATE as u64;

type Sink = Arc<Mutex<Box<dyn FnMut(&[u8], u64) + Send>>>;

pub struct SoundCapture {
    quit: pw::channel::Sender<()>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl SoundCapture {
    pub fn start(on_chunk: impl FnMut(&[u8], u64) + Send + 'static) -> Result<SoundCapture, String> {
        let sink: Sink = Arc::new(Mutex::new(Box::new(on_chunk)));
        let last_data = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (s, l) = (sink.clone(), last_data.clone());
        let capture = std::thread::Builder::new()
            .name("bb1 sound".into())
            .spawn(move || {
                if let Err(e) = run(s, l, quit_rx, ready_tx.clone()) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("PipeWire didn't answer".into()),
        }
        let (s, l, st) = (sink, last_data, stop.clone());
        let filler = std::thread::Builder::new()
            .name("bb1 silence".into())
            .spawn(move || {
                let silence = [0u8; CHUNK_BYTES];
                let mut next = 0u64;
                while !st.load(Relaxed) {
                    std::thread::sleep(Duration::from_millis(2));
                    let now = now_us();
                    if now.saturating_sub(l.load(Relaxed)) < 30_000 {
                        next = 0;
                        continue;
                    }
                    if next == 0 {
                        next = now;
                    }
                    while next + CHUNK_US <= now {
                        (s.lock().unwrap())(&silence, next);
                        next += CHUNK_US;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(SoundCapture { quit, stop, threads: vec![capture, filler] })
    }
}

impl Drop for SoundCapture {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        let _ = self.quit.send(());
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

struct UserData {
    sink: Sink,
    last_data: Arc<AtomicU64>,
    pending: Vec<u8>,
    ready: Option<std::sync::mpsc::Sender<Result<(), String>>>,
}

fn run(sink: Sink, last_data: Arc<AtomicU64>, quit: pw::channel::Receiver<()>, ready: std::sync::mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("PipeWire: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| format!("PipeWire: {e}"))?;
    let core = context.connect_rc(None).map_err(|e| format!("PipeWire isn't running: {e}"))?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "bb1-sound",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            *pw::keys::NODE_LATENCY => "240/48000",
            *pw::keys::NODE_NAME => "bb1-sound",
            *pw::keys::APP_NAME => "bb1",
        },
    )
    .map_err(|e| format!("PipeWire stream: {e}"))?;
    let data = UserData { sink, last_data, pending: Vec::with_capacity(CHUNK_BYTES * 8), ready: Some(ready) };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, ud, _, new| match new {
            pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming => {
                if let Some(r) = ud.ready.take() {
                    let _ = r.send(Ok(()));
                }
            }
            pw::stream::StreamState::Error(e) => {
                logi!("sound capture: {e}");
                if let Some(r) = ud.ready.take() {
                    let _ = r.send(Err(e));
                }
            }
            _ => {}
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let size = d.chunk().size() as usize;
            let offset = d.chunk().offset() as usize;
            let Some(bytes) = d.data() else { return };
            if size == 0 || bytes.len() < offset + size {
                return;
            }
            let now = now_us();
            ud.last_data.store(now, Relaxed);
            ud.pending.extend_from_slice(&bytes[offset..offset + size]);
            // The newest sample was captured about now; earlier ones before it.
            let total_frames = (ud.pending.len() / 4) as u64;
            let mut sent = 0usize;
            let mut sink = ud.sink.lock().unwrap();
            while ud.pending.len() - sent >= CHUNK_BYTES {
                let start_frame = (sent / 4) as u64;
                let at = now.saturating_sub((total_frames - start_frame) * 1_000_000 / RATE as u64);
                sink(&ud.pending[sent..sent + CHUNK_BYTES], at);
                sent += CHUNK_BYTES;
            }
            drop(sink);
            ud.pending.drain(..sent);
        })
        .register()
        .map_err(|e| format!("PipeWire: {e}"))?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::S16LE);
    info.set_rate(RATE);
    info.set_channels(2);
    let mut position = [0u32; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let obj = Object { type_: spa::sys::SPA_TYPE_OBJECT_Format, id: spa::sys::SPA_PARAM_EnumFormat, properties: info.into() };
    let bytes = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj)).map_err(|_| "pod")?.0.into_inner();
    let mut params = [Pod::from_bytes(&bytes).ok_or("bad pod")?];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("PipeWire: couldn't capture sound: {e}"))?;
    logi!("sound capture: what this computer plays, 48 kHz stereo");
    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |_| ml.quit());
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}
