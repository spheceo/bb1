//! The main window: the shared React UI (apps/ui) in a web view (wry on WebKitGTK, in a tao window),
//! and its bridge to the app (apps/ui/src/types.ts is the contract).
//!
//! - The page is embedded in the binary (apps/ui/dist/index.html) and served as bb1://localhost/,
//!   with computers' screenshots at bb1://localhost/preview/<name>. BB1_UI_URL=http://localhost:5173
//!   loads the UI's dev server instead (cd apps/ui && bun run dev), for working on it live.
//! - App → page: the whole state as JSON, through window.bb1Receive, whenever something changes and
//!   once a second (latencies, viewers).
//! - Page → app: one JSON command per message (window.ipc.postMessage).
//!
//! The stream runs in its own process (stream.rs), which reports over its stdout and gets pairing
//! codes on its stdin.

use crate::core::directory::Directory;
use crate::core::host::{Access, HostEvents};
use crate::core::store::{self, Settings};
use crate::linux::sharing::Sharing;
use crate::logi;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Theme, WindowBuilder};
use wry::WebViewBuilder;
use wry::http::{Response, header::CONTENT_TYPE};

#[cfg(bb1_ui)]
const PAGE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/dist/index.html"));
#[cfg(not(bb1_ui))]
const PAGE: &str = "<!doctype html><html><body style='background:#09090a;color:#eee;font:15px system-ui;padding:40px'>\
<h2>bb1's window isn't built</h2><p>Build the UI, then bb1 again:</p>\
<pre>cd apps/ui &amp;&amp; bun install &amp;&amp; bun run build\ncd ../windows &amp;&amp; cargo build --release</pre></body></html>";

#[derive(Debug)]
enum UserEvent {
    /// Something changed: send the state to the page.
    Push,
}

/// The stream window's process, and what it last said.
struct StreamProc {
    label: String,
    child: Child,
    stdin: Option<ChildStdin>,
    status: String,
    connected: bool,
    asking_code: Option<String>,
}

enum Share {
    Off,
    Starting,
    On(Sharing),
    Failed(String),
}

struct Model {
    settings: Settings,
    stream: Option<StreamProc>,
    last_ended: Option<String>,
    share: Share,
    pairing: Option<(String, String)>,
    me: String,
}

type Shared = Arc<Mutex<Model>>;

struct UiHostEvents {
    model: Shared,
    proxy: EventLoopProxy<UserEvent>,
}

impl HostEvents for UiHostEvents {
    fn pairing_code(&self, device: &str, code: &str) {
        self.model.lock().unwrap().pairing = Some((device.to_string(), code.to_string()));
        crate::linux::app::notify(&format!("{device} wants to connect"), &format!("Type this code on it: {code}"));
        let _ = self.proxy.send_event(UserEvent::Push);
    }
    fn pairing_finished(&self, _paired: bool) {
        self.model.lock().unwrap().pairing = None;
        let _ = self.proxy.send_event(UserEvent::Push);
    }
    fn viewers_changed(&self, _names: &[String]) {
        let _ = self.proxy.send_event(UserEvent::Push);
    }
}

