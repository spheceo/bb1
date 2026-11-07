//! The desktop portal (org.freedesktop.portal.RemoteDesktop + ScreenCast over D-Bus): how a Linux
//! app may share the screen and inject input on Wayland (and X11) without root.
//!
//! One session does both: SelectDevices (keyboard and pointer) and SelectSources (one monitor), then
//! Start shows the desktop's "Share screen / allow remote control" dialog. The answer comes with a
//! restore token, saved in the support folder, so the next session starts without asking again
//! (persist mode 2: until the user revokes it). OpenPipeWireRemote then gives the PipeWire socket the
//! screen's frames arrive on (capture.rs). Input goes through Notify* calls on the same session.
//!
//! Every portal call answers through a Request object's Response signal; `request` subscribes to it
//! before making the call, so the answer can't be missed.

use crate::core::store::support_dir;
use crate::logi;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const SCREEN: &str = "org.freedesktop.portal.ScreenCast";

/// RemoteDesktop device types.
const KEYBOARD: u32 = 1;
const POINTER: u32 = 2;
/// ScreenCast source type and cursor modes.
const MONITOR: u32 = 1;
pub const CURSOR_EMBEDDED: u32 = 2;
pub const CURSOR_METADATA: u32 = 4;

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn token() -> String {
    format!("bb1_{}_{}", std::process::id(), NEXT_TOKEN.fetch_add(1, Relaxed))
}

fn token_path() -> std::path::PathBuf {
    support_dir().join("portal-restore-token")
}

/// A started screen-share + remote-input session.
pub struct Portal {
    conn: Connection,
    remote: Proxy<'static>,
    session: OwnedObjectPath,
    /// The PipeWire node with the screen's frames.
    pub node_id: u32,
    /// The stream's size in the desktop's logical pixels (what absolute pointer positions are in).
    pub logical_size: (f64, f64),
    /// How the pointer comes with the frames (CURSOR_METADATA: beside them, drawn by capture.rs).
    pub cursor_mode: u32,
    /// Keyboard and pointer were granted.
    pub input: bool,
    fd: std::sync::Mutex<Option<OwnedFd>>,
}

impl Portal {
    /// Opens a session and starts it. The first time, the desktop asks the user (this blocks until
    /// they answer, at most `wait`).
    pub fn open(wait: Duration) -> Result<Portal, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("bb1 portal".into())
            .spawn(move || {
                let _ = tx.send(Self::open_now());
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(wait).map_err(|_| "the screen-sharing permission wasn't answered in time".to_string())?
    }

    fn open_now() -> Result<Portal, String> {
        let conn = Connection::session().map_err(|e| format!("no D-Bus session: {e}"))?;
        let remote = Proxy::new(&conn, DEST, PATH, REMOTE).map_err(|e| e.to_string())?;
        let screen = Proxy::new(&conn, DEST, PATH, SCREEN).map_err(|e| e.to_string())?;
        let version: u32 = remote.get_property("version").unwrap_or(0);
        if version == 0 {
            return Err("this desktop has no remote-desktop portal (xdg-desktop-portal with a GNOME or KDE backend)".into());
        }
        let cursor_modes: u32 = screen.get_property("AvailableCursorModes").unwrap_or(CURSOR_EMBEDDED);
        let cursor_mode = if cursor_modes & CURSOR_METADATA != 0 { CURSOR_METADATA } else { CURSOR_EMBEDDED };

        // CreateSession
        let session_token = token();
        let results = request(&conn, &remote, "CreateSession", |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            o.insert("session_handle_token", Value::from(session_token.clone()));
            Box::new(move |p: &Proxy| p.call_method("CreateSession", &(o,)).map(|_| ()))
        })?;
        let session: String = results
            .get("session_handle")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .ok_or("the portal didn't give a session")?;
        let session = OwnedObjectPath::try_from(session).map_err(|e| e.to_string())?;

        // SelectDevices (with the saved permission, so there's no dialog after the first time)
        let restore = std::fs::read_to_string(token_path()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let s = session.clone();
        let restore2 = restore.clone();
        request(&conn, &remote, "SelectDevices", move |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            o.insert("types", Value::from(KEYBOARD | POINTER));
            if version >= 2 {
                o.insert("persist_mode", Value::from(2u32));
                if let Some(r) = restore2.clone() {
                    o.insert("restore_token", Value::from(r));
                }
            }
            let s = s.clone();
            Box::new(move |p: &Proxy| p.call_method("SelectDevices", &(ObjectPath::from(&s), o)).map(|_| ()))
        })?;

        // SelectSources: one monitor, the pointer as metadata when the desktop can (so it can be left
        // out for clients that draw their own).
        let s = session.clone();
        request(&conn, &screen, "SelectSources", move |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            o.insert("types", Value::from(MONITOR));
            o.insert("multiple", Value::from(false));
            o.insert("cursor_mode", Value::from(cursor_mode));
            let s = s.clone();
            Box::new(move |p: &Proxy| p.call_method("SelectSources", &(ObjectPath::from(&s), o)).map(|_| ()))
        })?;

