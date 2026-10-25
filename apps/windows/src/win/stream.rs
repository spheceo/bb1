//! The stream window: shows the Mac's screen and sends it mouse and keyboard input.
//!
//! Control follows the pointer, like the Mac client: while this window is in front and the pointer
//! is on the picture (or the window is full screen), keys go to the Mac, including the Windows key,
//! Alt+Tab and other shortcuts (through a low-level keyboard hook). Move the pointer off the picture
//! and the keyboard is the PC's again. Ctrl+Alt+Enter switches full screen.

use crate::core::client::{Client, Stats};
use crate::core::keymap::{self, MAC_CAPS_LOCK, ModifierLayout};
use crate::core::proto::*;
use crate::logi;
use crate::win::app;
use crate::win::audio_out::AudioOut;
use crate::win::render::{self, VideoCmd, fit};
use crate::win::util::{scale, set_text, wide};
use crossbeam_channel::Sender;
use std::cell::RefCell;
use std::collections::HashSet;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

const WM_APP_FULLSCREEN: u32 = WM_APP + 10;
const TIMER_CLIPBOARD: usize = 2;

// System menu commands (multiples of 16: Windows uses the low 4 bits).
const CMD_FULLSCREEN: usize = 0x10;
const CMD_LAYOUT_STANDARD: usize = 0x20;
const CMD_LAYOUT_MAC: usize = 0x30;
const CMD_LAYOUT_CTRL: usize = 0x40;
const CMD_SOUND: usize = 0x50;
const CMD_CLIPBOARD: usize = 0x60;
const CMD_DISCONNECT: usize = 0x70;
const CMD_KEYFRAME: usize = 0x80;

struct State {
    hwnd: HWND,
    video: Sender<VideoCmd>,
    client: Option<Client>,
    label: String,
    video_size: (u32, u32),
    /// Saved window placement and style while full screen.
    fullscreen: Option<(WINDOWPLACEMENT, i32)>,
    keys_down: HashSet<u16>,
    /// AltGr's fake left Ctrl went to the Mac (so its key-up must too).
    altgr_ctrl_to_mac: bool,
    /// Enter is down as part of Ctrl+Alt+Enter.
    fullscreen_enter: bool,
    buttons_held: u32,
    /// (time ms, x, y, button, count) of the last press, for double clicks.
    last_click: (u32, i32, i32, i64, i64),
    caps_on: bool,
    cursor: Option<HCURSOR>,
    layout: ModifierLayout,
    hook: Option<HHOOK>,
    connected: bool,
    status_text: String,
    audio: Option<AudioOut>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

// Win32 calls can send messages straight back to `wndproc` (or run the keyboard hook), which use
// STATE too: closures passed to `with` only read and write fields, never call Win32.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

pub fn hwnd() -> Option<HWND> {
    with(|s| s.hwnd)
}

/// Creates the stream window and its video thread.
pub fn open(label: &str) -> Sender<VideoCmd> {
    close();
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = w!("bb1Stream");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: HCURSOR::default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // 80% of the screen's width at 16:10, centred.
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let mut w = ww * 8 / 10;
        let mut h = w * 10 / 16;
        if h > wh * 88 / 100 {
            h = wh * 88 / 100;
            w = h * 16 / 10;
        }
        let title = HSTRING::from(format!("{label} — bb1"));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &title,
            WS_OVERLAPPEDWINDOW,
            work.left + (ww - w) / 2,
            work.top + (wh - h) / 2,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .expect("couldn't create the stream window");
        add_menu(hwnd);
        let video = render::spawn(hwnd);
        let settings = app::settings();
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance.into()), 0).ok();
        if hook.is_none() {
            logi!("stream: couldn't install the keyboard hook; Windows-key shortcuts stay on this PC");
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                video: video.clone(),
                client: None,
                label: label.to_string(),
                video_size: (0, 0),
                fullscreen: None,
                keys_down: HashSet::new(),
                altgr_ctrl_to_mac: false,
                fullscreen_enter: false,
                buttons_held: 0,
                last_click: (0, 0, 0, -1, 0),
                caps_on: GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0,
                cursor: None,
                layout: ModifierLayout::from_id(&settings.modifier_layout),
                hook,
                connected: false,
                status_text: String::new(),
                audio: None,
            })
        });
        update_menu();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        video
    }
}