pub fn run() {
    // WebKitGTK's DMA-BUF renderer shows a blank or flickering window with NVIDIA's driver.
    if std::path::Path::new("/proc/driver/nvidia/version").exists() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("bb1")
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 760.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(480.0, 420.0))
        .with_theme(Some(Theme::Dark))
        .with_window_icon(icon())
        .build(&event_loop)
        .expect("couldn't open bb1's window");

    dark_title_bar();

    let settings = Settings::load();
    let start_sharing = settings.sharing;
    let model: Shared = Arc::new(Mutex::new(Model {
        settings,
        stream: None,
        last_ended: None,
        share: Share::Off,
        pairing: None,
        me: store::device_name(),
    }));
    let p = proxy.clone();
    let directory = Directory::start(move || {
        let _ = p.send_event(UserEvent::Push);
    });

    let previews = directory.clone();
    let (m, p) = (model.clone(), proxy.clone());
    let dir = directory.clone();
    let mut builder = WebViewBuilder::new()
        .with_background_color((9, 9, 10, 255))
        .with_devtools(cfg!(debug_assertions))
        .with_custom_protocol("bb1".into(), move |_id, request| {
            let path = request.uri().path().to_string();
            if let Some(name) = path.strip_prefix("/preview/") {
                let name = percent_decode(name);
                if let Some(p) = previews.preview(&name) {
                    return Response::builder().header(CONTENT_TYPE, "image/jpeg").header("Cache-Control", "max-age=31536000").body(Cow::Owned(p.jpeg.to_vec())).unwrap();
                }
                return Response::builder().status(404).body(Cow::Borrowed(&b""[..])).unwrap();
            }
            Response::builder().header(CONTENT_TYPE, "text/html; charset=utf-8").body(Cow::Borrowed(PAGE.as_bytes())).unwrap()
        })
        .with_ipc_handler(move |req| {
            let body: &String = req.body();
            match serde_json::from_str::<Value>(body) {
                Ok(cmd) => command(&m, &dir, &p, &cmd),
                Err(e) => logi!("ui: bad message from the page: {e}"),
            }
        });
    builder = match std::env::var("BB1_UI_URL") {
        Ok(url) => builder.with_url(url),
        Err(_) => builder.with_url("bb1://localhost/"),
    };
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("no GTK box")).expect("couldn't open the web view")
    };

    if start_sharing {
        start_sharing_now(&model, &proxy);
    }
    let mut last_push = Instant::now() - Duration::from_secs(10);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(1));
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                let mut m = model.lock().unwrap();
                disconnect(&mut m);
                if let Share::On(s) = std::mem::replace(&mut m.share, Share::Off) {
                    s.stop();
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::Focused(on), .. } => directory.set_active(on),
            Event::UserEvent(UserEvent::Push) => {
                push(&webview, &model, &directory);
                last_push = Instant::now();
            }
            Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. }) if last_push.elapsed() >= Duration::from_millis(900) => {
                push(&webview, &model, &directory);
                last_push = Instant::now();
            }
            _ => {}
        }
    });
}

