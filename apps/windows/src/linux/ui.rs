//! The main window (egui): your computers (Tailscale, home network, paired), connecting to one,
//! sharing this computer, and settings. The stream itself runs in its own process (stream.rs),
//! which reports back over its stdout and gets pairing codes on its stdin.

use crate::core::directory::Directory;
use crate::core::host::{Access, HostEvents, Viewer};
use crate::core::keymap::ModifierLayout;
use crate::core::store::{self, Settings};
use crate::linux::sharing::Sharing;
use crate::logi;
use eframe::egui;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The stream window's process, and what it last said.
struct StreamProc {
    label: String,
    child: Child,
    stdin: Option<ChildStdin>,
    state: Arc<Mutex<StreamState>>,
}

#[derive(Default)]
struct StreamState {
    status: String,
    connected: bool,
    asking_code: Option<String>,
    ended: Option<String>,
}

enum Share {
    Off,
    Starting,
    On(Sharing),
    Failed(String),
}

#[derive(Default)]
struct HostView {
    pairing: Option<(String, String)>,
    viewers: Vec<String>,
}

struct UiHostEvents {
    view: Arc<Mutex<HostView>>,
    ctx: egui::Context,
}

impl HostEvents for UiHostEvents {
    fn pairing_code(&self, device: &str, code: &str) {
        self.view.lock().unwrap().pairing = Some((device.to_string(), code.to_string()));
        crate::linux::app::notify(&format!("{device} wants to connect"), &format!("Type this code on it: {code}"));
        self.ctx.request_repaint();
    }
    fn pairing_finished(&self, _paired: bool) {
        self.view.lock().unwrap().pairing = None;
        self.ctx.request_repaint();
    }
    fn viewers_changed(&self, names: &[String]) {
        self.view.lock().unwrap().viewers = names.to_vec();
        self.ctx.request_repaint();
    }
}

