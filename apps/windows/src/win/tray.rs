//! bb1's icon in the notification area by the clock, like the Mac's menu bar item. Click it to open
//! Your Macs; its menu shares this PC (host/server.rs), says who's watching, shows a pairing code,
//! and quits bb1. Closing Your Macs leaves bb1 running here.

use crate::core::host::{Access, HostEvents, Viewer};
use crate::host::server::Sharing;
use crate::logi;
use crate::win::app;
use crate::win::util::wide;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

#[derive(Default)]
struct View {
    viewers: Vec<String>,
    pairing: Option<(String, String)>,
    /// Why sharing isn't on, if it was asked for and failed.
    problem: Option<String>,
}

static VIEW: Mutex<Option<View>> = Mutex::new(None);
static SHARING: Mutex<Option<Sharing>> = Mutex::new(None);
static HWND_TRAY: AtomicIsize = AtomicIsize::new(0);

const WM_TRAY: u32 = WM_APP + 41;
const WM_REFRESH: u32 = WM_APP + 42;
const TRAY_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const CMD_OPEN: usize = 1;
const CMD_SHARE: usize = 2;
const CMD_DISCONNECT: usize = 3;
const CMD_CANCEL_PAIRING: usize = 4;
const CMD_QUIT: usize = 5;

fn view<R>(f: impl FnOnce(&mut View) -> R) -> R {
    f(VIEW.lock().unwrap().get_or_insert_with(View::default))
}

fn sharing() -> bool {
    SHARING.lock().unwrap().is_some()
}

// For the window (Your Macs' sharing panel and Settings).

pub fn is_sharing() -> bool {
    sharing()
}

/// Why sharing didn't start, if it didn't.
pub fn problem() -> Option<String> {
    view(|v| v.problem.clone())
}

pub fn pairing() -> Option<(String, String)> {
    view(|v| v.pairing.clone())
}

pub fn viewers() -> Vec<Viewer> {
    SHARING.lock().unwrap().as_ref().map(|s| s.host().viewers()).unwrap_or_default()
}

pub fn set_access(id: u32, what: Access, on: bool) {
    if let Some(s) = SHARING.lock().unwrap().as_ref() {
        s.host().set_access(id, what, on);
    }
}

pub fn disconnect(id: u32) {
    if let Some(s) = SHARING.lock().unwrap().as_ref() {
        s.host().disconnect(id);
    }
}

pub fn cancel_pairing() {
    if let Some(s) = SHARING.lock().unwrap().as_ref() {
        s.host().cancel_pairing();
    }
    view(|v| v.pairing = None);
    update_tip();
}

/// Turn sharing on or off (remembered), from the window or the menu.
pub fn share(on: bool) {
    set_sharing(on, true);
    app::post(app::UiEvent::HostsChanged);
}

/// The muted apps or who-can-connect settings changed: apply them to the running host.
pub fn settings_changed() {
    if let Some(s) = SHARING.lock().unwrap().as_ref() {
        s.apply_settings(&app::settings());
    }
}

/// From any thread: refresh the tooltip (on the UI thread).
fn changed() {
    let h = HWND_TRAY.load(Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

struct Events;

impl HostEvents for Events {
    fn pairing_code(&self, device: &str, code: &str) {
        view(|v| v.pairing = Some((device.to_string(), code.to_string())));
        changed();
        app::post(app::UiEvent::ShowHome);
        notify(&format!("{device} wants to connect"), &format!("Type this code on it: {}", spaced(code)));
    }
    fn pairing_finished(&self, _paired: bool) {
        view(|v| v.pairing = None);
        changed();
        app::post(app::UiEvent::HostsChanged);
    }
    fn viewers_changed(&self, names: &[String]) {
        let before = view(|v| std::mem::replace(&mut v.viewers, names.to_vec()));
        for joined in names.iter().filter(|n| !before.contains(n)) {
            notify(&format!("{joined} is watching this PC"), "Open bb1 to choose what it can use, or disconnect it.");
        }
        changed();
        app::post(app::UiEvent::HostsChanged);
    }
}

fn spaced(code: &str) -> String {
    if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() }
}

/// UI thread, once: put the icon up; start sharing if it was on (or `share` asks for it).
pub fn init(share: bool) {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: w!("bb1Tray"), ..Default::default() };
        RegisterClassW(&class);
        // Hidden; only there to own the icon and its menu.
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW, w!("bb1Tray"), w!("bb1"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None) else {
            logi!("tray: couldn't create the icon's window");
            return;
        };
        HWND_TRAY.store(hwnd.0 as isize, Relaxed);
        add_icon(hwnd);
    }
    if share || app::settings().sharing {
        // `--host` shares this time only; the menu's switch is what's remembered.
        set_sharing(true, !share);
    }
}

