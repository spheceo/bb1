//! Your Macs: who's out there, who's sharing, and what their screens look like. A port of the Mac
//! client's HostDirectory + HostPreviews.
//!
//! Every few seconds: Macs on this Tailscale account are checked (does bb1 answer on its port?),
//! joined with Macs on the home network (Bonjour) and the ones this PC is paired with. Each Mac that
//! lets this PC in without a code gets a quiet side connection ("update": it doesn't touch a stream)
//! that asks for a small screenshot and a ping every 4 s. The last picture of each Mac is kept on
//! disk, so an offline Mac still shows what it last looked like.

use crate::core::client::{Auth, SideChannel, Target};
use crate::core::discovery::Browser;
use crate::core::proto::*;
use crate::core::store::{self, KnownHost};
use crate::core::{now_us, tailscale};
use crate::logi;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The first host build that answers preview requests.
const MIN_PREVIEW_BUILD: i64 = 15;
const PREVIEW_EVERY: Duration = Duration::from_secs(4);
const PREVIEW_WIDTH: i64 = 960;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Where to connect ("100.80.66.124", "192.168.3.10:47800").
    pub address: Option<String>,
    pub online: bool,
    /// Paired, or on this Tailscale account: connects without a code.
    pub quiet: bool,
}

#[derive(Clone)]
pub struct Preview {
    pub jpeg: Arc<Vec<u8>>,
    pub at: SystemTime,
    /// Bumps with every new picture, so the UI knows to decode it again.
    pub version: u64,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    previews: HashMap<String, Preview>,
    latency_ms: HashMap<String, f64>,
    builds: HashMap<String, i64>,
    sessions: HashMap<String, Arc<AtomicBool>>,
    failed_at: HashMap<String, Instant>,
    /// Tailscale Macs whose bb1 answered, by IP.
    answering: HashSet<String>,
    peers: Vec<tailscale::Peer>,
    tailscale_running: bool,
    next_version: u64,
}

#[derive(Clone)]
pub struct Directory {
    state: Arc<Mutex<State>>,
    changed: Arc<dyn Fn() + Send + Sync>,
    browser: Arc<Mutex<Option<Browser>>>,
    /// Previews only while the list is on screen.
    active: Arc<AtomicBool>,
}

impl Directory {
    /// `changed` is called (from background threads) whenever something to show changed.
    pub fn start(changed: impl Fn() + Send + Sync + 'static) -> Directory {
        let d = Directory {
            state: Arc::default(),
            changed: Arc::new(changed),
            browser: Arc::default(),
            active: Arc::new(AtomicBool::new(true)),
        };
        let notify = d.changed.clone();
        let browser = Browser::start(move || notify());
        *d.browser.lock().unwrap() = Some(browser);
        let me = d.clone();
        std::thread::Builder::new()
            .name("bb1 directory".into())
            .spawn(move || loop {
                me.refresh_tailscale();
                me.rebuild();
                std::thread::sleep(Duration::from_secs(5));
            })
            .unwrap();
        d
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.state.lock().unwrap().entries.clone()
    }

    pub fn tailscale_running(&self) -> bool {
        self.state.lock().unwrap().tailscale_running
    }

    pub fn preview(&self, name: &str) -> Option<Preview> {
        let mut s = self.state.lock().unwrap();
        if let Some(p) = s.previews.get(name) {
            return Some(p.clone());
        }
        // The last picture seen, from disk.
        let path = cache_path(name);
        let jpeg = std::fs::read(&path).ok()?;
        let at = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(SystemTime::now());
        s.next_version += 1;
        let p = Preview { jpeg: Arc::new(jpeg), at, version: s.next_version };
        s.previews.insert(name.to_string(), p.clone());
        Some(p)
    }

    pub fn latency_ms(&self, name: &str) -> Option<f64> {
        self.state.lock().unwrap().latency_ms.get(name).copied()
    }

    pub fn build(&self, name: &str) -> Option<i64> {
        self.state.lock().unwrap().builds.get(name).copied()
    }

