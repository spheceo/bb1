//! The parts of the main window beyond the Mac tiles: the "Sharing this PC" panel (who's watching,
//! and what each may use) and the Settings page, in the style of the Mac app's (Dashboard.swift).
//! Everything clickable is recorded with its `Act` while painting, so clicks and hovering find it.

use crate::core::host::{Access, Viewer};
use crate::core::store;
use crate::win::app;
use crate::win::gfx::{Color, Font, Gfx, line_box, rect, style, text_box};
use crate::win::tray;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Gear,
    Back,
    Share(bool),
    Access(u32, Access, bool),
    Disconnect(u32),
    CancelPairing,
    AllowTailscale(bool),
    AllowPairing(bool),
    ForgetPaired,
    Layout(&'static str),
    Nightly(bool),
    OpenLogs,
    /// Open or close the muted-apps list.
    Dropdown,
    Mute(String, bool),
    /// Download and run the ViGEmBus installer (friends' controllers need it).
    InstallControllerDriver,
}

pub type Acts = Vec<(D2D_RECT_F, Act)>;

const ROW: f32 = 58.0;
const RADIUS: f32 = 14.0;

fn hovered(hover: Option<&Act>, act: &Act) -> bool {
    hover == Some(act)
}

/// A switch: green when on.
fn switch(g: &mut Gfx, right: f32, cy: f32, on: bool, hovering: bool) -> D2D_RECT_F {
    let r = rect(right - 40.0, cy - 12.0, 40.0, 24.0);
    let track = if on { style::online() } else { Color::white(if hovering { 0.24 } else { 0.16 }) };
    g.fill_round(r, 12.0, track);
    let x = if on { r.right - 12.0 } else { r.left + 12.0 };
    g.fill_circle(x, cy, 9.5, Color::white(if hovering { 0.92 } else { 1.0 }));
    r
}

/// A small pill that's either on (filled) or off (outlined).
fn toggle_pill(g: &mut Gfx, label: &str, right: f32, cy: f32, on: bool, hovering: bool) -> D2D_RECT_F {
    let h = 28.0;
    let w = g.measure(label, Font::Button) + 26.0;
    let r = rect(right - w, cy - h / 2.0, w, h);
    if on {
        g.fill_round(r, h / 2.0, Color::white(if hovering { 0.84 } else { 0.95 }));
        g.text_centered(label, Font::Button, text_box(r, Font::Button), style::ink());
    } else {
        g.fill_round(r, h / 2.0, Color::white(if hovering { 0.08 } else { 0.0 }));
        g.stroke_round(r, h / 2.0, Color::white(0.22), 1.0);
        g.text_centered(label, Font::Button, text_box(r, Font::Button), Color::white(0.55));
    }
    r
}

// MARK: Sharing this PC

