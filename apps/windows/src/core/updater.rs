//! Updates, like the Mac app's (App/Updater.swift + Client/HostUpdates.swift): the newest build on
//! GitHub for this PC's channel, your Macs that are behind it, and updating them and then this PC.
//!
//! Builds are compared by build number (the commit count). A release's build is in its Mac zip's name
//! ("bb1-0.0.0-build48.zip"); its Windows installer is "bb1-windows-…-setup.exe" in the same release.
//! Macs update first (each downloads its own update when asked, over a quiet "update" connection),
//! then this PC: the installer is downloaded, started (Windows asks for admin), and bb1 quits; the
//! installer opens the new bb1 when it's done, which then says "Updated to …".

use crate::core::client::{SideChannel, Target};
use crate::core::proto::Msg;
use crate::core::store::support_dir;
use crate::logi;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// This build's number (set by build.rs from the commit count; 0 for a build outside git).
pub const BUILD: i64 = match option_env!("BB1_BUILD") {
    Some(s) => parse_i64(s),
    None => 0,
};

const fn parse_i64(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut n = 0i64;
    while i < b.len() {
        if b[i] >= b'0' && b[i] <= b'9' {
            n = n * 10 + (b[i] - b'0') as i64;
        }
        i += 1;
    }
    n
}

/// Nightly M is build M + 25 (see apps/mac/CHANGELOG.md).
pub fn build_name(build: i64) -> String {
    if build > 25 { format!("nightly-{}", build - 25) } else { format!("build {build}") }
}

const FEED: &str = "https://api.github.com/repos/spheceo/bb1/releases?per_page=30";
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub build: i64,
    pub nightly: bool,
    /// "nightly-24" or "v0.0.1"
    pub short_name: String,
    pub installer_url: String,
    pub installer_name: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// Asking Macs to update; (names, overall progress 0..1)
    UpdatingMacs(Vec<String>, f64),
    Downloading(f64),
    /// The installer is running (Windows asks for admin); bb1 quits.
    Installing,
    Failed(String),
}

#[derive(Default)]
struct State {
    latest: Option<Release>,
    phase: Option<Phase>,
    last_check: Option<Instant>,
    checking: bool,
    just_updated: Option<JustUpdated>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JustUpdated {
    pub from: i64,
    pub to: i64,
}

impl JustUpdated {
    fn path() -> PathBuf {
        support_dir().join("just-updated.json")
    }
}

/// What the pill should show.
#[derive(Clone, Debug, PartialEq)]
pub enum Pill {
    Hidden,
    Offer { label: String, also: Vec<String> },
    Busy { label: String, progress: f64 },
    Done(String),
    Failed(String),
}

#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<State>>,
    changed: Arc<dyn Fn() + Send + Sync>,
}

impl Updater {
    pub fn start(changed: impl Fn() + Send + Sync + 'static) -> Updater {
        let u = Updater { state: Arc::default(), changed: Arc::new(changed) };
        // Did we just come back as a new build?
        if let Some(j) = std::fs::read(JustUpdated::path()).ok().and_then(|d| serde_json::from_slice::<JustUpdated>(&d).ok()) {
            let _ = std::fs::remove_file(JustUpdated::path());
            if BUILD >= j.to {
                logi!("update: updated from {} to {}", build_name(j.from), build_name(BUILD));
                u.state.lock().unwrap().just_updated = Some(j);
            } else {
                logi!("update: the installer didn't finish (still {})", build_name(BUILD));
                u.state.lock().unwrap().phase = Some(Phase::Failed("The update didn't install. Try again.".into()));
            }
        }
        let me = u.clone();
        std::thread::Builder::new()
            .name("bb1 updates".into())
            .spawn(move || loop {
                me.check();
                std::thread::sleep(CHECK_EVERY);
            })
            .unwrap();
        u
    }

