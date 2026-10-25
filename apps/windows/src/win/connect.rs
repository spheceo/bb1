//! The first window: Macs to connect to (paired ones, Macs on this Tailscale account, Macs on the
//! home network), an address box, and the 6-digit code prompt for pairing.

use crate::core::discovery::Browser;
use crate::core::store::KnownHost;
use crate::core::tailscale::{self, Peer};
use crate::win::app::{self, UiEvent};
use crate::win::util::{get_text, pcwstr, scale, set_text, ui_font, wide};
use crossbeam_channel::Sender;
use std::cell::RefCell;
use std::sync::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const ID_LIST: i32 = 100;
const ID_ADDRESS: i32 = 101;
const ID_CONNECT: i32 = 1; // IDOK: Enter presses it
const ID_STATUS: i32 = 103;
const ID_TITLE: i32 = 104;
const ID_HINT: i32 = 105;
const TIMER_REFRESH: usize = 1;

#[derive(Clone)]
struct Entry {
    label: String,
    address: String,
    detail: String,
}

struct State {
    hwnd: HWND,
    list: HWND,
    address: HWND,
    status: HWND,
    title: HWND,
    hint: HWND,
    connect: HWND,
    entries: Vec<Entry>,
    browser: Option<Browser>,
    fonts: Vec<HFONT>,
    code_window: Option<(HWND, HWND, Option<Sender<Option<String>>>)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Tailscale peers, refreshed in the background (the CLI takes a moment).
static PEERS: Mutex<Option<Vec<Peer>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

pub fn create() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = w!("bb1Connect");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("bb1"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            520,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .expect("couldn't create the main window");
        // Before build(): the threads it starts post events straight away.
        app::set_event_window(hwnd);
        // Size for this monitor's DPI now that the window exists.
        let (w, h) = (scale(hwnd, 460), scale(hwnd, 520));
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER);
        build(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        hwnd
    }
}

unsafe fn child(parent: HWND, class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: i32) -> HWND {
    let t = wide(text);
    unsafe {
        CreateWindowExW(ex, class, pcwstr(&t), WS_CHILD | WS_VISIBLE | style, 0, 0, 10, 10, Some(parent),
                        Some(HMENU(id as _)), GetModuleHandleW(None).ok().map(|h| h.into()), None)
            .unwrap_or_default()
    }
}

fn build(hwnd: HWND) {
    unsafe {
        let title = child(hwnd, w!("STATIC"), "Connect to a Mac", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), ID_TITLE);
        let list = child(hwnd, w!("LISTBOX"), "", WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
                         WS_EX_CLIENTEDGE, ID_LIST);
        let hint = child(hwnd, w!("STATIC"), "Or type its address (Tailscale IP or name):", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), ID_HINT);
        let address = child(hwnd, w!("EDIT"), "", WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32), WS_EX_CLIENTEDGE, ID_ADDRESS);
        let connect = child(hwnd, w!("BUTTON"), "Connect", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_CONNECT);
        let status = child(hwnd, w!("STATIC"), "", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), ID_STATUS);
        let big = ui_font(hwnd, 14, true);
        let normal = ui_font(hwnd, 10, false);
        let list_font = ui_font(hwnd, 11, false);
        for (h, f) in [(title, big), (list, list_font), (hint, normal), (address, list_font), (connect, normal), (status, normal)] {
            SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        }
        if let Some(last) = app::settings().last_address {
            set_text(address, &last);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd, list, address, status, title, hint, connect,
                entries: Vec::new(),
                browser: None,
                fonts: vec![big, normal, list_font],
                code_window: None,
            })
        });
        layout(hwnd);
        let browser = Browser::start(|| app::post(UiEvent::HostsChanged));
        with(|s| s.browser = Some(browser));
        refresh_peers();
        SetTimer(Some(hwnd), TIMER_REFRESH, 5000, None);
        let _ = SetFocus(Some(list));
    }
}

fn layout(hwnd: HWND) {
    let Some((title, list, hint, address, connect, status)) = with(|s| (s.title, s.list, s.hint, s.address, s.connect, s.status)) else {
        return;
    };
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right, rc.bottom);
        let m = scale(hwnd, 16);
        let line = scale(hwnd, 24);
        let button_w = scale(hwnd, 96);
        let status_h = scale(hwnd, 44);
        let mut y = m;
        let _ = MoveWindow(title, m, y, w - 2 * m, scale(hwnd, 30), true);
        y += scale(hwnd, 38);
        let list_h = h - y - m - status_h - line * 2 - scale(hwnd, 16);
        let _ = MoveWindow(list, m, y, w - 2 * m, list_h, true);
        y += list_h + scale(hwnd, 10);
        let _ = MoveWindow(hint, m, y, w - 2 * m, line, true);
        y += line;
        let _ = MoveWindow(address, m, y, w - 3 * m / 2 - button_w, scale(hwnd, 28), true);
        let _ = MoveWindow(connect, w - m - button_w, y - 1, button_w, scale(hwnd, 30), true);
        y += scale(hwnd, 38);
        let _ = MoveWindow(status, m, y, w - 2 * m, status_h, true);
    }
}