/// Before quitting: stop sharing (without changing the setting) and take the icon down.
pub fn shutdown() {
    if let Some(s) = SHARING.lock().unwrap().take() {
        s.stop();
    }
    let h = HWND_TRAY.load(Relaxed);
    if h != 0 {
        unsafe {
            let d = icon_data(HWND(h as _));
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
}

fn set_sharing(on: bool, remember: bool) {
    if remember {
        app::update_settings(|s| s.sharing = on);
    }
    if on == sharing() {
        return;
    }
    if on {
        match Sharing::start(Arc::new(Events), &app::settings()) {
            Ok(s) => {
                *SHARING.lock().unwrap() = Some(s);
                view(|v| v.problem = None);
            }
            Err(e) => {
                logi!("host: couldn't share this PC: {e}");
                let mut text = e.clone();
                if let Some(first) = text.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                view(|v| v.problem = Some(text.clone()));
                notify("bb1 couldn't share this PC", &text);
            }
        }
    } else if let Some(s) = SHARING.lock().unwrap().take() {
        s.stop();
        view(|v| {
            v.viewers.clear();
            v.pairing = None;
            v.problem = None;
        });
    }
    update_tip();
}

/// Closing Your Macs the first time: say where bb1 went.
pub fn told_still_running() {
    if !app::settings().told_about_tray {
        app::update_settings(|s| s.told_about_tray = true);
        notify("bb1 is still running", "It's by the clock. Click it to open bb1; right-click to share this PC or quit.");
    }
}

fn tooltip() -> String {
    view(|v| {
        if !sharing() {
            if v.problem.is_some() { "bb1 · couldn't share this PC".into() } else { "bb1".into() }
        } else if let Some((device, code)) = &v.pairing {
            format!("bb1 · code for {device}: {}", spaced(code))
        } else if v.viewers.is_empty() {
            "bb1 · sharing this PC".into()
        } else {
            format!("bb1 · {} watching: {}", v.viewers.len(), crate::core::updater::list(&v.viewers))
        }
    })
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: TRAY_ID, ..Default::default() };
    d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    d
}

fn copy_into(dst: &mut [u16], text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn add_icon(hwnd: HWND) {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let mut d = icon_data(hwnd);
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default();
        copy_into(&mut d.szTip, &tooltip());
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
    }
}

fn update_tip() {
    let h = HWND_TRAY.load(Relaxed);
    if h == 0 {
        return;
    }
    unsafe {
        let mut d = icon_data(HWND(h as _));
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_into(&mut d.szTip, &tooltip());
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

/// A Windows notification from the icon.
pub fn notify(title: &str, text: &str) {
    let h = HWND_TRAY.load(Relaxed);
    if h == 0 {
        return;
    }
    unsafe {
        let mut d = icon_data(HWND(h as _));
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn show_menu(hwnd: HWND) {
    let (viewers, pairing, problem) = view(|v| (v.viewers.clone(), v.pairing.clone(), v.problem.clone()));
    let on = sharing();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(menu, flags, id, PCWSTR(t.as_ptr()));
        };
        add(MF_STRING, CMD_OPEN, "Open bb1");
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(if on { MF_STRING | MF_CHECKED } else { MF_STRING }, CMD_SHARE, "Share this PC");
        if on {
            if let Some((device, code)) = &pairing {
                add(MF_STRING | MF_GRAYED, 0, &format!("{device} wants to connect: type {}", spaced(code)));
                add(MF_STRING, CMD_CANCEL_PAIRING, "Don't let it in");
            }
            if viewers.is_empty() {
                add(MF_STRING | MF_GRAYED, 0, "Nobody's watching");
            } else {
                add(MF_STRING | MF_GRAYED, 0, &format!("Watching: {}", crate::core::updater::list(&viewers)));
                add(MF_STRING, CMD_DISCONNECT, if viewers.len() == 1 { "Disconnect" } else { "Disconnect everyone" });
            }
        } else if let Some(p) = &problem {
            add(MF_STRING | MF_GRAYED, 0, p);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(MF_STRING, CMD_QUIT, "Quit bb1");
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // Without this the menu doesn't close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match choice.0 as usize {
            CMD_OPEN => crate::win::connect::show(""),
            CMD_SHARE => share(!on),
            CMD_DISCONNECT => {
                if let Some(s) = SHARING.lock().unwrap().as_ref() {
                    s.host().disconnect_all("disconnected from the PC");
                }
            }
            CMD_CANCEL_PAIRING => cancel_pairing(),
            CMD_QUIT => app::quit(),
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
    let taskbar_created = *TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event is in the low word of lparam.
            match (lparam.0 & 0xFFFF) as u32 {
                NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => crate::win::connect::show(""),
                WM_CONTEXTMENU | WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_REFRESH => {
            update_tip();
            LRESULT(0)
        }
        // Explorer restarted: put the icon back.
        m if m == taskbar_created && m != 0 => {
            add_icon(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