pub fn attach(client: Client) {
    let Some(video) = with(|s| {
        s.client = Some(client.clone());
        s.video.clone()
    }) else { return };
    let _ = video.send(VideoCmd::Attach(client));
}

pub fn close() {
    let Some(state) = STATE.with(|s| s.borrow_mut().take()) else { return };
    unsafe {
        if let Some(h) = state.hook {
            let _ = UnhookWindowsHookEx(h);
        }
        let _ = KillTimer(Some(state.hwnd), TIMER_CLIPBOARD);
        let _ = state.video.send(VideoCmd::Quit);
        drop(state.audio);
        if let Some(c) = state.cursor {
            let _ = DestroyCursor(c);
        }
        let _ = DestroyWindow(state.hwnd);
    }
}

fn client() -> Option<Client> {
    with(|s| s.client.clone()).flatten()
}

fn send(msg: Msg) {
    if let Some(c) = client() {
        c.send(msg);
    }
}

fn refresh_title(extra: &str) {
    let Some((hwnd, label)) = with(|s| (s.hwnd, s.label.clone())) else { return };
    set_text(hwnd, &if extra.is_empty() { format!("{label} — bb1") } else { format!("{label} — {extra}") });
}

pub fn status(text: &str, connected: bool) {
    with(|s| {
        s.status_text = text.to_string();
        s.connected = connected;
    });
    refresh_title(text);
}

pub fn welcome(_host: &str) {
    let Some((hwnd, caps)) = with(|s| (s.hwnd, s.caps_on)) else { return };
    let settings = app::settings();
    send(Msg::CapsLock(v(caps)));
    if settings.sound {
        start_audio();
    }
    if settings.clipboard {
        crate::win::clipboard::start();
    }
    unsafe {
        SetTimer(Some(hwnd), TIMER_CLIPBOARD, 300, None);
    }
}

fn start_audio() {
    let Some(c) = client() else { return };
    let audio = AudioOut::start(c);
    with(|s| s.audio = Some(audio));
}

pub fn config(c: &StreamConfig) {
    let Some(hwnd) = with(|s| {
        s.video_size = (c.width as u32, c.height as u32);
        s.hwnd
    }) else { return };
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn stats(s: &Stats) {
    if !s.connected {
        return;
    }
    let mut t = format!("{:.0} fps · {:.1} Mbps · RTT {:.1} ms", s.fps, s.mbps, s.rtt_ms);
    if let Some(l) = s.latency_ms {
        t += &format!(" · {l:.0} ms behind");
    }
    if !s.path.is_empty() {
        t += &format!(" · {}", s.path);
    }
    refresh_title(&t);
}

/// A message from the Mac (no Accessibility permission, low disk…): shown without blocking the stream.
pub fn notice(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || unsafe {
        // No owner: MessageBox disables its owner window, which would block clicks on the stream.
        MessageBoxW(None, &HSTRING::from(text), w!("bb1"), MB_ICONINFORMATION | MB_OK | MB_TOPMOST);
    });
}

pub fn set_cursor(c: &CursorMsg) {
    let Some(hwnd) = hwnd() else { return };
    let Some(cursor) = crate::win::cursor::make(c, crate::win::util::dpi(hwnd)) else { return };
    let old = with(|s| s.cursor.replace(cursor)).flatten();
    unsafe {
        // Show it now if the pointer is over the picture.
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_ok() && WindowFromPoint(p) == hwnd && pointer_over_picture(hwnd) {
            SetCursor(Some(cursor));
        }
        if let Some(o) = old {
            let _ = DestroyCursor(o);
        }
    }
}

// MARK: Geometry