fn refresh_peers() {
    std::thread::spawn(|| {
        let peers = tailscale::peers();
        *PEERS.lock().unwrap() = peers;
        app::post(UiEvent::HostsChanged);
    });
}

/// Rebuilds the list: paired Macs, Macs on this Tailscale account, Macs on the home network.
pub fn refresh() {
    let peers = PEERS.lock().unwrap().clone();
    let Some(lan) = with(|s| s.browser.as_ref().map(|b| b.hosts()).unwrap_or_default()) else { return };
    let known = KnownHost::load();
    let mut entries: Vec<Entry> = Vec::new();
    let seen = |entries: &mut Vec<Entry>, e: Entry| {
        if !entries.iter().any(|x| x.label.eq_ignore_ascii_case(&e.label) || x.address == e.address) {
            entries.push(e);
        }
    };
    for p in peers.iter().flatten().filter(|p| p.os == "macOS" && p.online) {
        let paired = known.iter().any(|k| k.tailscale_ip.as_deref() == Some(&p.ip));
        let path = if p.direct.is_some() { "direct" } else { "relayed" };
        seen(&mut entries, Entry {
            label: p.name.clone(),
            address: p.ip.clone(),
            detail: format!("Tailscale · {path}{}", if paired { " · paired" } else { "" }),
        });
    }
    for h in &lan {
        seen(&mut entries, Entry { label: h.name.clone(), address: h.address.clone(), detail: "home network".into() });
    }
    for k in &known {
        let Some(address) = k.tailscale_ip.clone().or_else(|| k.address.clone()) else { continue };
        seen(&mut entries, Entry { label: k.name.clone(), address, detail: "paired".into() });
    }
    for p in peers.iter().flatten().filter(|p| p.os == "macOS" && !p.online) {
        seen(&mut entries, Entry { label: p.name.clone(), address: p.ip.clone(), detail: "Tailscale · offline".into() });
    }
    let Some((list, status, same)) = with(|s| {
        let same = entries.iter().map(|e| (&e.label, &e.detail)).eq(s.entries.iter().map(|e| (&e.label, &e.detail)));
        if !same {
            s.entries = entries.clone();
        }
        (s.list, s.status, same)
    }) else { return };
    if same {
        return;
    }
    unsafe {
        let selected = SendMessageW(list, LB_GETCURSEL, None, None).0;
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for e in &entries {
            let t = wide(&format!("{}    ({})", e.label, e.detail));
            SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
        let sel = if selected >= 0 && (selected as usize) < entries.len() { selected } else { 0 };
        if !entries.is_empty() {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(sel as usize)), None);
        }
    }
    if entries.is_empty() {
        set_text(status, "No Macs found yet. Is bb1 sharing on the Mac, and Tailscale running on both?");
    } else if get_text(status).starts_with("No Macs") {
        set_text(status, "");
    }
}

fn connect_selected(from_address_box: bool) {
    let Some((list, address_box)) = with(|s| (s.list, s.address)) else { return };
    let typed = get_text(address_box).trim().to_string();
    let sel = unsafe { SendMessageW(list, LB_GETCURSEL, None, None).0 };
    let picked = if from_address_box && !typed.is_empty() {
        Some((typed.clone(), String::new()))
    } else {
        with(|s| s.entries.get(sel.max(0) as usize).filter(|_| sel >= 0).map(|e| (e.address.clone(), e.label.clone()))).flatten()
            .or_else(|| (!typed.is_empty()).then(|| (typed, String::new())))
    };
    let Some((address, label)) = picked else {
        set_status("Pick a Mac, or type its address.");
        return;
    };
    set_status(&format!("Connecting to {}…", if label.is_empty() { &address } else { &label }));
    app::connect_to(&address, &label);
}

// Win32 calls can send messages straight back to `wndproc`, which uses STATE too: only ever make
// them after the borrow has ended.

pub fn set_status(text: &str) {
    if let Some(status) = with(|s| s.status) {
        set_text(status, text);
    }
}

