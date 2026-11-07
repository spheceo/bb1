//! The shared screen's frames, from the PipeWire stream the portal opened (portal.rs).
//!
//! A thread runs a PipeWire loop on the portal's socket and copies each new frame (BGRx or RGBx in
//! shared memory) into `Shared`. The desktop sends frames only when something on the screen changes.
//! With the pointer as metadata (portal CURSOR_METADATA) its position and picture arrive beside the
//! frames, sometimes without a new frame; `compose` draws it in when a viewer wants it in the video.

use crate::logi;
use pipewire as pw;
use pw::spa;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Frame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
    /// Byte order R, G, B (RGBx/RGBA) instead of B, G, R.
    rgb: bool,
    seq: u64,
}

#[derive(Default, Clone)]
struct Pointer {
    visible: bool,
    x: i32,
    y: i32,
    hot_x: i32,
    hot_y: i32,
    width: usize,
    height: usize,
    /// RGBA, premultiplied.
    pixels: Arc<Vec<u8>>,
    seq: u64,
}

#[derive(Default)]
struct State {
    frame: Frame,
    pointer: Pointer,
    error: Option<String>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// A picture ready to encode.
#[derive(Default)]
pub struct Picture {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub rgb: bool,
}

pub struct Capture {
    shared: Arc<Shared>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    /// What was last composed (frame seq, pointer seq), and whether the pointer was drawn.
    composed: (u64, u64, bool),
    pub picture: Picture,
    pub draw_pointer: bool,
}

impl Capture {
    /// Starts receiving frames from `node` on the portal's PipeWire socket. Waits (at most 5 s) for
    /// the first frame, so the size is known.
    pub fn start(fd: OwnedFd, node: u32) -> Result<Capture, String> {
        let shared = Arc::new(Shared::default());
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("bb1 screen".into())
            .spawn(move || {
                if let Err(e) = run(fd, node, s.clone(), quit_rx, ready_tx.clone()) {
                    let _ = ready_tx.send(Err(e.clone()));
                    s.state.lock().unwrap().error = Some(e);
                    s.changed.notify_all();
                }
            })
            .map_err(|e| e.to_string())?;
        let mut capture = Capture { shared, quit, thread: Some(thread), composed: (0, 0, false), picture: Picture::default(), draw_pointer: true };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("the screen's stream didn't start".into()),
        }
        // The first frame (the desktop sends one as soon as the stream runs).
        let until = Instant::now() + Duration::from_secs(5);
        while capture.size() == (0, 0) {
            if Instant::now() > until {
                return Err("no picture from the screen".into());
            }
            let _ = capture.wait(Duration::from_millis(100));
        }
        Ok(capture)
    }

    pub fn size(&self) -> (u32, u32) {
        let st = self.shared.state.lock().unwrap();
        (st.frame.width, st.frame.height)
    }