/// Where the picture is in the window (letterboxed), in client pixels.
fn picture_rect(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let size = with(|s| s.video_size).unwrap_or((0, 0));
    if size.0 == 0 {
        return rc;
    }
    let (x, y, w, h) = fit(rc.right as u32, rc.bottom as u32, size.0, size.1);
    RECT { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 }
}

fn pointer_over_picture(hwnd: HWND) -> bool {
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_err() {
            return false;
        }
        let _ = ScreenToClient(hwnd, &mut p);
        let r = picture_rect(hwnd);
        p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
    }
}

/// Client pixels → 0…1 on the Mac's screen.
fn normalized(hwnd: HWND, x: i32, y: i32) -> (f64, f64) {
    let r = picture_rect(hwnd);
    let w = (r.right - r.left).max(1) as f64;
    let h = (r.bottom - r.top).max(1) as f64;
    (((x - r.left) as f64 / w).clamp(0.0, 1.0), ((y - r.top) as f64 / h).clamp(0.0, 1.0))
}

fn over_picture_at(hwnd: HWND, x: i32, y: i32) -> bool {
    let r = picture_rect(hwnd);
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

// MARK: Keyboard

/// Keys go to the Mac while this window is in front and the pointer is on the picture (or full screen).
fn keyboard_goes_to_mac(hwnd: HWND, fullscreen: bool) -> bool {
    unsafe { GetForegroundWindow() == hwnd && (fullscreen || pointer_over_picture(hwnd)) }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kb.flags.0 & LLKHF_INJECTED.0 == 0 && handle_key(kb, wparam.0 as u32) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Returns true if the key was sent to the Mac (and so is kept from Windows).
///
/// Whatever Windows saw go down it also sees go up, and the same for the Mac, so no key ever
/// sticks on either side.
fn handle_key(kb: &KBDLLHOOKSTRUCT, message: u32) -> bool {
    let Some((hwnd, fullscreen, layout, connected)) = with(|s| (s.hwnd, s.fullscreen.is_some(), s.layout, s.connected)) else {
        return false;
    };
    let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    // AltGr sends a fake left Ctrl first (scan code 0x21D); the right Alt that follows is enough.
    // Its key-up goes wherever its key-down went.
    if kb.scanCode & 0x200 != 0 {
        if !down {
            return with(|s| std::mem::take(&mut s.altgr_ctrl_to_mac)).unwrap_or(false);
        }
        let to_mac = with(|s| s.altgr_ctrl_to_mac).unwrap_or(false) || (connected && keyboard_goes_to_mac(hwnd, fullscreen));
        with(|s| s.altgr_ctrl_to_mac = to_mac);
        return to_mac;
    }
    let vk = kb.vkCode as u16;
    let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
    // Modifiers by virtual key (left/right is reliable there); everything else by scan code (position).
    let (scan, ext) = match VIRTUAL_KEY(vk) {
        VK_LSHIFT => (0x2A, false),
        VK_RSHIFT => (0x36, false),
        VK_LCONTROL => (0x1D, false),
        VK_RCONTROL => (0x1D, true),
        VK_LMENU => (0x38, false),
        VK_RMENU => (0x38, true),
        VK_LWIN => (0x5B, true),
        VK_RWIN => (0x5C, true),
        VK_NUMLOCK => (0x45, false),
        VK_PAUSE => return false,
        _ => (kb.scanCode, extended),
    };
    let Some(code) = keymap::mac_key(scan, ext, layout) else { return false };
    let held = with(|s| s.keys_down.contains(&code)).unwrap_or(false);
    if code == MAC_CAPS_LOCK {
        // Caps Lock is a state on the Mac, not a key. It isn't kept from Windows, so this PC's
        // Caps Lock (its light, and what WM_ACTIVATE syncs from) always matches the Mac's.
        if !down {
            with(|s| s.keys_down.remove(&code));
        } else if !held && connected && unsafe { GetForegroundWindow() } == hwnd {
            let on = with(|s| {
                s.caps_on = !s.caps_on;
                s.keys_down.insert(code);
                s.caps_on
            })
            .unwrap_or(false);
            send(Msg::CapsLock(v(on)));
        }
        return false;
    }
    // The Enter of Ctrl+Alt+Enter: its repeats and key-up stay here too (repeats would toggle again).
    if vk == VK_RETURN.0 && with(|s| s.fullscreen_enter).unwrap_or(false) {
        if !down {
            with(|s| s.fullscreen_enter = false);
        }
        return true;
    }
    // A key that went down on the Mac goes up there too, and its auto-repeat stays there even if
    // the pointer has left the picture (otherwise Windows would see it go down but never up).
    if held {
        if !down {
            with(|s| s.keys_down.remove(&code));
        }
        send(Msg::key(code, down, down));
        return true;
    }
    if !down || !connected || !keyboard_goes_to_mac(hwnd, fullscreen) {
        return false;
    }
    // Ctrl+Alt+Enter: full screen (stays on this PC). Ctrl and Alt were kept from Windows (sent to
    // the Mac), so Windows' own key state doesn't know they're down: look at what the Mac has.
    if vk == VK_RETURN.0 {
        let ctrl_alt = with(|s| {
            let pressed = |scan: u32| [false, true].into_iter().any(|e| keymap::mac_key(scan, e, s.layout).is_some_and(|c| s.keys_down.contains(&c)));
            pressed(0x1D) && pressed(0x38)
        })
        .unwrap_or(false);
        if ctrl_alt || (key_down(VK_CONTROL) && key_down(VK_MENU)) {
            with(|s| s.fullscreen_enter = true);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_FULLSCREEN, WPARAM(0), LPARAM(0));
            }
            return true;
        }
    }
    with(|s| s.keys_down.insert(code));
    send(Msg::key(code, true, false));
    true
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

/// Lift everything we pressed on the Mac (focus left, pointer left the picture, disconnect).
fn release_all() {
    let had = with(|s| {
        let had = !s.keys_down.is_empty() || s.buttons_held > 0;
        s.keys_down.clear();
        s.buttons_held = 0;
        s.altgr_ctrl_to_mac = false;
        s.fullscreen_enter = false;
        had
    })
    .unwrap_or(false);
    if had {
        send(Msg::ReleaseAll {});
    }
}

// MARK: Full screen

fn toggle_fullscreen(hwnd: HWND) {
    let saved = with(|s| s.fullscreen.take()).flatten();
    unsafe {
        match saved {
            Some((placement, style)) => {
                SetWindowLongW(hwnd, GWL_STYLE, style);
                let _ = SetWindowPlacement(hwnd, &placement);
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            }
            None => {
                let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(hwnd, &mut placement);
                let style = GetWindowLongW(hwnd, GWL_STYLE);
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(monitor, &mut info);
                let r = info.rcMonitor;
                with(|s| s.fullscreen = Some((placement, style)));
                SetWindowLongW(hwnd, GWL_STYLE, (style as u32 & !WS_OVERLAPPEDWINDOW.0) as i32 | WS_POPUP.0 as i32 | WS_VISIBLE.0 as i32);
                let _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED);
            }
        }
    }
    update_menu();
}

