//! "Your Macs": the first window, laid out like the Mac app's (apps/mac/Sources/bb1/App/Dashboard.swift).
//! A tile per Mac with its live screen, name and status; click one to connect, + to connect by
//! address. Also the prompts for an address and for the 6-digit pairing code, in the same style.

use crate::core::client::{Auth, Target};
use crate::core::directory::{self, Directory, Entry};
use crate::core::store::KnownHost;
use crate::core::updater::{Pill, Updater};
use crate::win::app::{self, UiEvent};
use crate::win::gfx::{self, Color, Font, Gfx, contains, line_box, rect, style};
use crate::win::panels::{self, Act, Acts};
use crate::win::util::{get_text, scale, ui_font};
use crossbeam_channel::Sender;
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

const TIMER_TICK: usize = 1;
/// Hides "Updated to …" a few seconds after it's been seen.
const TIMER_DONE: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;
const FOOTER: f32 = 64.0;
const MARGIN: f32 = 40.0;
const GAP: f32 = 20.0;

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    None,
    Plus,
    Pill,
    /// Something in `acts` (the sharing panel, Settings, the gear).
    Act(usize),
    Tile(usize),
}

struct TileHit {
    rect: D2D_RECT_F,
    entry: Entry,
    can_act: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Macs,
    Settings,
}

struct Home {
    hwnd: HWND,
    gfx: Option<Gfx>,
    directory: Directory,
    hits: Vec<TileHit>,
    plus: D2D_RECT_F,
    updater: Updater,
    /// Where the update pill is (empty when hidden), and whether it can be clicked.
    pill: D2D_RECT_F,
    pill_clickable: bool,
    done_timer: bool,
    hover: Hover,
    scroll: f32,
    page: Page,
    /// Clickable things drawn this time, with what they do (later ones are on top).
    acts: Acts,
    /// The muted-apps list in Settings is open.
    dropdown: bool,
    message: String,
    prompt: Option<Prompt>,
}

thread_local! {
    static HOME: RefCell<Option<Home>> = const { RefCell::new(None) };
}

// Win32 calls can send messages straight back to a window procedure, which uses HOME too: closures
// passed to `with` never call Win32 (try_borrow_mut makes a slip harmless rather than a crash).
fn with<R>(f: impl FnOnce(&mut Home) -> R) -> Option<R> {
    HOME.with(|h| h.try_borrow_mut().ok().and_then(|mut h| h.as_mut().map(f)))
}