    /// Check GitHub now, unless it was checked in the last minute.
    pub fn check_soon(&self) {
        if self.state.lock().unwrap().last_check.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || me.check());
    }

    fn check(&self) {
        {
            let mut s = self.state.lock().unwrap();
            if s.checking {
                return;
            }
            s.checking = true;
            s.last_check = Some(Instant::now());
        }
        let nightly = crate::core::store::Settings::load().nightly;
        let latest = fetch(FEED).and_then(|json| parse(&json).ok()).map(|all| newest(&all, nightly));
        let mut s = self.state.lock().unwrap();
        s.checking = false;
        if let Some(latest) = latest {
            if latest != s.latest {
                if let Some(r) = &latest {
                    logi!("update: newest on GitHub is {} (build {}); this is build {BUILD}", r.short_name, r.build);
                }
                s.latest = latest;
                drop(s);
                (self.changed)();
            }
        }
    }

    pub fn latest(&self) -> Option<Release> {
        self.state.lock().unwrap().latest.clone()
    }

    /// The "Updated to …" pill has been seen.
    pub fn dismiss_done(&self) {
        self.state.lock().unwrap().just_updated = None;
        (self.changed)();
    }

    /// `behind`: Macs that would be updated too (from the Your Macs list).
    pub fn pill(&self, behind: &[String]) -> Pill {
        let s = self.state.lock().unwrap();
        match &s.phase {
            Some(Phase::UpdatingMacs(names, p)) => {
                return Pill::Busy { label: format!("Updating {} · {}%", list(names), (p * 100.0) as i64), progress: *p };
            }
            Some(Phase::Downloading(p)) => return Pill::Busy { label: format!("Updating · {}%", (p * 100.0) as i64), progress: *p },
            Some(Phase::Installing) => return Pill::Busy { label: "Installing…".into(), progress: 1.0 },
            _ => {}
        }
        let newer = s.latest.as_ref().filter(|r| r.build > BUILD);
        if let Some(Phase::Failed(why)) = &s.phase {
            if newer.is_some() || !behind.is_empty() {
                return Pill::Failed(why.clone());
            }
        }
        if let Some(r) = newer {
            return Pill::Offer { label: format!("Update to {}", r.short_name), also: behind.to_vec() };
        }
        if let Some(r) = s.latest.as_ref().filter(|_| !behind.is_empty()) {
            return Pill::Offer { label: format!("Update {} to {}", list(behind), r.short_name), also: Vec::new() };
        }
        if let Some(j) = &s.just_updated {
            return Pill::Done(format!("Updated to {}", build_name(j.to)));
        }
        Pill::Hidden
    }

    pub fn busy(&self) -> bool {
        matches!(self.state.lock().unwrap().phase, Some(Phase::UpdatingMacs(..) | Phase::Downloading(_) | Phase::Installing))
    }

    fn set(&self, phase: Phase) {
        self.state.lock().unwrap().phase = Some(phase);
        (self.changed)();
    }

    /// Update `macs` (each with the build it runs) to the newest build, then this PC if it's behind.
    /// `launch` starts the downloaded installer (asking for admin); it returns false if that was refused.
    pub fn update(&self, macs: Vec<(Target, i64)>, launch: impl Fn(&std::path::Path) -> Result<(), String> + Send + 'static) {
        let Some(release) = self.latest() else { return };
        if self.busy() {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            if !macs.is_empty() {
                let names: Vec<String> = macs.iter().map(|(t, _)| t.label.clone()).collect();
                me.set(Phase::UpdatingMacs(names.clone(), 0.0));
                let progress: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(vec![0.0; macs.len()]));
                let handles: Vec<_> = macs
                    .into_iter()
                    .enumerate()
                    .map(|(i, (target, _))| {
                        let (me, progress, names, build) = (me.clone(), progress.clone(), names.clone(), release.build);
                        std::thread::spawn(move || {
                            let report = |p: f64| {
                                let all = {
                                    let mut v = progress.lock().unwrap();
                                    v[i] = p;
                                    v.iter().sum::<f64>() / v.len() as f64
                                };
                                me.set(Phase::UpdatingMacs(names.clone(), all));
                            };
                            let r = update_mac(&target, build, &report);
                            if let Err(e) = &r {
                                logi!("update: {}: {e}", target.label);
                            }
                            r.map_err(|e| format!("{}: {e}", target.label))
                        })
                    })
                    .collect();
                let failures: Vec<String> = handles.into_iter().filter_map(|h| h.join().ok()).filter_map(|r| r.err()).collect();
                if let Some(first) = failures.first() {
                    me.set(Phase::Failed(format!("Couldn't update {first}")));
                    return;
                }
            }
            if release.build <= BUILD {
                me.state.lock().unwrap().phase = None;
                (me.changed)();
                return;
            }
            me.set(Phase::Downloading(0.0));
            let dir = std::env::temp_dir().join("bb1-update");
            let _ = std::fs::create_dir_all(&dir);
            let file = dir.join(&release.installer_name);
            if let Err(e) = download(&release.installer_url, &file, release.size, |p| me.set(Phase::Downloading(p))) {
                logi!("update: download failed: {e}");
                me.set(Phase::Failed(format!("Couldn't download the update ({e})")));
                return;
            }
            me.set(Phase::Installing);
            let marker = JustUpdated { from: BUILD, to: release.build };
            let _ = std::fs::write(JustUpdated::path(), serde_json::to_vec(&marker).unwrap_or_default());
            logi!("update: installing {} from {}", release.short_name, file.display());
            if let Err(e) = launch(&file) {
                let _ = std::fs::remove_file(JustUpdated::path());
                logi!("update: the installer didn't start: {e}");
                me.set(Phase::Failed(e));
            }
        });
    }
}