// MARK: Menu

fn add_menu(hwnd: HWND) {
    unsafe {
        let menu = GetSystemMenu(hwnd, false);
        let item = |id: usize, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(t.as_ptr()));
        };
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        item(CMD_FULLSCREEN, "Full screen\tCtrl+Alt+Enter");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        item(CMD_LAYOUT_STANDARD, "Windows key is \u{2318} Command");
        item(CMD_LAYOUT_MAC, "Alt is \u{2318} Command (Mac key positions)");
        item(CMD_LAYOUT_CTRL, "Ctrl is \u{2318} Command (Ctrl+C copies)");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        item(CMD_SOUND, "Sound");
        item(CMD_CLIPBOARD, "Share clipboard");
        item(CMD_KEYFRAME, "Refresh picture");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        item(CMD_DISCONNECT, "Disconnect");
    }
}

fn update_menu() {
    let Some((hwnd, layout, fullscreen)) = with(|s| (s.hwnd, s.layout, s.fullscreen.is_some())) else { return };
    let settings = app::settings();
    unsafe {
        let menu = GetSystemMenu(hwnd, false);
        let check = |id: usize, on: bool| {
            CheckMenuItem(menu, id as u32, (MF_BYCOMMAND | if on { MF_CHECKED } else { MF_UNCHECKED }).0);
        };
        check(CMD_FULLSCREEN, fullscreen);
        check(CMD_LAYOUT_STANDARD, layout == ModifierLayout::Standard);
        check(CMD_LAYOUT_MAC, layout == ModifierLayout::MacPositions);
        check(CMD_LAYOUT_CTRL, layout == ModifierLayout::CtrlIsCommand);
        check(CMD_SOUND, settings.sound);
        check(CMD_CLIPBOARD, settings.clipboard);
    }
}

