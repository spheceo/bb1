//! The main window as the shared React UI (apps/ui) in WebView2, the same page the Linux and Mac
//! apps show. apps/ui/src/types.ts is the contract: this module sends the whole state, the page
//! sends commands.
//!
//! - The page is embedded in bb1.exe and served as http://bb1.localhost/ (wry's custom protocol on
//!   Windows), computers' screenshots at /preview/<name>.
//! - The window is a plain Win32 window ("bb1Home", like the native one) with the web view filling
//!   it. It's still the app's event window (WM_APP_EVENT) and hides instead of closing.
//! - Without WebView2 (an old Windows 10 that never got it), `create` returns None and connect.rs
//!   draws the window itself, as before.
//! - WebView2's data (cache, local storage) lives in %LOCALAPPDATA%\bb1\WebView2: next to bb1.exe
//!   (Program Files) isn't writable.

use crate::core::client::{Auth, Target};
use crate::core::directory::{self, Directory};
use crate::core::host::Access;
use crate::core::store::{self, KnownHost};
use crate::core::updater::{Pill, Updater};
use crate::logi;
use crate::win::app::{self, UiEvent};
use crate::win::{connect, panels, tray};
use crossbeam_channel::Sender;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::cell::RefCell;
use std::num::NonZeroIsize;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};

#[cfg(bb1_ui)]
const PAGE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/dist/index.html"));
#[cfg(not(bb1_ui))]
const PAGE: &str = "";

/// Send the state to the page (posted, so it never runs inside a WebView2 callback).
const WM_APP_PUSH: u32 = WM_APP + 60;
const TIMER_TICK: usize = 1;
/// Hides "Updated to …" a few seconds after it's been seen.
const TIMER_DONE: usize = 2;

struct Web {
    hwnd: HWND,
    webview: wry::WebView,
    _context: wry::WebContext,
    directory: Directory,
    updater: Updater,
    /// The connection's last status line, and whether it's connected.
    status: String,
    connected: bool,
    /// Why the last connection ended (shown until dismissed).
    ended: Option<String>,
    /// The pairing code the connection waits for: the host's name, and where the code goes.
    asking: Option<(String, Sender<Option<String>>)>,
    done_timer: bool,
}

thread_local! {
    static WEB: RefCell<Option<Web>> = const { RefCell::new(None) };
}

// Win32 and WebView2 calls can re-enter this module: closures passed to `with` only touch fields.
fn with<R>(f: impl FnOnce(&mut Web) -> R) -> Option<R> {
    WEB.with(|w| w.try_borrow_mut().ok().and_then(|mut w| w.as_mut().map(f)))
}

/// The web UI is the main window (else connect.rs draws it).
pub fn active() -> bool {
    WEB.with(|w| w.borrow().is_some())
}

struct Parent(HWND);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = NonZeroIsize::new(self.0.0 as isize).ok_or(HandleError::Unavailable)?;
        // The window outlives the web view (it's never destroyed while bb1 runs).
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(handle))) })
    }
}