    /// Waits up to `timeout` for a new frame (or the pointer moving, when it's drawn in), and then
    /// composes `picture`. Ok(true): `picture` changed.
    pub fn wait(&mut self, timeout: Duration) -> Result<bool, String> {
        let until = Instant::now() + timeout;
        let mut st = self.shared.state.lock().unwrap();
        loop {
            if let Some(e) = &st.error {
                return Err(e.clone());
            }
            let pointer_changed = self.draw_pointer && st.pointer.seq != self.composed.1;
            let toggled = self.draw_pointer != self.composed.2;
            if st.frame.seq != 0 && (st.frame.seq != self.composed.0 || pointer_changed || toggled) {
                break;
            }
            let now = Instant::now();
            if now >= until {
                return Ok(false);
            }
            st = self.shared.changed.wait_timeout(st, until - now).unwrap().0;
        }
        let f = &st.frame;
        let p = &mut self.picture;
        p.data.resize(f.data.len(), 0);
        p.data.copy_from_slice(&f.data);
        (p.width, p.height, p.stride, p.rgb) = (f.width, f.height, f.stride, f.rgb);
        let pointer = st.pointer.clone();
        self.composed = (f.seq, pointer.seq, self.draw_pointer);
        drop(st);
        if self.draw_pointer && pointer.visible {
            draw(&mut self.picture, &pointer);
        }
        Ok(true)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Blends the pointer (premultiplied RGBA) onto the picture.
fn draw(p: &mut Picture, c: &Pointer) {
    let (x0, y0) = (c.x - c.hot_x, c.y - c.hot_y);
    for row in 0..c.height {
        let y = y0 + row as i32;
        if y < 0 || y >= p.height as i32 {
            continue;
        }
        for col in 0..c.width {
            let x = x0 + col as i32;
            if x < 0 || x >= p.width as i32 {
                continue;
            }
            let s = &c.pixels[(row * c.width + col) * 4..][..4];
            let a = s[3] as u32;
            if a == 0 {
                continue;
            }
            let d = &mut p.data[y as usize * p.stride + x as usize * 4..][..4];
            let (r, b) = if p.rgb { (0, 2) } else { (2, 0) };
            let inv = 255 - a;
            d[r] = (s[0] as u32 + d[r] as u32 * inv / 255).min(255) as u8;
            d[1] = (s[1] as u32 + d[1] as u32 * inv / 255).min(255) as u8;
            d[b] = (s[2] as u32 + d[b] as u32 * inv / 255).min(255) as u8;
        }
    }
}

struct UserData {
    shared: Arc<Shared>,
    format: VideoInfoRaw,
    ready: Option<std::sync::mpsc::Sender<Result<(), String>>>,
    frames: u64,
}

fn pod_bytes(v: Value) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &v).expect("pod").0.into_inner()
}

/// Room for a pointer picture of `side`×`side` pixels in the cursor metadata.
fn cursor_meta_size(side: i32) -> i32 {
    (std::mem::size_of::<spa::sys::spa_meta_cursor>() + std::mem::size_of::<spa::sys::spa_meta_bitmap>()) as i32 + side * side * 4
}