fn command(hwnd: HWND, id: usize) -> bool {
    match id {
        CMD_FULLSCREEN => toggle_fullscreen(hwnd),
        CMD_LAYOUT_STANDARD | CMD_LAYOUT_MAC | CMD_LAYOUT_CTRL => {
            release_all();
            let layout = match id {
                CMD_LAYOUT_MAC => ModifierLayout::MacPositions,
                CMD_LAYOUT_CTRL => ModifierLayout::CtrlIsCommand,
                _ => ModifierLayout::Standard,
            };
            with(|s| s.layout = layout);
            app::update_settings(|s| s.modifier_layout = layout.id().into());
            logi!("stream: modifier keys: {}", layout.id());
        }
        CMD_SOUND => {
            let on = !app::settings().sound;
            app::update_settings(|s| s.sound = on);
            if let Some(c) = client() {
                c.set_sound(on);
            }
            if on {
                start_audio();
            } else {
                with(|s| s.audio = None);
            }
        }
        CMD_CLIPBOARD => {
            let on = !app::settings().clipboard;
            app::update_settings(|s| s.clipboard = on);
            if on {
                crate::win::clipboard::start();
            }
        }
        CMD_KEYFRAME => {
            if let Some(c) = client() {
                c.decoder_failed("picture refresh requested");
            }
        }
        CMD_DISCONNECT => app::disconnect(),
        _ => return false,
    }
    update_menu();
    true
}

// MARK: Mouse

fn mouse_xy(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn press(hwnd: HWND, button: i64, lparam: LPARAM) {
    let (x, y) = mouse_xy(lparam);
    if !over_picture_at(hwnd, x, y) || !with(|s| s.connected).unwrap_or(false) {
        return;
    }
    let now = unsafe { GetMessageTime() } as u32;
    let (dx, dy) = unsafe { (GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK)) };
    let interval = unsafe { GetDoubleClickTime() };
    let clicks = with(|s| {
        let (t, lx, ly, lb, count) = s.last_click;
        let n = if lb == button && now.wrapping_sub(t) <= interval && (x - lx).abs() <= dx / 2 && (y - ly).abs() <= dy / 2 {
            count + 1
        } else {
            1
        };
        s.last_click = (now, x, y, button, n);
        s.buttons_held += 1;
        n
    })
    .unwrap_or(1);
    unsafe {
        SetCapture(hwnd);
    }
    let (nx, ny) = normalized(hwnd, x, y);
    send(Msg::MouseButton(v(MouseButtonMsg { button, down: true, clicks, x: nx, y: ny })));
}

fn release(hwnd: HWND, button: i64, lparam: LPARAM) {
    let (x, y) = mouse_xy(lparam);
    let Some((held, clicks)) = with(|s| {
        let held = s.buttons_held > 0;
        if held {
            s.buttons_held -= 1;
        }
        (held, if s.last_click.3 == button { s.last_click.4 } else { 1 })
    }) else { return };
    if !held {
        return;
    }
    if with(|s| s.buttons_held == 0).unwrap_or(true) {
        unsafe {
            let _ = ReleaseCapture();
        }
    }
    let (nx, ny) = normalized(hwnd, x, y);
    send(Msg::MouseButton(v(MouseButtonMsg { button, down: false, clicks, x: nx, y: ny })));
}