/// The panel at the top of Your Macs while sharing. Returns its height.
pub fn sharing_panel(g: &mut Gfx, x: f32, y: f32, w: f32, acts: &mut Acts, hover: Option<&Act>) -> f32 {
    let viewers: Vec<Viewer> = tray::viewers();
    let pairing = tray::pairing();
    let mut cy = y;
    // Heading: a green dot, "Sharing this PC", and Stop sharing on the right.
    g.fill_circle(x + 6.0, cy + 17.0, 5.0, style::online());
    g.text("Sharing this PC", Font::Title, rect(x + 20.0, cy - 4.0, 500.0, 44.0), Color::white(1.0));
    let stop = Act::Share(false);
    let after = x + 20.0 + g.measure("Sharing this PC", Font::Title) + 18.0;
    let stop_w = g.measure("Stop sharing", Font::Button) + 22.0;
    let r = g.pill("Stop sharing", after + stop_w, cy + 20.0, false, hovered(hover, &stop), true);
    acts.push((r, stop));
    cy += 50.0;

    if let Some((device, code)) = &pairing {
        let card = rect(x, cy, w, ROW + 8.0);
        g.fill_round(card, RADIUS, style::card());
        g.stroke_round(card, RADIUS, style::online(), 1.0);
        g.text(&format!("{device} wants to connect"), Font::Name, line_box(x + 20.0, cy + 12.0, w - 300.0, Font::Name), style::label());
        g.text("Type this code on it to let it in.", Font::Small, line_box(x + 20.0, cy + 35.0, w - 300.0, Font::Small), Color::white(0.55));
        let spaced = if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.clone() };
        let cancel = Act::CancelPairing;
        let r = g.pill("Don't let it in", x + w - 18.0, cy + (ROW + 8.0) / 2.0, false, hovered(hover, &cancel), true);
        acts.push((r, cancel));
        let cw = g.measure(&spaced, Font::Title);
        g.text(&spaced, Font::Title, rect(r.left - 24.0 - cw, cy + 10.0, cw + 4.0, 44.0), Color::white(1.0));
        cy += ROW + 8.0 + 12.0;
    }

    if viewers.is_empty() {
        g.text("Nobody's watching. Computers on your Tailscale account can connect now; others pair with a code.",
               Font::Small, line_box(x + 2.0, cy, w, Font::Small), Color::white(0.5));
        return cy + 26.0 - y;
    }
    for v in &viewers {
        let card = rect(x, cy, w, ROW);
        g.fill_round(card, RADIUS, style::card());
        g.stroke_round(card, RADIUS, style::card_border(), 1.0);
        let mid = cy + ROW / 2.0;
        // Right to left: Disconnect, then Controller, Keyboard, Mouse.
        let disconnect = Act::Disconnect(v.id);
        let r = g.pill("Disconnect", x + w - 16.0, mid, false, hovered(hover, &disconnect), true);
        acts.push((r, disconnect));
        let mut right = r.left - 20.0;
        for (what, on, label) in [(Access::Controller, v.controller, "Controller"), (Access::Keyboard, v.keyboard, "Keyboard"), (Access::Mouse, v.mouse, "Mouse")] {
            let act = Act::Access(v.id, what, !on);
            let r = toggle_pill(g, label, right, mid, on, hovered(hover, &act));
            acts.push((r, act));
            right = r.left - 8.0;
        }
        g.fill_circle(x + 24.0, mid, 4.0, style::online());
        g.text(&v.name, Font::Name, line_box(x + 38.0, mid - 19.0, (right - x - 50.0).max(40.0), Font::Name), style::label());
        let can: Vec<&str> = [(v.mouse, "mouse"), (v.keyboard, "keyboard"), (v.controller, "controller")]
            .into_iter().filter(|(on, _)| *on).map(|(_, n)| n).collect();
        let detail = if can.is_empty() { "Watching only".to_string() } else { format!("Watching · can use the {}", crate::core::updater::list(&can.iter().map(|s| s.to_string()).collect::<Vec<_>>())) };
        g.text(&detail, Font::Small, line_box(x + 38.0, mid + 2.0, (right - x - 50.0).max(40.0), Font::Small), Color::white(0.5));
        cy += ROW + 10.0;
    }
    cy - y
}

// MARK: Settings