    /// Pause previews while streaming or hidden (they'd only cost bandwidth).
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Relaxed);
        if !on {
            let sessions: Vec<_> = self.state.lock().unwrap().sessions.drain().collect();
            for (_, stop) in sessions {
                stop.store(true, Relaxed);
            }
        } else {
            self.rebuild();
        }
    }

    /// Check again now (after pairing, forgetting, or showing the list).
    pub fn refresh_now(&self) {
        let me = self.clone();
        std::thread::spawn(move || {
            me.refresh_tailscale();
            me.rebuild();
        });
    }

    fn refresh_tailscale(&self) {
        let peers = tailscale::peers();
        let running = peers.is_some();
        let candidates: Vec<tailscale::Peer> =
            peers.clone().unwrap_or_default().into_iter().filter(|p| p.online && p.os == "macOS" && p.mine).collect();
        // Check them all at once: each is a TCP connect that may take a second to fail.
        let handles: Vec<_> = candidates
            .iter()
            .map(|p| {
                let ip = p.ip.clone();
                std::thread::spawn(move || (tailscale::bb1_answers(&ip, Duration::from_millis(1500)), ip))
            })
            .collect();
        let answering: HashSet<String> = handles.into_iter().filter_map(|h| h.join().ok()).filter(|(ok, _)| *ok).map(|(_, ip)| ip).collect();
        let mut s = self.state.lock().unwrap();
        s.answering = answering;
        s.peers = peers.unwrap_or_default();
        s.tailscale_running = running;
    }

    /// Joins the sources into the list, and keeps previews going for the Macs that can send them.
    fn rebuild(&self) {
        let lan = self.browser.lock().unwrap().as_ref().map(|b| b.hosts()).unwrap_or_default();
        let known = KnownHost::load();
        let mut changed = false;
        let mut want_previews: Vec<Target> = Vec::new();
        {
            let mut s = self.state.lock().unwrap();
            let mut by_name: Vec<Entry> = Vec::new();
            let add = |list: &mut Vec<Entry>, e: Entry| {
                if let Some(x) = list.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&e.name)) {
                    if !x.online && e.online {
                        *x = Entry { quiet: x.quiet || e.quiet, ..e };
                    } else {
                        x.quiet |= e.quiet;
                    }
                } else {
                    list.push(e);
                }
            };
            for p in s.peers.iter().filter(|p| s.answering.contains(&p.ip)) {
                add(&mut by_name, Entry { name: p.name.clone(), address: Some(p.ip.clone()), online: true, quiet: true });
            }
            for h in &lan {
                let paired = KnownHost::find(&h.address, &h.name).is_some();
                add(&mut by_name, Entry { name: h.name.clone(), address: Some(h.address.clone()), online: true, quiet: paired });
            }
            for k in &known {
                let address = k.tailscale_ip.clone().or_else(|| k.address.clone());
                add(&mut by_name, Entry { name: k.name.clone(), address, online: false, quiet: true });
            }
            by_name.sort_by(|a, b| (!a.online, a.name.to_lowercase()).cmp(&(!b.online, b.name.to_lowercase())));
            if by_name.iter().map(|e| (&e.name, e.online, &e.address)).ne(s.entries.iter().map(|e| (&e.name, e.online, &e.address))) {
                changed = true;
            }
            s.entries = by_name;

            if self.active.load(Relaxed) {
                for e in s.entries.iter().filter(|e| e.online) {
                    let Some(address) = &e.address else { continue };
                    let target = Target::new(address, &e.name);
                    if matches!(target.auth, Auth::Code) {
                        continue; // needs pairing first
                    }
                    if s.builds.get(&e.name).is_some_and(|&b| b < MIN_PREVIEW_BUILD) {
                        continue;
                    }
                    want_previews.push(target);
                }
                // Stop previews for Macs that went away.
                let wanted: HashSet<String> = want_previews.iter().map(|t| t.label.clone()).collect();
                s.sessions.retain(|name, stop| {
                    let keep = wanted.contains(name);
                    if !keep {
                        stop.store(true, Relaxed);
                    }
                    keep
                });
            }
        }
        for target in want_previews {
            self.ensure_preview(target);
        }
        if changed {
            (self.changed)();
        }
    }

    fn ensure_preview(&self, target: Target) {
        let name = target.label.clone();
        let stop = {
            let mut s = self.state.lock().unwrap();
            if s.sessions.contains_key(&name) || s.failed_at.get(&name).is_some_and(|t| t.elapsed() < Duration::from_secs(15)) {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            s.sessions.insert(name.clone(), stop.clone());
            stop
        };
        let me = self.clone();
        std::thread::Builder::new()
            .name("bb1 preview".into())
            .spawn(move || {
                let result = me.run_preview(&target, &stop);
                let mut s = me.state.lock().unwrap();
                if s.sessions.get(&name).is_some_and(|x| Arc::ptr_eq(x, &stop)) {
                    s.sessions.remove(&name);
                }
                if let Err(e) = result {
                    s.failed_at.insert(name.clone(), Instant::now());
                    if !stop.load(Relaxed) {
                        logi!("previews: {name}: {e}");
                    }
                }
            })
            .unwrap();
    }

    fn run_preview(&self, target: &Target, stop: &AtomicBool) -> Result<(), String> {
        let name = target.label.clone();
        let mut ch = SideChannel::open(target, Duration::from_secs(3))?;
        self.state.lock().unwrap().builds.insert(name.clone(), ch.host_build);
        (self.changed)();
        if ch.host_build < MIN_PREVIEW_BUILD {
            return Ok(()); // too old to send its screen
        }
        let mut seq = 0u32;
        let mut sent_at = Instant::now();
        let mut answers_pings = false;
        while !stop.load(Relaxed) {
            seq = seq.wrapping_add(1);
            ch.send(&Msg::Ping(v(PingMsg { seq, client_us: now_us(), received: 0, decoded: 0, displayed: 0 })))?;
            ch.send(&Msg::RequestPreview { max_width: PREVIEW_WIDTH })?;
            sent_at = if answers_pings { sent_at } else { Instant::now() };
            let deadline = Instant::now() + PREVIEW_EVERY;
            while Instant::now() < deadline && !stop.load(Relaxed) {
                let left = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
                match ch.receive(left.max(Duration::from_millis(1))) {
                    Ok(Some(Msg::Pong(p))) => {
                        answers_pings = true;
                        let ms = (now_us().saturating_sub(p.v.client_us)) as f64 / 1000.0;
                        self.state.lock().unwrap().latency_ms.insert(name.clone(), ms.max(1.0));
                        (self.changed)();
                    }
                    Ok(Some(Msg::Preview(p))) => {
                        let p = p.v;
                        let mut s = self.state.lock().unwrap();
                        if !answers_pings {
                            let ms = sent_at.elapsed().as_secs_f64() * 1000.0 - p.capture_ms;
                            s.latency_ms.insert(name.clone(), ms.max(1.0));
                        }
                        s.failed_at.remove(&name);
                        if let Some(jpeg) = p.jpeg {
                            let _ = std::fs::write(cache_path(&name), &jpeg);
                            s.next_version += 1;
                            let version = s.next_version;
                            s.previews.insert(name.clone(), Preview { jpeg: Arc::new(jpeg), at: SystemTime::now(), version });
                        }
                        drop(s);
                        (self.changed)();
                    }
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
}

fn cache_path(name: &str) -> PathBuf {
    let dir = store::support_dir().join("previews");
    let _ = std::fs::create_dir_all(&dir);
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    dir.join(format!("{safe}.jpg"))
}

/// "just now", "5 min ago", "3 h ago", "2 days ago"
pub fn ago(at: SystemTime) -> String {
    let s = SystemTime::now().duration_since(at).unwrap_or_default().as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => {
            let d = s / 86_400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
    }
}