        // Start: the dialog, the first time.
        if restore.is_none() {
            logi!("portal: asking for permission to share the screen (the desktop shows a dialog)");
        }
        let s = session.clone();
        let results = request(&conn, &remote, "Start", move |t| {
            let mut o: HashMap<&str, Value> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_string()));
            let s = s.clone();
            Box::new(move |p: &Proxy| p.call_method("Start", &(ObjectPath::from(&s), "", o)).map(|_| ()))
        })?;
        if let Some(t) = results.get("restore_token").and_then(|v| String::try_from(v.try_clone().ok()?).ok()) {
            let _ = std::fs::write(token_path(), t);
        }
        let devices: u32 = results.get("devices").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> = results
            .get("streams")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(v).ok())
            .unwrap_or_default();
        let (node_id, props) = streams.into_iter().next().ok_or("no screen was chosen to share")?;
        let size: (i32, i32) = props.get("size").and_then(|v| v.try_clone().ok()).and_then(|v| <(i32, i32)>::try_from(v).ok()).unwrap_or((0, 0));

        let fd: zbus::zvariant::OwnedFd = screen
            .call("OpenPipeWireRemote", &(ObjectPath::from(&session), HashMap::<&str, Value>::new()))
            .map_err(|e| format!("couldn't open the screen's PipeWire stream: {e}"))?;
        logi!(
            "portal: sharing PipeWire node {node_id} ({}x{} logical), keyboard {}, pointer {}, pointer {}",
            size.0,
            size.1,
            devices & KEYBOARD != 0,
            devices & POINTER != 0,
            if cursor_mode == CURSOR_METADATA { "drawn by bb1" } else { "drawn by the desktop" }
        );
        Ok(Portal {
            conn,
            remote,
            session,
            node_id,
            logical_size: (size.0.max(1) as f64, size.1.max(1) as f64),
            cursor_mode,
            input: devices & (KEYBOARD | POINTER) != 0,
            fd: std::sync::Mutex::new(Some(std::os::fd::OwnedFd::from(fd))),
        })
    }

    /// The PipeWire socket for the screen (once).
    pub fn take_fd(&self) -> Option<OwnedFd> {
        self.fd.lock().unwrap().take()
    }

    fn notify<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) {
        if let Err(e) = self.remote.call_noreply(method, body) {
            logi!("portal: {method} failed: {e}");
        }
    }

    fn path(&self) -> ObjectPath<'_> {
        ObjectPath::from(&self.session)
    }

    /// `code`: a Linux input event code (KEY_*).
    pub fn key(&self, code: u32, down: bool) {
        self.notify("NotifyKeyboardKeycode", &(self.path(), HashMap::<&str, Value>::new(), code as i32, down as u32));
    }

    /// Logical pixels on the shared screen.
    pub fn pointer_to(&self, x: f64, y: f64) {
        self.notify("NotifyPointerMotionAbsolute", &(self.path(), HashMap::<&str, Value>::new(), self.node_id, x, y));
    }

    /// `button`: BTN_LEFT (0x110) and up.
    pub fn button(&self, button: i32, down: bool) {
        self.notify("NotifyPointerButton", &(self.path(), HashMap::<&str, Value>::new(), button, down as u32));
    }

    /// Smooth scrolling in logical pixels (positive: content moves up / left, like a wheel turned
    /// towards you). `finish`: the gesture ended (kinetic scrolling may follow).
    pub fn scroll(&self, dx: f64, dy: f64, finish: bool) {
        let mut o: HashMap<&str, Value> = HashMap::new();
        if finish {
            o.insert("finish", Value::from(true));
        }
        self.notify("NotifyPointerAxis", &(self.path(), o, dx, dy));
    }

    /// Wheel clicks: axis 0 vertical (positive: down), 1 horizontal (positive: right).
    pub fn scroll_steps(&self, axis: u32, steps: i32) {
        self.notify("NotifyPointerAxisDiscrete", &(self.path(), HashMap::<&str, Value>::new(), axis, steps));
    }

    pub fn close(&self) {
        if let Ok(p) = Proxy::new(&self.conn, DEST, self.session.as_str(), "org.freedesktop.portal.Session") {
            let _ = p.call_noreply("Close", &());
        }
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        self.close();
    }
}

type Call = Box<dyn FnOnce(&Proxy) -> zbus::Result<()>>;

/// Makes a portal call and waits for its Response. `make` gets the handle token and returns the call.
fn request(conn: &Connection, proxy: &Proxy, what: &str, make: impl FnOnce(&str) -> Call) -> Result<HashMap<String, OwnedValue>, String> {
    let t = token();
    let sender = conn.unique_name().ok_or("no D-Bus name")?.as_str().trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{t}");
    let req = Proxy::new(conn, DEST, path.as_str(), "org.freedesktop.portal.Request").map_err(|e| e.to_string())?;
    let mut responses = req.receive_signal("Response").map_err(|e| e.to_string())?;
    make(&t)(proxy).map_err(|e| format!("portal {what}: {e}"))?;
    let msg = responses.next().ok_or_else(|| format!("portal {what}: no answer"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|e| format!("portal {what}: {e}"))?;
    match code {
        0 => Ok(results),
        1 => {
            if what == "Start" || what == "SelectDevices" {
                // A refused or stale permission: ask afresh next time.
                let _ = std::fs::remove_file(token_path());
            }
            Err("screen sharing wasn't allowed (the dialog was cancelled)".into())
        }
        _ => {
            let _ = std::fs::remove_file(token_path());
            Err(format!("the desktop refused {what} (code {code})"))
        }
    }
}