pub fn hide() {
    if let Some(hwnd) = with(|s| s.hwnd) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

pub fn show(message: &str) {
    let Some((hwnd, status)) = with(|s| (s.hwnd, s.status)) else { return };
    set_text(status, message);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    refresh_peers();
}

/// Lets Tab, Enter and Esc work in the host list and the code prompt.
pub fn is_dialog_message(msg: &MSG) -> bool {
    let Some((hwnd, code)) = with(|s| (s.hwnd, s.code_window.as_ref().map(|c| c.0))) else { return false };
    unsafe {
        if let Some(code) = code {
            if IsDialogMessageW(code, msg).as_bool() {
                return true;
            }
        }
        IsWindowVisible(hwnd).as_bool() && IsDialogMessageW(hwnd, msg).as_bool()
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        app::WM_APP_EVENT => {
            app::drain_events();
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = (wparam.0 >> 16) as u32;
            match (id, code) {
                (ID_LIST, LBN_DBLCLK) => connect_selected(false),
                (ID_CONNECT, _) => {
                    // Enter in the address box, or the button: the typed address wins if there is one.
                    let list = with(|s| s.list);
                    let focus_in_list = list == Some(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() });
                    connect_selected(!focus_in_list);
                }
                (ID_LIST, LBN_SELCHANGE) => {
                    if let Some(a) = with(|s| s.address) {
                        set_text(a, "");
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_REFRESH => {
            if unsafe { IsWindowVisible(hwnd).as_bool() } {
                refresh_peers();
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => unsafe {
            let hdc = HDC(wparam.0 as _);
            SetBkMode(hdc, TRANSPARENT);
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        },
        WM_DPICHANGED => unsafe {
            let rc = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER);
            layout(hwnd);
            LRESULT(0)
        },
        WM_SIZE => {
            layout(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => unsafe {
            PostQuitMessage(0);
            LRESULT(0)
        },
        WM_DESTROY => {
            for f in with(|s| std::mem::take(&mut s.fonts)).unwrap_or_default() {
                unsafe {
                    let _ = DeleteObject(f.into());
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// MARK: Pairing code prompt

const ID_CODE: i32 = 200;
const ID_CANCEL: i32 = 2; // IDCANCEL: Esc presses it

/// Asks for the 6-digit code the Mac shows; `reply` gets it (None = cancelled).
pub fn ask_code(host: &str, reply: Sender<Option<String>>) {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = w!("bb1Code");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(code_wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let owner = crate::win::stream::hwnd().or_else(|| with(|s| s.hwnd));
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_TOPMOST,
            class,
            w!("Pair with a Mac"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            200,
            owner,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap_or_default();
        let (w, h) = (scale(hwnd, 380), scale(hwnd, 190));
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let _ = SetWindowPos(hwnd, None, (sw - w) / 2, (sh - h) / 3, w, h, SWP_NOZORDER);
        let m = scale(hwnd, 16);
        let label = child(hwnd, w!("STATIC"), &format!("Type the 6-digit code shown on {host}:"), WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0);
        let edit = child(hwnd, w!("EDIT"), "", WS_TABSTOP | WINDOW_STYLE((ES_NUMBER | ES_CENTER) as u32), WS_EX_CLIENTEDGE, ID_CODE);
        let ok = child(hwnd, w!("BUTTON"), "Pair", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_CONNECT);
        let cancel = child(hwnd, w!("BUTTON"), "Cancel", WS_TABSTOP, WINDOW_EX_STYLE(0), ID_CANCEL);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let cw = rc.right;
        let _ = MoveWindow(label, m, m, cw - 2 * m, scale(hwnd, 22), true);
        let _ = MoveWindow(edit, m, m + scale(hwnd, 30), cw - 2 * m, scale(hwnd, 40), true);
        let bw = scale(hwnd, 90);
        let by = m + scale(hwnd, 84);
        let _ = MoveWindow(cancel, cw - m - bw, by, bw, scale(hwnd, 30), true);
        let _ = MoveWindow(ok, cw - m - 2 * bw - scale(hwnd, 8), by, bw, scale(hwnd, 30), true);
        let normal = ui_font(hwnd, 10, false);
        let big = ui_font(hwnd, 20, true);
        for (c, f) in [(label, normal), (edit, big), (ok, normal), (cancel, normal)] {
            SendMessageW(c, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        }
        SendMessageW(edit, 0x00C5 /* EM_LIMITTEXT */, Some(WPARAM(6)), None);
        // Cancel any older prompt.
        let old = with(|s| s.code_window.replace((hwnd, edit, Some(reply)))).flatten();
        if let Some((old_hwnd, _, old_reply)) = old {
            if let Some(r) = old_reply {
                let _ = r.send(None);
            }
            let _ = DestroyWindow(old_hwnd);
        }
        if let Some(o) = owner {
            let _ = EnableWindow(o, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(edit));
    }
}

fn finish_code(ok: bool) {
    let Some(Some((hwnd, edit, reply))) = with(|s| s.code_window.take()) else { return };
    let code = get_text(edit);
    if let Some(r) = reply {
        let _ = r.send(if ok && !code.trim().is_empty() { Some(code.trim().to_string()) } else { None });
    }
    unsafe {
        if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
            let _ = EnableWindow(owner, true);
            let _ = SetForegroundWindow(owner);
        }
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn code_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as i32 {
                ID_CONNECT => finish_code(true),
                ID_CANCEL => finish_code(false),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => unsafe {
            SetBkMode(HDC(wparam.0 as _), TRANSPARENT);
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        },
        WM_CLOSE => {
            finish_code(false);
            LRESULT(0)
        }
        // Destroyed along with its owner (the stream window closed while it was up): answer
        // "cancelled" so the connection thread waiting in ask_code isn't left waiting forever.
        WM_DESTROY => {
            let gone = with(|s| if s.code_window.as_ref().is_some_and(|c| c.0 == hwnd) { s.code_window.take() } else { None }).flatten();
            if let Some((_, _, Some(reply))) = gone {
                let _ = reply.send(None);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