/// Paints Settings. `top` is where content starts after the header; returns the content height.
pub fn settings_page(g: &mut Gfx, x: f32, top: f32, w: f32, acts: &mut Acts, hover: Option<&Act>, dropdown_open: bool) -> f32 {
    let s = app::settings();
    let mut y = top;

    let label = |g: &mut Gfx, ry: f32, title: &str, detail: &str| {
        g.text(title, Font::Name, line_box(x + 20.0, ry + (if detail.is_empty() { 19.0 } else { 10.0 }), w - 260.0, Font::Name), style::label());
        if !detail.is_empty() {
            g.text(detail, Font::Small, line_box(x + 20.0, ry + 32.0, w - 260.0, Font::Small), Color::white(0.5));
        }
    };
    let card = |g: &mut Gfx, y: f32, rows: usize| {
        let r = rect(x, y, w, rows as f32 * ROW);
        g.fill_round(r, RADIUS, style::card());
        g.stroke_round(r, RADIUS, style::card_border(), 1.0);
        for i in 1..rows {
            let ly = y + i as f32 * ROW;
            g.line(x + 20.0, ly, x + w - 20.0, ly, Color::white(0.06), 1.0);
        }
    };
    let heading = |g: &mut Gfx, y: &mut f32, title: &str, blurb: &str| {
        g.text(title, Font::Heading, line_box(x, *y, w, Font::Heading), style::label());
        g.text(blurb, Font::Small, line_box(x, *y + 22.0, w, Font::Small), Color::white(0.5));
        *y += 50.0;
    };

    // Sharing
    heading(g, &mut y, "Sharing", "Let your other computers see and use this PC. Needs an NVIDIA graphics card.");
    card(g, y, 3);
    let sharing = tray::is_sharing();
    let detail = match (sharing, tray::problem()) {
        (true, _) => "On: computers can connect from their bb1.".to_string(),
        (false, Some(p)) => p,
        (false, None) => "Off.".to_string(),
    };
    label(g, y, "Share this PC", &detail);
    let act = Act::Share(!sharing);
    let r = switch(g, x + w - 20.0, y + ROW / 2.0, sharing, hovered(hover, &act));
    acts.push((r, act));
    y += ROW;
    label(g, y, "Don't share sound from", "Browsers are muted whole: every tab's sound comes from one process.");
    let dropdown_anchor = Some(muted_trigger(g, x + w - 20.0, y + ROW / 2.0, &s.muted_apps, acts, hover));
    y += ROW;
    controllers_row(g, x, y, w, acts, hover, &label);
    y += ROW + 28.0;

    // Who can connect
    heading(g, &mut y, "Who can connect", "Computers you've paired, or on your Tailscale account, get in without asking.");
    card(g, y, 3);
    label(g, y, "Your Tailscale account", "Computers signed in to the same Tailscale connect by themselves.");
    let act = Act::AllowTailscale(!s.allow_tailscale);
    let r = switch(g, x + w - 20.0, y + ROW / 2.0, s.allow_tailscale, hovered(hover, &act));
    acts.push((r, act));
    y += ROW;
    label(g, y, "Pairing with a code", "A new computer gets in with a 6-digit code shown on this PC.");
    let act = Act::AllowPairing(!s.allow_pairing);
    let r = switch(g, x + w - 20.0, y + ROW / 2.0, s.allow_pairing, hovered(hover, &act));
    acts.push((r, act));
    y += ROW;
    label(g, y, "Paired computers", "They sign in without a code. Forgetting makes them pair again.");
    let act = Act::ForgetPaired;
    let r = g.pill("Forget all", x + w - 20.0, y + ROW / 2.0, false, hovered(hover, &act), true);
    acts.push((r, act));
    y += ROW + 28.0;

    // Keyboard
    heading(g, &mut y, "Keyboard", "When this PC controls a Mac.");
    card(g, y, 1);
    label(g, y, "The ⌘ key is", "");
    let mut right = x + w - 20.0;
    for (id, name) in [("ctrl", "Ctrl"), ("mac", "Alt"), ("standard", "Windows key")] {
        let act = Act::Layout(id);
        let r = toggle_pill(g, name, right, y + ROW / 2.0, s.modifier_layout == id, hovered(hover, &act));
        acts.push((r, act));
        right = r.left - 8.0;
    }
    y += ROW + 28.0;

    // Updates
    heading(g, &mut y, "Updates", "New builds arrive inside the app (the pill at the top of Your Macs).");
    card(g, y, 1);
    label(g, y, "Nightly builds", if s.nightly { "Every new build, as it's made." } else { "Releases only." });
    let act = Act::Nightly(!s.nightly);
    let r = switch(g, x + w - 20.0, y + ROW / 2.0, s.nightly, hovered(hover, &act));
    acts.push((r, act));
    y += ROW + 28.0;

    // Help
    heading(g, &mut y, "Help", "For when something isn't right.");
    card(g, y, 1);
    let build = crate::core::updater::BUILD;
    label(g, y, "Logs", &format!("bb1 {} (build {build}). Your other devices on Tailscale can read them too.", crate::core::updater::build_name(build)));
    let act = Act::OpenLogs;
    let r = g.pill("Open folder", x + w - 20.0, y + ROW / 2.0, false, hovered(hover, &act), true);
    acts.push((r, act));
    y += ROW + 28.0;

    // The muted-apps list opens over everything else, so it's drawn (and hit-tested) last.
    if dropdown_open {
        if let Some(anchor) = dropdown_anchor {
            muted_list(g, anchor, &s.muted_apps, acts, hover);
        }
    }
    y - top
}