fn invalidate() {
    if let Some(hwnd) = with(|h| h.hwnd) {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn register(class: PCWSTR, proc: WNDPROC) {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // a second registration just fails, which is fine
    }
}

pub fn create() -> HWND {
    register(w!("bb1Home"), Some(wndproc));
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
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
        .expect("couldn't create the main window");
        gfx::dark_title_bar(hwnd);
        // Three tiles across at this screen's scale, centred in the work area.
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let w = scale(hwnd, 1220).min(ww * 9 / 10);
        let h = scale(hwnd, 760).min(wh * 9 / 10);
        let _ = SetWindowPos(hwnd, None, work.left + (ww - w) / 2, work.top + (wh - h) / 2, w, h, SWP_NOZORDER);
        // The window gets events from here on (before any background thread can post one).
        app::set_event_window(hwnd);
        let directory = Directory::start(|| app::post(UiEvent::HostsChanged));
        let updater = Updater::start(|| app::post(UiEvent::HostsChanged));
        HOME.with(|h| {
            *h.borrow_mut() = Some(Home {
                hwnd,
                gfx: Gfx::new().ok(),
                directory,
                hits: Vec::new(),
                plus: rect(0.0, 0.0, 0.0, 0.0),
                updater,
                pill: rect(0.0, 0.0, 0.0, 0.0),
                pill_clickable: false,
                done_timer: false,
                hover: Hover::None,
                scroll: 0.0,
                page: Page::Macs,
                acts: Vec::new(),
                dropdown: false,
                message: String::new(),
                prompt: None,
            })
        });
        SetTimer(Some(hwnd), TIMER_TICK, 30_000, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        hwnd
    }
}

pub fn refresh() {
    invalidate();
}

pub fn set_status(text: &str) {
    with(|h| h.message = text.to_string());
    invalidate();
}

pub fn hide() {
    let Some((hwnd, dir)) = with(|h| (h.hwnd, h.directory.clone())) else { return };
    dir.set_active(false);
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn show(message: &str) {
    let Some((hwnd, dir)) = with(|h| {
        h.message = message.to_string();
        (h.hwnd, h.directory.clone())
    }) else { return };
    dir.set_active(true);
    dir.refresh_now();
    if let Some(u) = with(|h| h.updater.clone()) {
        u.check_soon();
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Show the window on top of whatever's in front (a pairing code must be seen). Windows only lets a
/// background app take the foreground in some cases, so it's also made topmost for a moment.
pub fn bring_forward() {
    let Some((hwnd, dir)) = with(|h| {
        h.page = Page::Macs;
        h.scroll = 0.0;
        (h.hwnd, h.directory.clone())
    }) else { return };
    dir.set_active(true);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Lets Enter and Esc work in the prompts.
pub fn is_dialog_message(msg: &MSG) -> bool {
    let Some(Some(prompt)) = with(|h| h.prompt.as_ref().map(|p| p.hwnd)) else { return false };
    unsafe { IsDialogMessageW(prompt, msg).as_bool() }
}

// MARK: Tiles

struct Model {
    name: String,
    status: String,
    live: bool,
    dim: bool,
    action: Option<&'static str>,
}

fn model(e: &Entry, dir: &Directory, preview: Option<&directory::Preview>) -> Model {
    let pairing = e.online && !e.quiet && e.address.as_deref().is_some_and(|a| matches!(Target::new(a, &e.name).auth, Auth::Code));
    let status = if !e.online {
        preview.map(|p| format!("Offline · seen {}", directory::ago(p.at))).unwrap_or_else(|| "Offline".into())
    } else if pairing {
        "Online · needs pairing".into()
    } else if let Some(ms) = dir.latency_ms(&e.name) {
        format!("Online · {} ms", ms.round() as i64)
    } else if dir.build(&e.name).is_some_and(|b| b < 15) {
        "Online · update bb1 there to see its screen".into()
    } else {
        "Online".into()
    };
    Model {
        name: e.name.clone(),
        status,
        live: e.online,
        dim: !e.online,
        action: (e.online && e.address.is_some()).then_some(if pairing { "Pair" } else { "Connect" }),
    }
}

fn paint(h: &mut Home) {
    let Some(g) = h.gfx.as_mut() else { return };
    let Ok((width, height)) = g.begin(h.hwnd) else { return };
    g.clear(style::background());

    // "Your Macs" and its +, like the Mac's header.
    // What the pointer is over, from the last paint's list (hover indexes into it).
    let hover_act = match h.hover {
        Hover::Act(i) => h.acts.get(i).map(|(_, a)| a.clone()),
        _ => None,
    };
    h.acts.clear();
    if h.page == Page::Settings {
        paint_settings(g, &mut h.acts, &mut h.scroll, h.dropdown, width, height, hover_act.as_ref());
        return;
    }
    let top = 26.0;
    // The gear (Settings), top right; the update pill sits to its left.
    let gear = rect(width - MARGIN - 34.0, top + 6.0, 34.0, 34.0);
    draw_gear(g, gear, hover_act.as_ref() == Some(&Act::Gear));
    h.acts.push((gear, Act::Gear));
    // While sharing, who's watching comes first; Your Macs moves down.
    let mut title_y = top;
    if crate::win::tray::is_sharing() {
        let used = panels::sharing_panel(g, MARGIN, top, (width - 2.0 * MARGIN - 50.0).max(200.0), &mut h.acts, hover_act.as_ref());
        title_y = top + used + 30.0;
    }
    g.text("Your computers", Font::Title, rect(MARGIN, title_y, 500.0, 44.0), Color::white(1.0));
    let title_w = g.measure("Your computers", Font::Title);
    h.plus = rect(MARGIN + title_w + 8.0, title_y + 6.0, 30.0, 34.0);
    let plus_alpha = if h.hover == Hover::Plus { 0.75 } else { 0.4 };
    let (px, py) = ((h.plus.left + h.plus.right) / 2.0, (h.plus.top + h.plus.bottom) / 2.0 + 2.0);
    g.line(px - 7.0, py, px + 7.0, py, Color::white(plus_alpha), 2.0);
    g.line(px, py - 7.0, px, py + 7.0, Color::white(plus_alpha), 2.0);

    // The update pill, top right, like the Mac's: offers a newer build, then shows progress inside itself.
    let behind: Vec<String> = macs_behind(&h.directory, &h.updater).into_iter().map(|(t, _)| t.label).collect();
    let pill = h.updater.pill(&behind);
    let hovering = h.hover == Hover::Pill;
    let (label, kind) = match &pill {
        Pill::Hidden => (String::new(), 0),
        Pill::Offer { label, .. } => (label.clone(), 1),
        Pill::Busy { label, .. } => (label.clone(), 2),
        Pill::Done(label) => (label.clone(), 3),
        Pill::Failed(_) => ("Update failed · Try again".to_string(), 4),
    };
    h.pill_clickable = matches!(pill, Pill::Offer { .. } | Pill::Failed(_));
    if kind == 0 {
        h.pill = rect(0.0, 0.0, 0.0, 0.0);
    } else {
        let (ph, cy) = (30.0, top + 23.0);
        let pw = g.measure(&label, Font::Button) + 28.0;
        let r = rect(width - MARGIN - 34.0 - 12.0 - pw, cy - ph / 2.0, pw, ph);
        h.pill = r;
        let radius = ph / 2.0;
        match &pill {
            Pill::Offer { .. } => {
                g.fill_round(r, radius, Color::white(if hovering { 0.84 } else { 0.95 }));
                g.text_centered(&label, Font::Button, gfx::text_box(r, Font::Button), style::ink());
            }
            Pill::Busy { progress, .. } => {
                g.fill_round(r, radius, Color::white(0.05));
                g.push_round_clip(r, radius);
                g.fill_rect(D2D_RECT_F { right: r.left + (r.right - r.left) * progress.clamp(0.0, 1.0) as f32, ..r }, Color::white(0.14));
                g.pop_clip();
                g.stroke_round(r, radius, Color::white(0.16), 1.0);
                g.text_centered(&label, Font::Button, gfx::text_box(r, Font::Button), style::label());
            }
            Pill::Done(_) => {
                g.fill_round(r, radius, Color::white(0.05));
                g.stroke_round(r, radius, style::online(), 1.0);
                g.text_centered(&label, Font::Button, gfx::text_box(r, Font::Button), style::online());
                if !h.done_timer {
                    h.done_timer = true;
                    unsafe {
                        SetTimer(Some(h.hwnd), TIMER_DONE, 6_000, None);
                    }
                }
            }
            _ => {
                g.fill_round(r, radius, Color::white(if hovering { 0.1 } else { 0.05 }));
                g.stroke_round(r, radius, Color(1.0, 0.45, 0.4, 0.8), 1.0);
                g.text_centered(&label, Font::Button, gfx::text_box(r, Font::Button), style::label());
            }
        }
    }

    let entries = h.directory.entries();
    let grid_top = title_y + 44.0 + 24.0;
    // A failed update says why down here, unless something more recent is being said.
    let note = match (&pill, h.message.is_empty()) {
        (Pill::Failed(why), true) => why.clone(),
        _ => h.message.clone(),
    };
    let message_h = if note.is_empty() { 0.0 } else { 34.0 };
    let grid = rect(MARGIN, grid_top, (width - 2.0 * MARGIN).max(100.0), (height - grid_top - message_h).max(50.0));
    h.hits.clear();

    if entries.is_empty() {
        // Nothing yet: what to do next.
        let cy = grid.top + (grid.bottom - grid.top) / 2.0 - 40.0;
        g.mark(rect(width / 2.0 - 32.0, cy - 60.0, 64.0, 51.0), 0.25, style::background());
        g.text_centered("No computers yet", Font::Heading, rect(0.0, cy + 6.0, width, 24.0), style::label());
        let (one, two) = if h.directory.tailscale_running() {
            ("Turn on Share This Mac in bb1 on a Mac signed in to your Tailscale,", "or add one by its address with +.")
        } else {
            ("Tailscale isn't running on this PC.", "Start it, or add a Mac by its address with +.")
        };
        g.text_centered(one, Font::Small, rect(0.0, cy + 34.0, width, 20.0), Color::white(0.5));
        g.text_centered(two, Font::Small, rect(0.0, cy + 52.0, width, 20.0), Color::white(0.5));
    } else {
        // Cards stay about the same size; a wider window fits more of them across.
        let gw = grid.right - grid.left;
        let columns = (((gw + GAP) / (360.0 + GAP)).floor() as usize).max(1);
        let tw = ((gw - (columns as f32 - 1.0) * GAP) / columns as f32).floor();
        let th = (tw * 10.0 / 16.0).floor() + FOOTER;
        let rows = entries.len().div_ceil(columns);
        let content = rows as f32 * (th + GAP);
        h.scroll = h.scroll.clamp(0.0, (content - (grid.bottom - grid.top) + 8.0).max(0.0));
        g.push_rect_clip(rect(0.0, grid.top - 4.0, width, grid.bottom - grid.top + 4.0));
        for (i, e) in entries.iter().enumerate() {
            let (col, row) = (i % columns, i / columns);
            let r = rect(grid.left + col as f32 * (tw + GAP), grid.top + row as f32 * (th + GAP) - h.scroll, tw, th);
            let preview = h.directory.preview(&e.name);
            let m = model(e, &h.directory, preview.as_ref());
            draw_tile(g, r, &m, preview.as_ref(), h.hover == Hover::Tile(i));
            h.hits.push(TileHit { rect: r, entry: e.clone(), can_act: m.action.is_some() });
        }
        g.pop_rect_clip();
    }

    if !note.is_empty() {
        g.text(&note, Font::Small, line_box(MARGIN, height - 28.0, width - 2.0 * MARGIN, Font::Small), Color::white(0.6));
    }
    g.end();
}

fn paint_settings(g: &mut Gfx, acts: &mut Acts, scroll: &mut f32, dropdown: bool, width: f32, height: f32, hover: Option<&Act>) {
    let top = 26.0;
    // "‹ Settings": the whole heading goes back to Your Macs.
    let title_w = g.measure("Settings", Font::Title);
    let back = rect(MARGIN - 8.0, top, title_w + 44.0, 44.0);
    let back_hover = hover == Some(&Act::Back);
    if back_hover {
        g.fill_round(back, 10.0, Color::white(0.06));
    }
    let (cx, cy) = (MARGIN + 6.0, top + 24.0);
    g.line(cx + 5.0, cy - 9.0, cx - 3.0, cy, Color::white(0.8), 2.5);
    g.line(cx - 3.0, cy, cx + 5.0, cy + 9.0, Color::white(0.8), 2.5);
    g.text("Settings", Font::Title, rect(MARGIN + 24.0, top, 400.0, 44.0), Color::white(1.0));
    acts.push((back, Act::Back));
    let content_top = top + 44.0 + 24.0;
    let w = (width - 2.0 * MARGIN).clamp(300.0, 760.0);
    g.push_rect_clip(rect(0.0, content_top - 6.0, width, (height - content_top + 6.0).max(1.0)));
    let used = panels::settings_page(g, MARGIN, content_top - *scroll, w, acts, hover, dropdown);
    g.pop_rect_clip();
    *scroll = scroll.clamp(0.0, (used - (height - content_top) + 24.0).max(0.0));
    // Scrolled out of sight: not clickable.
    acts.retain(|(r, a)| *a == Act::Back || r.bottom > content_top);
    g.end();
}

/// A small gear: a ring with teeth.
fn draw_gear(g: &mut Gfx, r: D2D_RECT_F, hovering: bool) {
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    if hovering {
        g.fill_circle(cx, cy, 17.0, Color::white(0.08));
    }
    let c = Color::white(if hovering { 0.85 } else { 0.55 });
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::PI / 4.0;
        g.line(cx + a.cos() * 6.0, cy + a.sin() * 6.0, cx + a.cos() * 9.5, cy + a.sin() * 9.5, c, 3.0);
    }
    g.fill_circle(cx, cy, 6.5, c);
    g.fill_circle(cx, cy, 3.0, style::background());
}

fn draw_tile(g: &mut Gfx, r: D2D_RECT_F, m: &Model, preview: Option<&directory::Preview>, hovering: bool) {
    let can_act = m.action.is_some();
    g.fill_round(r, 16.0, if hovering { style::card_hover() } else { style::card() });
    // The screen: the Mac's picture, or the bb1 mark until there is one.
    let screen = D2D_RECT_F { bottom: r.bottom - FOOTER, ..r };
    g.push_round_clip(r, 16.0);
    g.fill_rect(screen, style::screen());
    match preview.and_then(|p| g.jpeg(&m.name, p.version, &p.jpeg, m.dim)) {
        Some((bitmap, bw, bh)) => g.picture_fill(&bitmap, bw, bh, screen),
        None => {
            let (cx, cy) = ((screen.left + screen.right) / 2.0, (screen.top + screen.bottom) / 2.0);
            g.mark(rect(cx - 21.0, cy - 16.5, 42.0, 33.0), 0.16, style::screen());
        }
    }
    g.pop_clip();
    if hovering && can_act {
        g.stroke_round(r, 16.0, Color::white(0.75), 1.5);
    } else {
        g.stroke_round(r, 16.0, style::card_border(), 1.0);
    }

    // Footer: name, status, and Connect on hover.
    let button_space = if hovering && can_act { 110.0 } else { 0.0 };
    let text_w = (r.right - r.left) - 36.0 - button_space;
    g.text(&m.name, Font::Name, line_box(r.left + 18.0, screen.bottom + 11.0, text_w, Font::Name), style::label());
    let sy = screen.bottom + 34.0;
    let mut sx = r.left + 18.0;
    if m.live {
        g.fill_circle(sx + 3.5, sy + 9.0, 3.5, style::online());
        sx += 12.0;
    }
    g.text(&m.status, Font::Small, line_box(sx, sy, text_w - (sx - r.left - 18.0), Font::Small), Color::white(if m.live { 0.6 } else { 0.4 }));
    if hovering {
        if let Some(action) = m.action {
            g.pill(action, r.right - 16.0, r.bottom - FOOTER / 2.0, true, false, true);
        }
    }
}

fn hit(h: &Home, x: f32, y: f32) -> Hover {
    // Later ones are drawn on top (the open muted-apps list).
    if let Some(i) = h.acts.iter().rposition(|(r, _)| contains(r, x, y)) {
        return Hover::Act(i);
    }
    if h.page == Page::Settings {
        return Hover::None;
    }
    if contains(&h.plus, x, y) {
        return Hover::Plus;
    }
    if contains(&h.pill, x, y) {
        return Hover::Pill;
    }
    h.hits.iter().position(|t| contains(&t.rect, x, y)).map(Hover::Tile).unwrap_or(Hover::None)
}

fn dips(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let dpi = crate::win::util::dpi(hwnd) as f32;
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x * 96.0 / dpi, y * 96.0 / dpi)
}

fn connect(e: &Entry) {
    let Some(address) = e.address.clone() else { return };
    set_status("");
    app::connect_to(&address, &e.name);
}

fn tile_menu(hwnd: HWND, e: Entry) {
    let known = e.address.as_deref().and_then(|a| KnownHost::find(a, &e.name)).or_else(|| KnownHost::find("", &e.name));
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let can_connect = e.online && e.address.is_some();
        let _ = AppendMenuW(menu, if can_connect { MF_STRING } else { MF_STRING | MF_GRAYED }, 1, w!("Connect"));
        if known.is_some() {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 2, w!("Forget pairing"));
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match choice.0 {
            1 => connect(&e),
            2 => {
                if let Some(k) = known {
                    KnownHost::forget(&k.host_id);
                    set_status(&format!("Forgot the pairing with {}. It asks for a code (or Tailscale) next time.", k.name));
                    if let Some(d) = with(|h| h.directory.clone()) {
                        d.refresh_now();
                    }
                }
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        app::WM_APP_EVENT => {
            app::drain_events();
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(paint);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            invalidate();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(hwnd, lparam);
            let changed = with(|h| {
                let new = hit(h, x, y);
                let changed = new != h.hover;
                h.hover = new;
                changed
            })
            .unwrap_or(false);
            unsafe {
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut t);
            }
            if changed {
                invalidate();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(|h| h.hover = Hover::None);
            invalidate();
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                let hand = with(|h| match h.hover {
                    Hover::Plus => true,
                    Hover::Pill => h.pill_clickable,
                    Hover::Act(_) => true,
                    Hover::Tile(i) => h.hits.get(i).is_some_and(|t| t.can_act),
                    Hover::None => false,
                })
                .unwrap_or(false);
                unsafe {
                    SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(hwnd, lparam);
            // The sharing panel, Settings and the gear.
            let clicked = with(|h| match hit(h, x, y) {
                Hover::Act(i) => Some(h.acts[i].1.clone()),
                _ => {
                    // A click anywhere else closes the muted-apps list.
                    h.dropdown = false;
                    None
                }
            })
            .flatten();
            if let Some(act) = clicked {
                match act {
                    Act::Gear => {
                        with(|h| {
                            h.page = Page::Settings;
                            h.scroll = 0.0;
                            h.dropdown = false;
                        });
                    }
                    Act::Back => {
                        with(|h| {
                            h.page = Page::Macs;
                            h.scroll = 0.0;
                            h.dropdown = false;
                        });
                    }
                    Act::Dropdown => {
                        with(|h| h.dropdown = !h.dropdown);
                    }
                    other => {
                        let keep_open = panels::perform(&other);
                        if !keep_open {
                            with(|h| h.dropdown = false);
                        }
                    }
                }
                invalidate();
                return LRESULT(0);
            }
            invalidate();
            if with(|h| hit(h, x, y) == Hover::Pill && h.pill_clickable).unwrap_or(false) {
                start_update();
                return LRESULT(0);
            }
            let target = with(|h| match hit(h, x, y) {
                Hover::Plus => Some(None),
                Hover::Tile(i) => h.hits.get(i).filter(|t| t.can_act).map(|t| Some(t.entry.clone())),
                Hover::None | Hover::Pill | Hover::Act(_) => None,
            })
            .flatten();
            match target {
                Some(None) => ask_address(),
                Some(Some(e)) => connect(&e),
                None => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = dips(hwnd, lparam);
            let entry = with(|h| match hit(h, x, y) {
                Hover::Tile(i) => h.hits.get(i).map(|t| t.entry.clone()),
                _ => None,
            })
            .flatten();
            if let Some(e) = entry {
                tile_menu(hwnd, e);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
            with(|h| h.scroll -= delta / 120.0 * 60.0);
            invalidate();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_TICK => {
            invalidate(); // "seen 5 min ago" moves on
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_DONE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_DONE);
            }
            if let Some(u) = with(|h| h.updater.clone()) {
                u.dismiss_done();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let rc = unsafe { &*(lparam.0 as *const RECT) };
            with(|h| {
                if let Some(g) = h.gfx.as_mut() {
                    g.target = None;
                }
            });
            unsafe {
                let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: scale(hwnd, 480), y: scale(hwnd, 400) };
            LRESULT(0)
        }
        WM_CLOSE => unsafe {
            // bb1 keeps running by the clock (sharing, updates); Quit is in the icon's menu.
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(d) = with(|h| h.directory.clone()) {
                d.set_active(false);
            }
            crate::win::tray::told_still_running();
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// MARK: Prompts (connect by address, pairing code)

enum PromptKind {
    Address,
    Code(Option<Sender<Option<String>>>),
}

struct Prompt {
    hwnd: HWND,
    edit: HWND,
    kind: PromptKind,
    title: String,
    detail: String,
    ok_label: &'static str,
    gfx: Option<Gfx>,
    hover: Option<bool>, // Some(true) = OK, Some(false) = Cancel
    ok: D2D_RECT_F,
    cancel: D2D_RECT_F,
    font: HFONT,
    field_brush: HBRUSH,
}

const PROMPT_W: i32 = 420;
const PROMPT_H: i32 = 200;

fn ask_address() {
    open_prompt(PromptKind::Address, "Connect by address".into(), "A Mac's Tailscale IP or name, or its home network address.".into(), "Connect");
}

/// Asks for the 6-digit code the Mac shows; `reply` gets it (None = cancelled).
pub fn ask_code(host: &str, reply: Sender<Option<String>>) {
    open_prompt(PromptKind::Code(Some(reply)), format!("Pair with {host}"), "Type the 6-digit code shown on the Mac.".into(), "Pair");
}

fn open_prompt(kind: PromptKind, title: String, detail: String, ok_label: &'static str) {
    close_prompt(false);
    register(w!("bb1Prompt"), Some(prompt_proc));
    let owner = crate::win::stream::hwnd().or_else(|| with(|h| h.hwnd));
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let hwnd = CreateWindowExW(WS_EX_DLGMODALFRAME, w!("bb1Prompt"), &HSTRING::from(title.as_str()), style, CW_USEDEFAULT, CW_USEDEFAULT, 400, 200, owner, None, Some(instance.into()), None)
            .unwrap_or_default();
        gfx::dark_title_bar(hwnd);
        // The client area in points at this DPI, centred on the owner.
        let mut rc = RECT { left: 0, top: 0, right: scale(hwnd, PROMPT_W), bottom: scale(hwnd, PROMPT_H) };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_DLGMODALFRAME);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut orc = RECT::default();
        match owner {
            Some(o) if IsWindowVisible(o).as_bool() => {
                let _ = GetWindowRect(o, &mut orc);
            }
            _ => {
                let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut orc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            }
        }
        let _ = SetWindowPos(hwnd, None, (orc.left + orc.right - w) / 2, (orc.top + orc.bottom - h) / 2, w, h, SWP_NOZORDER);
        let numeric = matches!(kind, PromptKind::Code(_));
        let edit_style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE((ES_AUTOHSCROLL | if numeric { ES_NUMBER | ES_CENTER } else { 0 }) as u32);
        let edit = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), edit_style, 0, 0, 10, 10, Some(hwnd), Some(HMENU(100 as _)), Some(instance.into()), None)
            .unwrap_or_default();
        let font = ui_font(hwnd, if numeric { 18 } else { 11 }, numeric);
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        if numeric {
            SendMessageW(edit, 0x00C5 /* EM_LIMITTEXT */, Some(WPARAM(6)), None);
        } else if let Some(last) = app::settings().last_address {
            crate::win::util::set_text(edit, &last);
            SendMessageW(edit, 0x00B1 /* EM_SETSEL */, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        // The text field sits inside a drawn rounded box (see paint_prompt).
        let (fx, fy, fw, fh) = if numeric { (36, 92, 348, 30) } else { (36, 97, 348, 22) };
        let _ = MoveWindow(edit, scale(hwnd, fx), scale(hwnd, fy), scale(hwnd, fw), scale(hwnd, fh), false);
        let field_brush = CreateSolidBrush(COLORREF(gfx::colorref(gfx::graphite(0.10))));
        let prompt = Prompt {
            hwnd,
            edit,
            kind,
            title,
            detail,
            ok_label,
            gfx: Gfx::new().ok(),
            hover: None,
            ok: rect(0.0, 0.0, 0.0, 0.0),
            cancel: rect(0.0, 0.0, 0.0, 0.0),
            font,
            field_brush,
        };
        with(|h| h.prompt = Some(prompt));
        if let Some(o) = owner {
            let _ = EnableWindow(o, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(edit));
    }
}

/// Closes the prompt; `ok` answers with what was typed.
fn close_prompt(ok: bool) {
    let Some(Some(p)) = with(|h| h.prompt.take()) else { return };
    let text = get_text(p.edit).trim().to_string();
    let mut connect_to = None;
    match p.kind {
        PromptKind::Code(Some(reply)) => {
            let _ = reply.send((ok && !text.is_empty()).then_some(text));
        }
        PromptKind::Code(None) => {}
        PromptKind::Address => {
            if ok && !text.is_empty() {
                connect_to = Some(text);
            }
        }
    }
    unsafe {
        if let Ok(owner) = GetWindow(p.hwnd, GW_OWNER) {
            let _ = EnableWindow(owner, true);
            if connect_to.is_none() {
                let _ = SetForegroundWindow(owner);
            }
        }
        let _ = DestroyWindow(p.hwnd);
        let _ = DeleteObject(p.font.into());
        let _ = DeleteObject(p.field_brush.into());
    }
    if let Some(address) = connect_to {
        set_status("");
        app::connect_to(&address, "");
    }
}

fn paint_prompt(h: &mut Home) {
    let Some(p) = h.prompt.as_mut() else { return };
    let Some(g) = p.gfx.as_mut() else { return };
    let Ok((width, height)) = g.begin(p.hwnd) else { return };
    g.clear(gfx::graphite(0.06));
    g.text(&p.title, Font::Heading, line_box(30.0, 22.0, width - 60.0, Font::Heading), style::label());
    g.text(&p.detail, Font::Small, line_box(30.0, 50.0, width - 60.0, Font::Small), Color::white(0.55));
    let numeric = matches!(p.kind, PromptKind::Code(_));
    let field = if numeric { rect(24.0, 84.0, width - 48.0, 46.0) } else { rect(24.0, 88.0, width - 48.0, 40.0) };
    g.fill_round(field, 10.0, gfx::graphite(0.10));
    g.stroke_round(field, 10.0, Color::white(0.16), 1.0);
    let by = height - 34.0;
    p.ok = g.pill(p.ok_label, width - 24.0, by, true, p.hover == Some(true), false);
    p.cancel = g.pill("Cancel", p.ok.left - 10.0, by, false, p.hover == Some(false), false);
    g.end();
}

unsafe extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(paint_prompt);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLOREDIT => {
            let brush = with(|h| h.prompt.as_ref().map(|p| p.field_brush)).flatten().unwrap_or_default();
            unsafe {
                let hdc = HDC(wparam.0 as _);
                SetTextColor(hdc, COLORREF(gfx::colorref(Color::white(0.92))));
                SetBkColor(hdc, COLORREF(gfx::colorref(gfx::graphite(0.10))));
            }
            LRESULT(brush.0 as isize)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as i32 {
                1 => close_prompt(true),  // IDOK (Enter)
                2 => close_prompt(false), // IDCANCEL (Esc)
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(hwnd, lparam);
            let changed = with(|h| {
                let Some(p) = h.prompt.as_mut() else { return false };
                let new = if contains(&p.ok, x, y) { Some(true) } else if contains(&p.cancel, x, y) { Some(false) } else { None };
                let changed = new != p.hover;
                p.hover = new;
                changed
            })
            .unwrap_or(false);
            if changed {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(hwnd, lparam);
            let choice = with(|h| {
                h.prompt.as_ref().and_then(|p| if contains(&p.ok, x, y) { Some(true) } else if contains(&p.cancel, x, y) { Some(false) } else { None })
            })
            .flatten();
            if let Some(ok) = choice {
                close_prompt(ok);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                let over = with(|h| h.prompt.as_ref().is_some_and(|p| p.hover.is_some())).unwrap_or(false);
                unsafe {
                    SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok());
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CLOSE => {
            close_prompt(false);
            LRESULT(0)
        }
        // Destroyed along with its owner (the stream window closed while it was up): answer
        // "cancelled" so a connection waiting for a code isn't left waiting forever.
        WM_DESTROY => {
            let gone = with(|h| if h.prompt.as_ref().is_some_and(|p| p.hwnd == hwnd) { h.prompt.take() } else { None }).flatten();
            if let Some(p) = gone {
                if let PromptKind::Code(Some(reply)) = p.kind {
                    let _ = reply.send(None);
                }
                unsafe {
                    let _ = DeleteObject(p.font.into());
                    let _ = DeleteObject(p.field_brush.into());
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Online Macs that sign in quietly and run an older build than the newest release (with their build).
fn macs_behind(dir: &Directory, updater: &Updater) -> Vec<(Target, i64)> {
    let Some(latest) = updater.latest() else { return Vec::new() };
    dir.entries()
        .into_iter()
        .filter(|e| e.online && e.quiet)
        .filter_map(|e| {
            // Builds from before self-update report 0; they're updated by hand once.
            // The newer of what the list saw and what the Mac said after updating.
            let build = dir.build(&e.name).max(updater.known_build(&e.name)).filter(|&b| b > 0 && b < latest.build)?;
            let target = Target::new(e.address.as_deref()?, &e.name);
            (!matches!(target.auth, Auth::Code)).then_some((target, build))
        })
        .collect()
}

/// The pill was clicked: update the Macs that are behind, then this PC.
fn start_update() {
    let Some((dir, updater)) = with(|h| (h.directory.clone(), h.updater.clone())) else { return };
    let macs = macs_behind(&dir, &updater);
    set_status("");
    updater.update(
        macs,
        |installer| {
            let r = launch_installer(installer);
            if r.is_ok() {
                // The installer closes bb1 too; quit first so it doesn't have to.
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            }
            r
        },
        crate::win::selfupdate::quiet_install(),
    );
    invalidate();
}

/// Run the installer quietly; it asks for admin, then opens the new bb1 when it's done.
fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::ShellExecuteW;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let file = HSTRING::from(path.as_os_str());
        let r = ShellExecuteW(None, w!("runas"), &file, w!("/S"), None, SW_SHOWNORMAL);
        let code = r.0 as isize;
        if code > 32 {
            Ok(())
        } else if code == 5 || code == 1223 {
            Err("The update needs your OK to install. Try again.".into())
        } else {
            Err(format!("Couldn't start the installer (error {code})"))
        }
    }
}