fn run(
    fd: OwnedFd,
    node: u32,
    shared: Arc<Shared>,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("PipeWire: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| format!("PipeWire: {e}"))?;
    let core = context.connect_fd_rc(fd, None).map_err(|e| format!("PipeWire: couldn't connect to the screen's stream: {e}"))?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "bb1-screen",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| format!("PipeWire stream: {e}"))?;

    let data = UserData { shared: shared.clone(), format: VideoInfoRaw::default(), ready: Some(ready), frames: 0 };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, ud, old, new| {
            logi!("capture: stream {old:?} → {new:?}");
            match new {
                pw::stream::StreamState::Streaming => {
                    if let Some(r) = ud.ready.take() {
                        let _ = r.send(Ok(()));
                    }
                }
                pw::stream::StreamState::Error(e) => {
                    let e = format!("the screen's stream failed: {e}");
                    if let Some(r) = ud.ready.take() {
                        let _ = r.send(Err(e.clone()));
                    }
                    ud.shared.state.lock().unwrap().error = Some(e);
                    ud.shared.changed.notify_all();
                }
                pw::stream::StreamState::Unconnected => {
                    ud.shared.state.lock().unwrap().error = Some("the screen's stream ended (sharing was stopped on this computer)".into());
                    ud.shared.changed.notify_all();
                }
                _ => {}
            }
        })
        .param_changed(|stream, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if ud.format.parse(param).is_err() {
                return;
            }
            let size = ud.format.size();
            logi!("capture: {}x{} {:?}", size.width, size.height, ud.format.format());
            // Ask for the pointer as metadata (position and picture).
            let meta = pod_bytes(Value::Object(Object {
                type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
                id: spa::sys::SPA_PARAM_Meta,
                properties: vec![
                    Property::new(spa::sys::SPA_PARAM_META_type, Value::Id(Id(spa::sys::SPA_META_Cursor))),
                    Property::new(
                        spa::sys::SPA_PARAM_META_size,
                        Value::Choice(ChoiceValue::Int(Choice(
                            ChoiceFlags::empty(),
                            ChoiceEnum::Range { default: cursor_meta_size(64), min: cursor_meta_size(1), max: cursor_meta_size(512) },
                        ))),
                    ),
                ],
            }));
            if let Some(pod) = Pod::from_bytes(&meta) {
                let _ = stream.update_params(&mut [pod]);
            }
        })
        .process(|stream, ud| {
            // Only the newest buffer matters: older ones go straight back.
            let mut newest = None;
            while let Some(b) = stream.dequeue_buffer() {
                newest = Some(b);
            }
            let Some(mut buffer) = newest else { return };
            let mut pointer_update: Option<Pointer> = None;
            if let Some(c) = buffer.find_meta::<spa::buffer::meta::MetaCursor>() {
                let mut p = ud.shared.state.lock().unwrap().pointer.clone();
                if c.is_valid() {
                    p.visible = true;
                    (p.x, p.y) = (c.position().x, c.position().y);
                    if let Some(bm) = c.bitmap() {
                        let size = bm.size();
                        if size.width > 0 && size.height > 0 && let Some(bytes) = bm.bitmap_data() {
                            (p.hot_x, p.hot_y) = (c.hotspot().x, c.hotspot().y);
                            p.width = size.width as usize;
                            p.height = size.height as usize;
                            p.pixels = Arc::new(to_rgba(bytes, size.width as usize, size.height as usize, bm.stride().unsigned_abs() as usize, bm.format()));
                        }
                    }
                } else {
                    p.visible = false;
                }
                pointer_update = Some(p);
            }
            let format = ud.format.format();
            let size = ud.format.size();
            let datas = buffer.datas_mut();
            let mut st = ud.shared.state.lock().unwrap();
            let mut changed = false;
            if let Some(mut p) = pointer_update {
                p.seq = st.pointer.seq + 1;
                st.pointer = p;
                changed = true;
            }
            if let Some(d) = datas.first_mut() {
                let chunk_size = d.chunk().size() as usize;
                let offset = d.chunk().offset() as usize;
                let stride = match d.chunk().stride() {
                    s if s > 0 => s as usize,
                    _ => size.width as usize * 4,
                };
                let corrupted = d.chunk().flags().contains(spa::buffer::ChunkFlags::CORRUPTED);
                let need = stride * size.height as usize;
                if chunk_size > 0 && !corrupted && let Some(bytes) = d.data() && bytes.len() >= offset + need {
                    // Into the same buffer every time (no allocation per frame).
                    ud.frames += 1;
                    let f = &mut st.frame;
                    f.data.resize(need, 0);
                    f.data.copy_from_slice(&bytes[offset..offset + need]);
                    f.stride = stride;
                    f.width = size.width;
                    f.height = size.height;
                    f.rgb = matches!(format, VideoFormat::RGBx | VideoFormat::RGBA);
                    f.seq += 1;
                    changed = true;
                }
            }
            drop(st);
            if changed {
                ud.shared.changed.notify_all();
            }
        })
        .register()
        .map_err(|e| format!("PipeWire: {e}"))?;

    let format = pod_bytes(Value::Object(spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Video),
        spa::pod::property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Raw),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 360, denom: 1 }
        ),
    )));
    let mut params = [Pod::from_bytes(&format).ok_or("bad format pod")?];
    stream
        .connect(spa::utils::Direction::Input, Some(node), pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS, &mut params)
        .map_err(|e| format!("PipeWire: couldn't connect to the screen: {e}"))?;

    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |_| ml.quit());
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// A pointer picture in any 32-bit format → premultiplied RGBA, tightly packed.
fn to_rgba(src: &[u8], w: usize, h: usize, stride: usize, format: VideoFormat) -> Vec<u8> {
    let stride = if stride == 0 { w * 4 } else { stride };
    let (r, g, b, a) = match format {
        VideoFormat::BGRA => (2, 1, 0, 3),
        VideoFormat::ARGB => (1, 2, 3, 0),
        VideoFormat::ABGR => (3, 2, 1, 0),
        _ => (0, 1, 2, 3), // RGBA
    };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let Some(s) = src.get(y * stride + x * 4..y * stride + x * 4 + 4) else { continue };
            let o = &mut out[(y * w + x) * 4..][..4];
            o.copy_from_slice(&[s[r], s[g], s[b], s[a]]);
        }
    }
    out
}