/// The button showing the muted apps' icons, overlapping a little, and their names.
fn muted_trigger(g: &mut Gfx, right: f32, cy: f32, muted: &[String], acts: &mut Acts, hover: Option<&Act>) -> D2D_RECT_F {
    let apps = crate::win::apps::known_apps();
    let shown: Vec<&crate::win::apps::MutableApp> = apps.iter().filter(|a| muted.iter().any(|m| m == a.id)).collect();
    let text = if shown.is_empty() { "None".to_string() } else { shown.iter().map(|a| a.name).collect::<Vec<_>>().join(", ") };
    let icon = 22.0;
    let icons_w = if shown.is_empty() { 0.0 } else { icon + (shown.len() as f32 - 1.0) * 14.0 + 8.0 };
    let w = icons_w + g.measure(&text, Font::Button) + 44.0;
    let h = 34.0;
    let r = rect(right - w, cy - h / 2.0, w, h);
    let act = Act::Dropdown;
    let hovering = hovered(hover, &act);
    g.fill_round(r, h / 2.0, Color::white(if hovering { 0.1 } else { 0.05 }));
    g.stroke_round(r, h / 2.0, Color::white(0.16), 1.0);
    let mut ix = r.left + 8.0;
    for a in &shown {
        // A ring in the card's colour separates each icon from the one under it.
        g.fill_circle(ix + icon / 2.0, cy, icon / 2.0 + 2.0, style::card());
        draw_icon(g, a, rect(ix, cy - icon / 2.0, icon, icon));
        ix += 14.0;
    }
    let tx = r.left + 10.0 + icons_w;
    g.text(&text, Font::Button, text_box(rect(tx, r.top, w, h), Font::Button), style::label());
    // A small chevron.
    let (chx, chy) = (r.right - 16.0, cy - 1.0);
    g.line(chx - 4.0, chy - 2.0, chx, chy + 2.0, Color::white(0.6), 1.5);
    g.line(chx, chy + 2.0, chx + 4.0, chy - 2.0, Color::white(0.6), 1.5);
    acts.push((r, act));
    r
}

fn draw_icon(g: &mut Gfx, a: &crate::win::apps::MutableApp, r: D2D_RECT_F) {
    match &a.icon {
        Some(i) => {
            if let Some((b, bw, bh)) = g.bgra(&format!("app-icon-{}", a.id), &i.bgra, i.width, i.height) {
                g.picture_fill(&b, bw, bh, r);
            }
        }
        None => {
            g.fill_round(r, 5.0, Color::white(0.2));
            let initial: String = a.name.chars().take(1).collect();
            g.text_centered(&initial, Font::Button, text_box(r, Font::Button), style::label());
        }
    }
}