/// Creates the main window with the web UI. None: no page built in, or WebView2 isn't available.
pub fn create() -> Option<HWND> {
    if PAGE.is_empty() || std::env::var_os("BB1_NATIVE_WINDOW").is_some() {
        return None;
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), windows::core::PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::CreateSolidBrush(COLORREF(0x000a0909)),
            lpszClassName: w!("bb1Home"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("bb1Home"),
            w!("bb1"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1200,
            780,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        crate::win::gfx::dark_title_bar(hwnd);
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let w = crate::win::util::scale(hwnd, 1220).min(ww * 9 / 10);
        let h = crate::win::util::scale(hwnd, 760).min(wh * 9 / 10);
        let _ = SetWindowPos(hwnd, None, work.left + (ww - w) / 2, work.top + (wh - h) / 2, w, h, SWP_NOZORDER);

        let directory = Directory::start(|| app::post(UiEvent::HostsChanged));
        let updater = Updater::start(|| app::post(UiEvent::HostsChanged));
        let data = store::log_dir().parent().map(|p| p.join("WebView2")).unwrap_or_else(|| std::env::temp_dir().join("bb1-WebView2"));
        let mut context = wry::WebContext::new(Some(data));
        let previews = directory.clone();
        let built = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_background_color((9, 9, 10, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_custom_protocol("bb1".into(), move |_id, request| {
                let path = request.uri().path().to_string();
                if let Some(name) = path.strip_prefix("/preview/") {
                    if let Some(p) = previews.preview(&percent_decode(name)) {
                        return Response::builder().header(CONTENT_TYPE, "image/jpeg").header("Cache-Control", "max-age=31536000").body(Cow::Owned(p.jpeg.to_vec())).unwrap();
                    }
                    return Response::builder().status(404).body(Cow::Borrowed(&b""[..])).unwrap();
                }
                Response::builder().header(CONTENT_TYPE, "text/html; charset=utf-8").body(Cow::Borrowed(PAGE.as_bytes())).unwrap()
            })
            .with_ipc_handler(move |req| {
                let body: &String = req.body();
                match serde_json::from_str::<Value>(body) {
                    Ok(cmd) => command(&cmd),
                    Err(e) => logi!("ui: bad message from the page: {e}"),
                }
            })
            .with_url(std::env::var("BB1_UI_URL").unwrap_or_else(|_| "http://bb1.localhost/".into()))
            .build(&Parent(hwnd));
        let webview = match built {
            Ok(v) => v,
            Err(e) => {
                logi!("ui: no WebView2 ({e}); drawing the window natively");
                let _ = DestroyWindow(hwnd);
                return None;
            }
        };
        app::set_event_window(hwnd);
        WEB.with(|cell| {
            *cell.borrow_mut() = Some(Web {
                hwnd,
                webview,
                _context: context,
                directory,
                updater,
                status: String::new(),
                connected: false,
                ended: None,
                asking: None,
                done_timer: false,
            })
        });
        SetTimer(Some(hwnd), TIMER_TICK, 1000, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        logi!("ui: the window is the web UI (WebView2)");
        Some(hwnd)
    }
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

fn post_push() {
    if let Some(hwnd) = with(|w| w.hwnd) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_PUSH, WPARAM(0), LPARAM(0));
        }
    }
}

// MARK: What connect.rs's callers ask of the window (app.rs, tray.rs)

pub fn refresh() {
    post_push();
}

pub fn set_status(text: &str) {
    with(|w| w.status = text.to_string());
    post_push();
}

pub fn set_connected(on: bool) {
    with(|w| w.connected = on);
}

pub fn hide() {
    let Some((hwnd, dir)) = with(|w| (w.hwnd, w.directory.clone())) else { return };
    dir.set_active(false);
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn show(message: &str) {
    let Some((hwnd, dir, updater)) = with(|w| {
        w.ended = (!message.is_empty()).then(|| message.to_string());
        w.status.clear();
        w.connected = false;
        (w.hwnd, w.directory.clone(), w.updater.clone())
    }) else {
        return;
    };
    dir.set_active(true);
    dir.refresh_now();
    updater.check_soon();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    post_push();
}

/// On top of whatever's in front (a pairing code must be seen).
pub fn bring_forward() {
    let Some((hwnd, dir)) = with(|w| (w.hwnd, w.directory.clone())) else { return };
    dir.set_active(true);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
    }
    post_push();
}

/// The Mac shows a 6-digit code: the window comes forward with the code field.
pub fn ask_code(host: &str, reply: Sender<Option<String>>) {
    if let Some(Some((_, old))) = with(|w| w.asking.replace((host.to_string(), reply))) {
        let _ = old.send(None);
    }
    bring_forward();
}

// MARK: Window

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        app::WM_APP_EVENT => {
            app::drain_events();
            LRESULT(0)
        }
        WM_APP_PUSH => {
            push();
            LRESULT(0)
        }
        WM_APP_LATER => {
            run_queued();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_TICK => {
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                push();
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_DONE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_DONE);
            }
            if let Some(u) = with(|w| {
                w.done_timer = false;
                w.updater.clone()
            }) {
                u.dismiss_done();
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: crate::win::util::scale(hwnd, 480), y: crate::win::util::scale(hwnd, 420) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let rc = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER);
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Keyboard focus into the page when the window comes forward.
            if wparam.0 & 0xFFFF != 0 {
                let _ = with(|w| w.webview.focus());
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // bb1 keeps running by the clock (sharing, updates); Quit is in the icon's menu.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            if let Some(d) = with(|w| w.directory.clone()) {
                d.set_active(false);
            }
            tray::told_still_running();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn push() {
    let Some(s) = state() else { return };
    let script = format!("window.bb1Receive && window.bb1Receive({{type:'state',state:{s}}})");
    with(|w| {
        let _ = w.webview.evaluate_script(&script);
    });
}

/// The state the page shows (apps/ui/src/types.ts, State).
fn state() -> Option<Value> {
    let (dir, updater, status, connected, ended, asking, hwnd, done_timer) =
        with(|w| (w.directory.clone(), w.updater.clone(), w.status.clone(), w.connected, w.ended.clone(), w.asking.as_ref().map(|a| a.0.clone()), w.hwnd, w.done_timer))?;
    let settings = app::settings();
    let computers: Vec<Value> = dir
        .entries()
        .into_iter()
        .map(|e| {
            let preview = dir.preview(&e.name);
            let pairing = e.online && !e.quiet && e.address.as_deref().is_some_and(|a| matches!(Target::new(a, &e.name).auth, Auth::Code));
            let known = e.address.as_deref().and_then(|a| KnownHost::find(a, &e.name)).or_else(|| KnownHost::find("", &e.name));
            json!({
                "name": e.name,
                "address": e.address,
                "online": e.online,
                "quiet": !pairing,
                "latencyMs": dir.latency_ms(&e.name),
                "previewVersion": preview.as_ref().map(|p| p.version),
                "previewAgo": preview.as_ref().filter(|_| !e.online).map(|p| directory::ago(p.at)),
                "canForget": known.is_some(),
            })
        })
        .collect();
    let behind: Vec<String> = connect::macs_behind(&dir, &updater).into_iter().map(|(t, _)| t.label).collect();
    let pill = match updater.pill(&behind) {
        Pill::Hidden => Value::Null,
        Pill::Offer { label, also } => json!({ "kind": "offer", "label": label, "also": also }),
        Pill::Busy { label, progress } => json!({ "kind": "busy", "label": label, "progress": progress }),
        Pill::Done(label) => {
            if !done_timer {
                with(|w| w.done_timer = true);
                unsafe {
                    SetTimer(Some(hwnd), TIMER_DONE, 6000, None);
                }
            }
            json!({ "kind": "done", "label": label })
        }
        Pill::Failed(why) => {
            logi!("ui: update failed: {why}");
            json!({ "kind": "failed", "label": "Couldn't update · try again" })
        }
    };
    let update_status = match updater.latest() {
        Some(r) if r.build > crate::core::updater::BUILD => format!("{} is ready", r.short_name),
        Some(_) => "Up to date".to_string(),
        None => "Checks every few minutes".to_string(),
    };
    let sharing_state = if tray::is_sharing() {
        "on"
    } else if tray::problem().is_some() {
        "failed"
    } else {
        "off"
    };
    let viewers: Vec<Value> = tray::viewers()
        .into_iter()
        .map(|v| json!({ "id": v.id, "name": v.name, "mouse": v.mouse, "keyboard": v.keyboard, "controller": v.controller }))
        .collect();
    let muted: Vec<Value> = crate::win::apps::known_apps()
        .into_iter()
        .filter(|a| a.path.is_some() || settings.muted_apps.iter().any(|m| m == a.id))
        .map(|a| json!({ "id": a.id, "name": a.name, "muted": settings.muted_apps.iter().any(|m| m == a.id) }))
        .collect();
    let (driver_state, driver_error) = panels::driver_status();
    let stream = app::client().map(|c| {
        json!({
            "label": c.label(),
            "status": if status.is_empty() { "Connecting…".to_string() } else { status.clone() },
            "connected": connected,
            "askingCode": asking,
        })
    });
    Some(json!({
        "platform": "windows",
        "me": store::device_name(),
        "build": crate::core::updater::build_name(crate::core::updater::BUILD),
        "tailscaleRunning": dir.tailscale_running(),
        "computers": computers,
        "stream": stream,
        "lastEnded": ended,
        "sharing": {
            "state": sharing_state,
            "error": tray::problem(),
            "note": "Needs an NVIDIA graphics card.",
            "pairing": tray::pairing().map(|(d, c)| json!({ "device": d, "code": c })),
            "viewers": viewers,
        },
        "settings": {
            "sound": settings.sound,
            "clipboard": settings.clipboard,
            "modifierLayout": settings.modifier_layout,
            "allowTailscale": settings.allow_tailscale,
            "allowPairing": settings.allow_pairing,
            "sharing": settings.sharing,
            "nightly": settings.nightly,
        },
        "logDir": store::log_dir().display().to_string(),
        "update": pill,
        "updateStatus": { "text": update_status, "canCheck": true },
        "pairedCount": crate::core::host::paired_device_count(),
        "mutedApps": muted,
        "controllerDriver": { "state": driver_state, "error": driver_error },
    }))
}

/// One command from the page (apps/ui/src/types.ts, Command). Runs inside a WebView2 callback, so
/// anything that opens windows or pumps messages is posted to run afterwards.
fn command(c: &Value) {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |k: &str| c.get(k).and_then(Value::as_bool).unwrap_or(false);
    let id = || c.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
    match c.get("cmd").and_then(Value::as_str).unwrap_or_default() {
        "ready" => {
            if let Some(d) = with(|w| w.directory.clone()) {
                d.set_active(true);
            }
        }
        "connect" => {
            let (address, label) = (s("address"), s("label"));
            with(|w| w.ended = None);
            run_later(move || app::connect_to(&address, &label));
        }
        "disconnect" => run_later(app::disconnect),
        "sendCode" => {
            let code: String = s("code").chars().filter(|c| c.is_ascii_digit()).collect();
            if let Some(Some((_, reply))) = with(|w| w.asking.take()) {
                let _ = reply.send((!code.is_empty()).then_some(code));
            }
            // Back to the stream while it's open.
            if crate::win::stream::hwnd().is_some() {
                run_later(hide);
            }
        }
        "setSharing" => {
            let on = b("on");
            run_later(move || tray::share(on));
        }
        "setViewerAccess" => {
            let what = match s("what").as_str() {
                "mouse" => Access::Mouse,
                "keyboard" => Access::Keyboard,
                _ => Access::Controller,
            };
            tray::set_access(id(), what, b("on"));
        }
        "disconnectViewer" => tray::disconnect(id()),
        "cancelPairing" => tray::cancel_pairing(),
        "setSettings" => {
            let Some(new) = c.get("settings").cloned() else { return };
            app::update_settings(|st| {
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
            });
            tray::settings_changed();
            if new.get("nightly").is_some()
                && let Some(u) = with(|w| w.updater.clone())
            {
                u.check_soon();
            }
        }
        "setMuted" => {
            let (app_id, on) = (s("id"), b("muted"));
            app::update_settings(|st| {
                st.muted_apps.retain(|m| *m != app_id);
                if on {
                    st.muted_apps.push(app_id.clone());
                }
            });
            tray::settings_changed();
        }
        "forgetPaired" => crate::core::host::forget_paired_devices(),
        "forgetComputer" => {
            let name = s("name");
            if let Some(k) = KnownHost::find("", &name) {
                KnownHost::forget(&k.host_id);
                logi!("ui: forgot the pairing with {}", k.name);
            }
            if let Some(d) = with(|w| w.directory.clone()) {
                d.refresh_now();
            }
        }
        "installControllerDriver" => panels::install_controller_driver(),
        "openLogs" => panels::open_folder(&store::log_dir()),
        "dismissEnded" => {
            with(|w| w.ended = None);
        }
        "update" => run_later(start_update),
        "dismissUpdate" => {
            if let Some(u) = with(|w| w.updater.clone()) {
                u.dismiss_done();
            }
        }
        "checkUpdates" => {
            if let Some(u) = with(|w| w.updater.clone()) {
                u.check_soon();
            }
        }
        other => logi!("ui: unknown command {other}"),
    }
    post_push();
}

/// Runs `f` on the UI thread after the current WebView2 callback (as a queued UI event).
fn run_later(f: impl FnOnce() + 'static) {
    LATER.with(|l| l.borrow_mut().push(Box::new(f)));
    if let Some(hwnd) = with(|w| w.hwnd) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_LATER, WPARAM(0), LPARAM(0));
        }
    }
}

const WM_APP_LATER: u32 = WM_APP + 61;

thread_local! {
    static LATER: RefCell<Vec<Box<dyn FnOnce()>>> = RefCell::new(Vec::new());
}

/// Called by the window procedure for WM_APP_LATER.
fn run_queued() {
    let jobs: Vec<Box<dyn FnOnce()>> = LATER.with(|l| std::mem::take(&mut *l.borrow_mut()));
    for job in jobs {
        job();
    }
}

/// The update button: update the Macs that are behind, then this PC (as connect.rs does).
fn start_update() {
    let Some((dir, updater)) = with(|w| (w.directory.clone(), w.updater.clone())) else { return };
    let macs = connect::macs_behind(&dir, &updater);
    updater.update(
        macs,
        |installer| {
            let r = connect::launch_installer(installer);
            if r.is_ok() {
                // The installer closes bb1 too; quit first so it doesn't have to.
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            }
            r
        },
        crate::win::selfupdate::quiet_install(),
    );
    post_push();
}