/// The window's title bar (GTK's header bar) in the page's background colour, so the window is one
/// piece of graphite.
fn dark_title_bar() {
    use gtk::prelude::*;
    let css = gtk::CssProvider::new();
    let style = "headerbar, .titlebar, headerbar:backdrop, .titlebar:backdrop { \
        background: #09090a; background-image: none; border: none; box-shadow: none; color: #f2f3f5; } \
        headerbar .title { color: #f2f3f5; font-weight: 600; } \
        headerbar:backdrop .title { color: rgba(255, 255, 255, 0.5); } \
        window.csd, window.csd decoration { background: #09090a; }";
    if css.load_from_data(style.as_bytes()).is_ok()
        && let Some(screen) = gtk::gdk::Screen::default()
    {
        gtk::StyleContext::add_provider_for_screen(&screen, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn icon() -> Option<Icon> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(&include_bytes!("../../res/bb1.png")[..])).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    buf.truncate(info.width as usize * info.height as usize * 4);
    Icon::from_rgba(buf, info.width, info.height).ok()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The state the page shows (apps/ui/src/types.ts, State).
fn state(model: &Shared, directory: &Directory) -> Value {
    let m = model.lock().unwrap();
    let computers: Vec<Value> = directory
        .entries()
        .into_iter()
        .map(|e| {
            let preview = directory.preview(&e.name);
            let known = e.address.as_deref().and_then(|a| store::KnownHost::find(a, &e.name)).or_else(|| store::KnownHost::find("", &e.name));
            json!({
                "name": e.name,
                "address": e.address,
                "online": e.online,
                "quiet": e.quiet,
                "latencyMs": directory.latency_ms(&e.name),
                "previewVersion": preview.as_ref().map(|p| p.version),
                "previewAgo": preview.as_ref().filter(|_| !e.online).map(|p| crate::core::directory::ago(p.at)),
                "canForget": known.is_some(),
            })
        })
        .collect();
    let (share_state, error, viewers) = match &m.share {
        Share::Off => ("off", None, vec![]),
        Share::Starting => ("starting", None, vec![]),
        Share::Failed(e) => ("failed", Some(e.clone()), vec![]),
        Share::On(s) => (
            "on",
            None,
            s.host()
                .viewers()
                .into_iter()
                .map(|v| json!({ "id": v.id, "name": v.name, "mouse": v.mouse, "keyboard": v.keyboard, "controller": v.controller }))
                .collect(),
        ),
    };
    let s = &m.settings;
    json!({
        "platform": "linux",
        "me": m.me,
        "build": crate::core::updater::build_name(crate::core::updater::BUILD),
        "tailscaleRunning": directory.tailscale_running(),
        "computers": computers,
        "stream": m.stream.as_ref().map(|st| json!({
            "label": st.label,
            "status": st.status,
            "connected": st.connected,
            "askingCode": st.asking_code,
        })),
        "lastEnded": m.last_ended,
        "sharing": {
            "state": share_state,
            "error": error,
            "pairing": m.pairing.as_ref().filter(|_| share_state == "on").map(|(d, c)| json!({ "device": d, "code": c })),
            "viewers": viewers,
        },
        "settings": {
            "sound": s.sound,
            "clipboard": s.clipboard,
            "modifierLayout": s.modifier_layout,
            "allowTailscale": s.allow_tailscale,
            "allowPairing": s.allow_pairing,
            "sharing": s.sharing,
            "nightly": s.nightly,
        },
        "logDir": store::log_dir().display().to_string(),
        "pairedCount": crate::core::host::paired_device_count(),
    })
}

fn push(webview: &wry::WebView, model: &Shared, directory: &Directory) {
    let s = state(model, directory);
    let _ = webview.evaluate_script(&format!("window.bb1Receive && window.bb1Receive({{type:'state',state:{s}}})"));
}

/// One command from the page (apps/ui/src/types.ts, Command).
fn command(model: &Shared, directory: &Directory, proxy: &EventLoopProxy<UserEvent>, c: &Value) {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |k: &str| c.get(k).and_then(Value::as_bool).unwrap_or(false);
    match c.get("cmd").and_then(Value::as_str).unwrap_or_default() {
        "ready" => directory.set_active(true),
        "connect" => connect(model, proxy, &s("address"), &s("label")),
        "disconnect" => disconnect(&mut model.lock().unwrap()),
        "sendCode" => {
            let code: String = s("code").chars().filter(|c| c.is_ascii_digit()).collect();
            let mut m = model.lock().unwrap();
            if let Some(st) = m.stream.as_mut() {
                if let Some(stdin) = st.stdin.as_mut() {
                    let _ = writeln!(stdin, "{code}");
                    let _ = stdin.flush();
                }
                st.asking_code = None;
            }
        }
        "setSharing" => {
            let on = b("on");
            {
                let mut m = model.lock().unwrap();
                m.settings.sharing = on;
                m.settings.save();
            }
            if on {
                start_sharing_now(model, proxy);
            } else {
                let old = std::mem::replace(&mut model.lock().unwrap().share, Share::Off);
                if let Share::On(s) = old {
                    s.stop();
                }
                model.lock().unwrap().pairing = None;
            }
        }
        "setViewerAccess" => {
            let id = c.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            let what = match s("what").as_str() {
                "mouse" => Access::Mouse,
                "keyboard" => Access::Keyboard,
                _ => Access::Controller,
            };
            if let Share::On(sh) = &model.lock().unwrap().share {
                sh.host().set_access(id, what, b("on"));
            }
        }
        "disconnectViewer" => {
            let id = c.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            if let Share::On(sh) = &model.lock().unwrap().share {
                sh.host().disconnect(id);
            }
        }
        "setSettings" => {
            let Some(new) = c.get("settings") else { return };
            let mut m = model.lock().unwrap();
            let st = &mut m.settings;
            let flag = |k: &str, v: &mut bool| {
                if let Some(x) = new.get(k).and_then(Value::as_bool) {
                    *v = x;
                }
            };
            flag("sound", &mut st.sound);
            flag("clipboard", &mut st.clipboard);
            flag("allowTailscale", &mut st.allow_tailscale);
            flag("allowPairing", &mut st.allow_pairing);
            flag("nightly", &mut st.nightly);
            if let Some(l) = new.get("modifierLayout").and_then(Value::as_str) {
                st.modifier_layout = l.to_string();
            }
            st.save();
            if let Share::On(sh) = &m.share {
                sh.apply_settings(&m.settings);
            }
        }
        "openLogs" => {
            let _ = Command::new("xdg-open").arg(store::log_dir()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
        "dismissEnded" => model.lock().unwrap().last_ended = None,
        "forgetPaired" => crate::core::host::forget_paired_devices(),
        "forgetComputer" => {
            if let Some(k) = store::KnownHost::find("", &s("name")) {
                store::KnownHost::forget(&k.host_id);
                logi!("ui: forgot the pairing with {}", k.name);
            }
            directory.refresh_now();
        }
        "cancelPairing" => {
            if let Share::On(sh) = &model.lock().unwrap().share {
                sh.host().cancel_pairing();
            }
            model.lock().unwrap().pairing = None;
        }
        other => logi!("ui: unknown command {other}"),
    }
    let _ = proxy.send_event(UserEvent::Push);
}

fn start_sharing_now(model: &Shared, proxy: &EventLoopProxy<UserEvent>) {
    model.lock().unwrap().share = Share::Starting;
    let (model, proxy) = (model.clone(), proxy.clone());
    std::thread::spawn(move || {
        let settings = model.lock().unwrap().settings.clone();
        let events = Arc::new(UiHostEvents { model: model.clone(), proxy: proxy.clone() });
        let result = Sharing::start(events, &settings, false);
        let mut m = model.lock().unwrap();
        match result {
            Ok(sharing) => {
                if matches!(m.share, Share::Starting) {
                    m.share = Share::On(sharing);
                } else {
                    sharing.stop(); // turned off while starting
                }
            }
            Err(e) => {
                logi!("host: couldn't share: {e}");
                if matches!(m.share, Share::Starting) {
                    m.share = Share::Failed(e);
                }
            }
        }
        drop(m);
        let _ = proxy.send_event(UserEvent::Push);
    });
}

fn connect(model: &Shared, proxy: &EventLoopProxy<UserEvent>, address: &str, label: &str) {
    let mut m = model.lock().unwrap();
    disconnect(&mut m);
    m.last_ended = None;
    let exe = std::env::current_exe().unwrap_or_else(|_| "bb1".into());
    let child = Command::new(exe).args(["--stream", address, "--label", label]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            m.last_ended = Some(format!("Couldn't start the stream window: {e}"));
            return;
        }
    };
    let shown = if label.is_empty() { store::host_of(address) } else { label.to_string() };
    let pid = child.id();
    if let Some(out) = child.stdout.take() {
        let (model, proxy) = (model.clone(), proxy.clone());
        std::thread::spawn(move || {
            let mut ended = None;
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                let Some(rest) = line.strip_prefix('@') else { continue };
                let (kind, text) = rest.split_once(' ').unwrap_or((rest, ""));
                let mut m = model.lock().unwrap();
                if let Some(st) = m.stream.as_mut().filter(|st| st.child.id() == pid) {
                    match kind {
                        "status" => {
                            st.status = text.to_string();
                            if !text.starts_with("Type the") {
                                st.asking_code = None;
                            }
                        }
                        "connected" => {
                            st.connected = true;
                            st.asking_code = None;
                            st.label = text.to_string();
                            st.status = "Connected".into();
                        }
                        "ask-code" => st.asking_code = Some(text.to_string()),
                        "paired" => st.status = format!("Paired with {text}"),
                        "notice" => st.status = text.to_string(),
                        "ended" => ended = Some(text.to_string()),
                        _ => {}
                    }
                }
                drop(m);
                let _ = proxy.send_event(UserEvent::Push);
            }
            // The stream window closed.
            let mut m = model.lock().unwrap();
            if m.stream.as_ref().is_some_and(|st| st.child.id() == pid) {
                if let Some(mut st) = m.stream.take() {
                    std::thread::spawn(move || {
                        let _ = st.child.wait();
                    });
                }
                let reason = ended.unwrap_or_else(|| "The stream window closed.".into());
                if reason != "Disconnected." {
                    m.last_ended = Some(reason);
                }
            }
            drop(m);
            let _ = proxy.send_event(UserEvent::Push);
        });
    }
    let stdin = child.stdin.take();
    m.stream = Some(StreamProc { label: shown, child, stdin, status: "Connecting…".into(), connected: false, asking_code: None });
    m.settings.last_address = Some(address.to_string());
    m.settings.save();
}

fn disconnect(m: &mut Model) {
    if let Some(mut st) = m.stream.take() {
        // SIGTERM: the stream window says goodbye to the host and closes.
        unsafe { kill(st.child.id() as i32, 15) };
        std::thread::spawn(move || {
            let _ = st.child.wait();
        });
    }
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