fn wheel(hwnd: HWND, wparam: WPARAM, lparam: LPARAM, horizontal: bool) {
    // Wheel messages carry screen coordinates.
    let mut p = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
    }
    if !over_picture_at(hwnd, p.x, p.y) || !with(|s| s.connected).unwrap_or(false) {
        return;
    }
    let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f64;
    let lines = app::settings().wheel_lines;
    // Whole notches are a mouse wheel (lines); anything finer is a touchpad (pixels).
    let precise = delta.abs() % 120.0 != 0.0;
    let amount = if precise { delta * 0.5 } else { delta / 120.0 * lines };
    // Windows: wheel forward / tilt right are positive. Mac: scroll up / scroll left are positive.
    let (dx, dy) = if horizontal { (-amount, 0.0) } else { (0.0, amount) };
    send(Msg::Scroll(v(ScrollMsg { dx, dy, precise, phase: 0, momentum: 0 })));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = mouse_xy(lparam);
            let held = with(|s| s.buttons_held > 0).unwrap_or(false);
            if (held || over_picture_at(hwnd, x, y)) && with(|s| s.connected).unwrap_or(false) {
                let (nx, ny) = normalized(hwnd, x, y);
                send(Msg::MouseMove(v(MouseMoveMsg { x: nx, y: ny })));
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            press(hwnd, 0, lparam);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            release(hwnd, 0, lparam);
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => {
            press(hwnd, 1, lparam);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            release(hwnd, 1, lparam);
            LRESULT(0)
        }
        WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => {
            press(hwnd, 2, lparam);
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            release(hwnd, 2, lparam);
            LRESULT(0)
        }
        WM_XBUTTONDOWN | WM_XBUTTONDBLCLK => {
            press(hwnd, if (wparam.0 >> 16) & 0xFFFF == 1 { 3 } else { 4 }, lparam);
            LRESULT(1)
        }
        WM_XBUTTONUP => {
            release(hwnd, if (wparam.0 >> 16) & 0xFFFF == 1 { 3 } else { 4 }, lparam);
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            wheel(hwnd, wparam, lparam, false);
            LRESULT(0)
        }
        WM_MOUSEHWHEEL => {
            wheel(hwnd, wparam, lparam, true);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                let cursor = with(|s| s.cursor).flatten();
                unsafe {
                    match cursor {
                        Some(c) if pointer_over_picture(hwnd) => SetCursor(Some(c)),
                        _ => SetCursor(LoadCursorW(None, IDC_ARROW).ok()),
                    };
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == WA_INACTIVE as usize {
                release_all();
            } else if with(|s| s.connected).unwrap_or(false) {
                // Match the Mac's Caps Lock to this keyboard's when coming back.
                let on = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
                with(|s| s.caps_on = on);
                send(Msg::CapsLock(v(on)));
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_TIMER if wparam.0 == TIMER_CLIPBOARD => {
            if app::settings().clipboard {
                if let Some(clip) = crate::win::clipboard::local_change() {
                    send(Msg::Clipboard(v(clip)));
                }
            }
            LRESULT(0)
        }
        WM_APP_FULLSCREEN => {
            toggle_fullscreen(hwnd);
            LRESULT(0)
        }
        WM_SYSCOMMAND => {
            if command(hwnd, wparam.0 & 0xFFF0) {
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SIZE => {
            if let Some(v) = with(|s| s.video.clone()) {
                let _ = v.send(VideoCmd::Resize);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            if let Some(v) = with(|s| s.video.clone()) {
                let _ = v.send(VideoCmd::Redraw);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: scale(hwnd, 320), y: scale(hwnd, 200) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let rc = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            app::disconnect();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