/// Ask one Mac to update itself, follow along, then wait until it's back on the new build.
fn update_mac(target: &Target, build: i64, report: &dyn Fn(f64)) -> Result<(), String> {
    logi!("update: asking {} to update to build {build}", target.label);
    let mut ch = SideChannel::open(target, Duration::from_secs(4))?;
    if ch.host_build >= build {
        report(1.0);
        return Ok(());
    }
    ch.send(&Msg::UpdateHost { build })?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if Instant::now() > deadline {
            return Err("took too long".into());
        }
        match ch.receive(Duration::from_millis(500)) {
            Ok(Some(Msg::UpdateStatus(s))) => match s.v.phase.as_str() {
                "downloading" => report(0.8 * s.v.progress.unwrap_or(0.0)),
                "installing" => report(0.85),
                "restarting" => report(0.9),
                "upToDate" => {
                    report(1.0);
                    return Ok(());
                }
                _ => return Err(s.v.detail.unwrap_or_else(|| "the update failed".into())),
            },
            Ok(_) => {}
            // The Mac quit to install: wait for it to come back with the new build.
            Err(_) => break,
        }
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        if let Ok(ch) = SideChannel::open(target, Duration::from_secs(3)) {
            if ch.host_build >= build {
                logi!("update: {} is back on build {}", target.label, ch.host_build);
                report(1.0);
                return Ok(());
            }
        }
    }
    Err("didn't come back after updating".into())
}

/// "PC", "PC and LEPC", "A, B and C"
pub fn list(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

fn curl() -> Command {
    // Windows 10 and 11 come with curl.exe; using it keeps a TLS stack out of bb1.
    let path = std::env::var("SystemRoot").map(|r| format!(r"{r}\System32\curl.exe")).ok().filter(|p| std::path::Path::new(p).exists());
    let mut cmd = Command::new(path.unwrap_or_else(|| "curl".into()));
    cmd.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn fetch(url: &str) -> Option<Vec<u8>> {
    let out = curl()
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json", "-A", "bb1-windows", url])
        .stdout(Stdio::piped())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn download(url: &str, to: &std::path::Path, size: u64, progress: impl Fn(f64)) -> Result<(), String> {
    let _ = std::fs::remove_file(to);
    let mut child = curl()
        .args(["-fsSL", "--max-time", "600", "-A", "bb1-windows", "-o"])
        .arg(to)
        .arg(url)
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let got = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
                return if status.success() && (size == 0 || got == size) {
                    progress(1.0);
                    Ok(())
                } else {
                    Err(if status.success() { "incomplete download".into() } else { "network error".into() })
                };
            }
            Ok(None) => {
                if size > 0 {
                    let got = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
                    progress((got as f64 / size as f64).min(0.99));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[derive(Deserialize)]
struct GhRelease {
    prerelease: bool,
    draft: bool,
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Releases that have a Windows installer, with their build numbers.
pub fn parse(json: &[u8]) -> Result<Vec<Release>, String> {
    let all: Vec<GhRelease> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(all
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let build = r.assets.iter().find_map(|a| zip_build(&a.name))?;
            let exe = r.assets.iter().find(|a| a.name.starts_with("bb1-windows-") && a.name.ends_with("-setup.exe"))?;
            Some(Release {
                build,
                nightly: r.prerelease,
                short_name: r.tag_name.clone(),
                installer_url: exe.browser_download_url.clone(),
                installer_name: exe.name.clone(),
                size: exe.size,
            })
        })
        .collect())
}

/// The newest release on this channel (None if there's none yet).
pub fn newest(all: &[Release], nightly: bool) -> Option<Release> {
    all.iter().filter(|r| r.nightly == nightly).max_by_key(|r| r.build).cloned()
}

/// "bb1-0.0.1-build12.zip" or "bb1-v0.0.1-build12.zip" → 12
fn zip_build(name: &str) -> Option<i64> {
    let middle = name.strip_prefix("bb1-")?.strip_suffix(".zip")?;
    let (_, build) = middle.rsplit_once("-build")?;
    build.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_windows_installers_and_their_builds() {
        let json = br#"[
          {"prerelease": true, "draft": false, "tag_name": "nightly-23", "assets": [
            {"name": "bb1-0.0.0-build48.zip", "browser_download_url": "https://x/zip", "size": 10},
            {"name": "bb1-windows-nightly-23-setup.exe", "browser_download_url": "https://x/exe", "size": 1105614},
            {"name": "SHA256SUMS", "browser_download_url": "https://x/sums", "size": 1}]},
          {"prerelease": true, "draft": false, "tag_name": "nightly-20", "assets": [
            {"name": "bb1-0.0.0-build45.zip", "browser_download_url": "https://x/zip", "size": 10}]},
          {"prerelease": false, "draft": false, "tag_name": "v0.0.0", "assets": [
            {"name": "bb1-v0.0.0-build9.zip", "browser_download_url": "https://x/zip", "size": 10}]},
          {"prerelease": true, "draft": true, "tag_name": "nightly-24", "assets": [
            {"name": "bb1-0.0.0-build49.zip", "browser_download_url": "https://x/zip", "size": 10},
            {"name": "bb1-windows-nightly-24-setup.exe", "browser_download_url": "https://x/exe", "size": 5}]}
        ]"#;
        let all = parse(json).unwrap();
        assert_eq!(all.len(), 1, "only published releases with a Windows installer");
        let n = newest(&all, true).unwrap();
        assert_eq!((n.build, n.short_name.as_str(), n.size), (48, "nightly-23", 1105614));
        assert_eq!(newest(&all, false), None);
        assert_eq!(build_name(48), "nightly-23");
        assert_eq!(list(&["A".into(), "B".into(), "C".into()]), "A, B and C");
    }
}