struct App {
    settings: Settings,
    directory: Directory,
    address: String,
    code: String,
    stream: Option<StreamProc>,
    last_ended: Option<String>,
    share: Arc<Mutex<Share>>,
    host_view: Arc<Mutex<HostView>>,
    me: String,
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("bb1").with_app_id("bb1").with_inner_size([520.0, 640.0]).with_min_inner_size([420.0, 420.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "bb1",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            let directory = Directory::start(move || ctx.request_repaint());
            let settings = Settings::load();
            let mut app = App {
                address: settings.last_address.clone().unwrap_or_default(),
                settings,
                directory,
                code: String::new(),
                stream: None,
                last_ended: None,
                share: Arc::new(Mutex::new(Share::Off)),
                host_view: Arc::default(),
                me: store::device_name(),
            };
            if app.settings.sharing {
                app.start_sharing(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        eprintln!("bb1: couldn't open its window: {e}");
    }
}

impl App {
    fn connect(&mut self, ctx: &egui::Context, address: &str, label: &str) {
        self.disconnect();
        self.last_ended = None;
        let exe = std::env::current_exe().unwrap_or_else(|_| "bb1".into());
        let child = Command::new(exe)
            .args(["--stream", address, "--label", label])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.last_ended = Some(format!("Couldn't start the stream window: {e}"));
                return;
            }
        };
        let state = Arc::new(Mutex::new(StreamState { status: format!("Connecting to {label}…"), ..Default::default() }));
        if let Some(out) = child.stdout.take() {
            let (state, ctx) = (state.clone(), ctx.clone());
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    let Some(rest) = line.strip_prefix('@') else { continue };
                    let (kind, text) = rest.split_once(' ').unwrap_or((rest, ""));
                    let mut s = state.lock().unwrap();
                    match kind {
                        "status" => {
                            s.status = text.to_string();
                            if !text.starts_with("Type the") {
                                s.asking_code = None;
                            }
                        }
                        "connected" => {
                            s.connected = true;
                            s.asking_code = None;
                            s.status = format!("Connected to {text}");
                        }
                        "ask-code" => s.asking_code = Some(text.to_string()),
                        "paired" => s.status = format!("Paired with {text}"),
                        "notice" => s.status = text.to_string(),
                        "ended" => s.ended = Some(text.to_string()),
                        _ => {}
                    }
                    drop(s);
                    ctx.request_repaint();
                }
                let mut s = state.lock().unwrap();
                if s.ended.is_none() {
                    s.ended = Some("The stream window closed.".into());
                }
                ctx.request_repaint();
            });
        }
        let stdin = child.stdin.take();
        self.stream = Some(StreamProc { label: label.to_string(), child, stdin, state });
        self.settings.last_address = Some(address.to_string());
        self.settings.save();
    }

    fn disconnect(&mut self) {
        if let Some(mut s) = self.stream.take() {
            // SIGTERM: the stream window says goodbye to the host and closes.
            unsafe { libc_kill(s.child.id() as i32, 15) };
            std::thread::spawn(move || {
                let _ = s.child.wait();
            });
        }
    }

    fn send_code(&mut self) {
        let code: String = self.code.chars().filter(|c| c.is_ascii_digit()).collect();
        if let Some(s) = &mut self.stream
            && let Some(stdin) = s.stdin.as_mut()
        {
            let _ = writeln!(stdin, "{code}");
            let _ = stdin.flush();
            s.state.lock().unwrap().asking_code = None;
        }
        self.code.clear();
    }

    fn start_sharing(&mut self, ctx: &egui::Context) {
        *self.share.lock().unwrap() = Share::Starting;
        let (share, view, settings, ctx) = (self.share.clone(), self.host_view.clone(), self.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let events = Arc::new(UiHostEvents { view, ctx: ctx.clone() });
            let result = Sharing::start(events, &settings, false);
            let mut s = share.lock().unwrap();
            match result {
                Ok(sharing) => {
                    if matches!(*s, Share::Starting) {
                        *s = Share::On(sharing);
                    } else {
                        sharing.stop(); // turned off while starting
                    }
                }
                Err(e) => {
                    logi!("host: couldn't share: {e}");
                    *s = Share::Failed(e);
                }
            }
            ctx.request_repaint();
        });
    }

    fn stop_sharing(&mut self) {
        let old = std::mem::replace(&mut *self.share.lock().unwrap(), Share::Off);
        if let Share::On(s) = old {
            s.stop();
        }
        *self.host_view.lock().unwrap() = HostView::default();
    }

    fn computers(&mut self, ui: &mut egui::Ui) {
        ui.heading("Your computers");
        ui.add_space(4.0);
        let mut entries = self.directory.entries();
        entries.retain(|e| !e.name.eq_ignore_ascii_case(&self.me));
        if entries.is_empty() {
            ui.label(if self.directory.tailscale_running() {
                "Looking for computers sharing with bb1…"
            } else {
                "Looking on the home network… (Tailscale isn't running, so only computers here show up.)"
            });
        }
        let busy = self.stream.as_ref().is_some_and(|s| s.state.lock().unwrap().ended.is_none());
        let mut connect_to = None;
        for e in &entries {
            ui.horizontal(|ui| {
                let dot = if e.online { egui::Color32::from_rgb(60, 190, 90) } else { egui::Color32::GRAY };
                ui.colored_label(dot, "●");
                ui.label(egui::RichText::new(&e.name).strong());
                if let Some(ms) = self.directory.latency_ms(&e.name) {
                    ui.weak(format!("{ms:.0} ms"));
                }
                if !e.quiet && e.online {
                    ui.weak("(asks for a code)");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(address) = &e.address
                        && e.online
                        && ui.add_enabled(!busy, egui::Button::new("Connect")).clicked()
                    {
                        connect_to = Some((address.clone(), e.name.clone()));
                    }
                });
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("Address or name (Tailscale IP, mac.local, …)").desired_width(300.0));
            let go = ui.add_enabled(!busy && !self.address.trim().is_empty(), egui::Button::new("Connect")).clicked()
                || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.address.trim().is_empty());
            if go && !busy {
                connect_to = Some((self.address.trim().to_string(), String::new()));
            }
        });
        if let Some((address, label)) = connect_to {
            self.connect(ui.ctx(), &address, &label);
        }

        // The stream's state.
        let mut ask = None;
        let mut ended_now = None;
        if let Some(s) = &self.stream {
            let st = s.state.lock().unwrap();
            if let Some(e) = &st.ended {
                ended_now = Some(e.clone());
            } else {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(format!("{} — {}", s.label, st.status));
                });
                ask = st.asking_code.clone();
            }
        }
        if let Some(e) = ended_now {
            self.stream = None;
            if e != "Disconnected." {
                self.last_ended = Some(e);
            }
        }
        if let Some(host) = ask {
            ui.group(|ui| {
                ui.label(format!("{host} shows a 6-digit code. Type it here:"));
                ui.horizontal(|ui| {
                    let f = ui.add(egui::TextEdit::singleline(&mut self.code).desired_width(120.0).font(egui::TextStyle::Heading));
                    f.request_focus();
                    let enter = f.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Pair").clicked() || enter {
                        self.send_code();
                    }
                    if ui.button("Cancel").clicked() {
                        self.code.clear();
                        self.send_code();
                    }
                });
            });
        }
        if self.stream.is_some() {
            if ui.button("Disconnect").clicked() {
                self.disconnect();
            }
        } else if let Some(e) = &self.last_ended {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::from_rgb(220, 120, 60), e);
        }
    }

    fn sharing(&mut self, ui: &mut egui::Ui) {
        ui.heading("Share this computer");
        ui.add_space(4.0);
        let state = match &*self.share.lock().unwrap() {
            Share::Off => 0,
            Share::Starting => 1,
            Share::On(_) => 2,
            Share::Failed(_) => 3,
        };
        let mut on = state == 1 || state == 2;
        if ui.checkbox(&mut on, format!("Share {} with my other computers", self.me)).changed() {
            self.settings.sharing = on;
            self.settings.save();
            if on {
                self.start_sharing(ui.ctx());
            } else {
                self.stop_sharing();
            }
        }
        match &*self.share.lock().unwrap() {
            Share::Starting => {
                ui.label("Waiting for permission: allow sharing the screen and remote control in the desktop's dialog (asked only once).");
            }
            Share::Failed(e) => {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 60), format!("Couldn't share: {e}"));
            }
            Share::On(s) => {
                let view = self.host_view.lock().unwrap();
                if let Some((device, code)) = &view.pairing {
                    ui.group(|ui| {
                        ui.label(format!("{device} wants to connect. Type this code on it:"));
                        ui.label(egui::RichText::new(code).size(34.0).monospace().strong());
                    });
                }
                let viewers: Vec<Viewer> = s.host().viewers();
                if viewers.is_empty() {
                    ui.weak("Sharing. Nobody is watching.");
                }
                let host = s.host().clone();
                for v in viewers {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&v.name).strong());
                        let (mut m, mut k, mut c) = (v.mouse, v.keyboard, v.controller);
                        if ui.checkbox(&mut m, "mouse").changed() {
                            host.set_access(v.id, Access::Mouse, m);
                        }
                        if ui.checkbox(&mut k, "keyboard").changed() {
                            host.set_access(v.id, Access::Keyboard, k);
                        }
                        if ui.checkbox(&mut c, "controller").changed() {
                            host.set_access(v.id, Access::Controller, c);
                        }
                        if ui.button("Disconnect").clicked() {
                            host.disconnect(v.id);
                        }
                    });
                }
                ui.weak("Sharing stops when this window closes. To share without a window: bb1 --host");
            }
            Share::Off => {}
        }
        let mut changed = false;
        changed |= ui.checkbox(&mut self.settings.allow_tailscale, "Computers on my Tailscale account connect without a code").changed();
        changed |= ui.checkbox(&mut self.settings.allow_pairing, "New computers can pair with a 6-digit code").changed();
        if changed {
            self.settings.save();
            if let Share::On(s) = &*self.share.lock().unwrap() {
                s.apply_settings(&self.settings);
            }
        }
    }

    fn preferences(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(4.0);
        let mut changed = false;
        changed |= ui.checkbox(&mut self.settings.sound, "Play the other computer's sound").changed();
        changed |= ui.checkbox(&mut self.settings.clipboard, "Share the clipboard").changed();
        let layout = ModifierLayout::from_id(&self.settings.modifier_layout);
        ui.horizontal(|ui| {
            ui.label("Mac's Command key is:");
            for (l, name) in [
                (ModifierLayout::Standard, "Super"),
                (ModifierLayout::MacPositions, "Alt (Mac key positions)"),
                (ModifierLayout::CtrlIsCommand, "Ctrl"),
            ] {
                if ui.radio(layout == l, name).clicked() {
                    self.settings.modifier_layout = l.id().to_string();
                    changed = true;
                }
            }
        });
        if changed {
            self.settings.save();
        }
        ui.add_space(4.0);
        ui.weak(format!(
            "In the stream window: Ctrl+Alt+Enter full screen, Ctrl+Alt+← / → the other screen. Settings apply to the next connection. Logs: {}",
            store::log_dir().display()
        ));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("bb1").size(22.0).strong());
                    ui.weak(crate::core::updater::build_name(crate::core::updater::BUILD));
                });
                ui.add_space(8.0);
                self.computers(ui);
                ui.add_space(14.0);
                ui.separator();
                self.sharing(ui);
                ui.add_space(14.0);
                ui.separator();
                self.preferences(ui);
            });
        });
        // Keep the computer list and the viewers fresh.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
        self.stop_sharing();
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