/// The open list: each installed app with its icon and a check; clicking toggles and keeps it open.
fn muted_list(g: &mut Gfx, anchor: D2D_RECT_F, muted: &[String], acts: &mut Acts, hover: Option<&Act>) {
    let apps: Vec<crate::win::apps::MutableApp> = crate::win::apps::known_apps().into_iter().filter(|a| a.path.is_some()).collect();
    let row = 40.0;
    let w = 240.0f32.max(anchor.right - anchor.left);
    let h = (apps.len().max(1) as f32) * row + 12.0;
    let panel = rect(anchor.right - w, anchor.bottom + 6.0, w, h);
    g.fill_round(rect(panel.left, panel.top + 3.0, w, h), 12.0, Color(0.0, 0.0, 0.0, 0.35));
    g.fill_round(panel, 12.0, crate::win::gfx::graphite(0.11));
    g.stroke_round(panel, 12.0, Color::white(0.12), 1.0);
    if apps.is_empty() {
        g.text("Discord, Chrome and Helium aren't installed.", Font::Small, line_box(panel.left + 14.0, panel.top + 18.0, w - 28.0, Font::Small), Color::white(0.55));
        return;
    }
    for (i, a) in apps.iter().enumerate() {
        let on = muted.iter().any(|m| m == a.id);
        let r = rect(panel.left + 6.0, panel.top + 6.0 + i as f32 * row, w - 12.0, row);
        let act = Act::Mute(a.id.to_string(), !on);
        if hovered(hover, &act) {
            g.fill_round(r, 8.0, Color::white(0.08));
        }
        draw_icon(g, a, rect(r.left + 10.0, r.top + (row - 22.0) / 2.0, 22.0, 22.0));
        g.text(a.name, Font::Name, text_box(rect(r.left + 42.0, r.top, w - 90.0, row), Font::Name), style::label());
        if on {
            let (cx, cy) = (r.right - 18.0, r.top + row / 2.0);
            g.line(cx - 6.0, cy, cx - 2.0, cy + 4.0, style::online(), 2.0);
            g.line(cx - 2.0, cy + 4.0, cx + 6.0, cy - 5.0, style::online(), 2.0);
        }
        acts.push((r, act));
    }
}

/// Do what was clicked. Returns true when the muted-apps list should stay open.
pub fn perform(act: &Act) -> bool {
    match act.clone() {
        Act::Share(on) => tray::share(on),
        Act::Access(id, what, on) => tray::set_access(id, what, on),
        Act::Disconnect(id) => tray::disconnect(id),
        Act::CancelPairing => tray::cancel_pairing(),
        Act::AllowTailscale(on) => {
            app::update_settings(|s| s.allow_tailscale = on);
            tray::settings_changed();
        }
        Act::AllowPairing(on) => {
            app::update_settings(|s| s.allow_pairing = on);
            tray::settings_changed();
        }
        Act::ForgetPaired => crate::core::host::forget_paired_devices(),
        Act::Layout(id) => app::update_settings(|s| s.modifier_layout = id.to_string()),
        Act::Nightly(on) => app::update_settings(|s| s.nightly = on),
        Act::OpenLogs => open_folder(&store::log_dir()),
        Act::Mute(id, on) => {
            app::update_settings(|s| {
                s.muted_apps.retain(|m| *m != id);
                if on {
                    s.muted_apps.push(id.clone());
                }
            });
            tray::settings_changed();
            return true;
        }
        Act::InstallControllerDriver => install_controller_driver(),
        Act::Gear | Act::Back | Act::Dropdown => {}
    }
    false
}

// MARK: Game controllers (the ViGEmBus driver)

#[derive(Clone, PartialEq)]
enum DriverInstall {
    Idle,
    Downloading,
    /// Its installer is open.
    Installing,
    Failed(String),
}

static DRIVER_INSTALL: std::sync::Mutex<DriverInstall> = std::sync::Mutex::new(DriverInstall::Idle);

/// For the web UI: "ready" | "missing" | "downloading" | "installing" | "failed", and why it failed.
pub(crate) fn driver_status() -> (&'static str, Option<String>) {
    if crate::host::input::controller_driver_installed() {
        return ("ready", None);
    }
    match DRIVER_INSTALL.lock().unwrap().clone() {
        DriverInstall::Idle => ("missing", None),
        DriverInstall::Downloading => ("downloading", None),
        DriverInstall::Installing => ("installing", None),
        DriverInstall::Failed(e) => ("failed", Some(e)),
    }
}

/// "Game controllers": Ready once ViGEmBus is installed, otherwise a pill that installs it.
fn controllers_row(g: &mut Gfx, x: f32, y: f32, w: f32, acts: &mut Acts, hover: Option<&Act>, label: &dyn Fn(&mut Gfx, f32, &str, &str)) {
    let detail = "Friends' controllers become Xbox controllers on this PC.";
    let cy = y + ROW / 2.0;
    let right = x + w - 20.0;
    if crate::host::input::controller_driver_installed() {
        label(g, y, "Game controllers", detail);
        let tw = g.measure("Ready", Font::Button);
        g.text("Ready", Font::Button, line_box(right - tw - 2.0, cy - 9.0, tw + 4.0, Font::Button), Color::white(0.55));
        return;
    }
    let state = DRIVER_INSTALL.lock().unwrap().clone();
    let failed;
    let detail = match &state {
        DriverInstall::Failed(e) => {
            failed = format!("Couldn't install the controller driver: {e}");
            failed.as_str()
        }
        _ => detail,
    };
    label(g, y, "Game controllers", detail);
    match state {
        DriverInstall::Downloading | DriverInstall::Installing => {
            let text = if state == DriverInstall::Downloading { "Downloading…" } else { "Installing…" };
            g.pill(text, right, cy, false, false, true);
        }
        DriverInstall::Idle | DriverInstall::Failed(_) => {
            let act = Act::InstallControllerDriver;
            let r = g.pill(if matches!(state, DriverInstall::Idle) { "Install driver" } else { "Try again" }, right, cy, true, hovered(hover, &act), true);
            acts.push((r, act));
        }
    }
}

/// The official ViGEmBus installer, from its GitHub releases (it has its own window and asks for admin).
pub(crate) fn install_controller_driver() {
    {
        let mut st = DRIVER_INSTALL.lock().unwrap();
        if matches!(*st, DriverInstall::Downloading | DriverInstall::Installing) {
            return;
        }
        *st = DriverInstall::Downloading;
    }
    std::thread::spawn(|| {
        let result = download_and_run_vigem(|| {
            *DRIVER_INSTALL.lock().unwrap() = DriverInstall::Installing;
            app::post(app::UiEvent::HostsChanged);
        });
        if let Err(e) = &result {
            crate::logi!("controllers: driver install: {e}");
        }
        *DRIVER_INSTALL.lock().unwrap() = match result {
            Ok(()) => DriverInstall::Idle,
            Err(e) => DriverInstall::Failed(e),
        };
        app::post(app::UiEvent::HostsChanged); // repaint (and check for the driver again)
    });
}

fn download_and_run_vigem(started: impl Fn()) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    const FALLBACK: &str = "https://github.com/nefarius/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe";
    #[derive(serde::Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
    }
    let latest = crate::core::updater::fetch("https://api.github.com/repos/nefarius/ViGEmBus/releases/latest")
        .and_then(|j| serde_json::from_slice::<Release>(&j).ok())
        .and_then(|r| r.assets.into_iter().find(|a| a.name.ends_with(".exe") && a.name.contains("x64")));
    let (name, url, size) = match latest {
        Some(a) => (a.name, a.browser_download_url, a.size),
        None => ("ViGEmBus_1.22.0_x64_x86_arm64.exe".to_string(), FALLBACK.to_string(), 0),
    };
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).collect();
    let path = std::env::temp_dir().join(name);
    crate::logi!("controllers: downloading {url}");
    crate::core::updater::download(&url, &path, size, |_| {})?;
    started();
    let file = windows::core::HSTRING::from(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: windows::core::w!("runas"),
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| format!("the installer didn't start ({e})"))?;
        if !info.hProcess.is_invalid() {
            windows::Win32::System::Threading::WaitForSingleObject(info.hProcess, windows::Win32::System::Threading::INFINITE);
            let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        }
    }
    let _ = std::fs::remove_file(&path);
    crate::host::input::recheck_controller_driver();
    Ok(())
}

pub(crate) fn open_folder(path: &std::path::Path) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let p = windows::core::HSTRING::from(path.as_os_str());
    unsafe {
        let _ = ShellExecuteW(None, windows::core::w!("open"), &p, None, None, SW_SHOWNORMAL);
    }
}
