//! Updating bb1 without an admin prompt each time, so it can update itself when asked from
//! another computer (nobody there to click Yes) as well as from its own pill.
//!
//! The installer (which runs as admin once) registers the task "bb1 Update": it runs
//! `bb1.exe --apply-update` from Program Files as SYSTEM, and any signed-in user may start it.
//! Asking for an update writes the wanted build to %ProgramData%\bb1\update-request.json and starts
//! the task. The task only installs a real bb1 release: it looks that build up on GitHub, downloads
//! its Windows installer, checks it against the release's SHA256SUMS and installs it quietly. So
//! starting the task can't make it run anything else. A copy of bb1 in %TEMP% waits for it and opens
//! the new bb1 on the desktop (the installer runs as SYSTEM, so it mustn't open bb1 itself).

use crate::core::updater::{self, BUILD, build_name};
use crate::logi;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

pub const TASK: &str = "bb1 Update";

#[derive(Serialize, Deserialize, Debug)]
struct Request {
    build: i64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Outcome {
    build: i64,
    ok: bool,
    error: Option<String>,
}

fn shared_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join("bb1")
}

fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}

/// Was this bb1 installed with the update task (installers from nightly-29 on)?
pub fn task_installed() -> bool {
    hidden(Command::new("schtasks").args(["/query", "/tn", TASK])).output().is_ok_and(|o| o.status.success())
}

/// Ask the task to install `build`, and arrange for bb1 to open again afterwards. The caller quits
/// bb1 once this returns Ok (the installer replaces bb1.exe either way).
pub fn request(build: i64) -> Result<(), String> {
    let dir = shared_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(dir.join("update-result.json"));
    std::fs::write(dir.join("update-request.json"), serde_json::to_vec(&Request { build }).unwrap_or_default())
        .map_err(|e| format!("couldn't ask for the update ({e})"))?;
    // The helper that opens bb1 again: a copy, because the installer replaces bb1.exe.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let helper = std::env::temp_dir().join("bb1-relaunch.exe");
    std::fs::copy(&exe, &helper).map_err(|e| format!("couldn't prepare reopening bb1 ({e})"))?;
    let started = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    hidden(Command::new(&helper).arg("--relaunch-after-update").arg(build.to_string()).arg(&install_dir).arg(started.to_string()))
        .spawn()
        .map_err(|e| format!("couldn't prepare reopening bb1 ({e})"))?;
    let out = hidden(Command::new("schtasks").args(["/run", "/tn", TASK])).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("the update task didn't start ({})", String::from_utf8_lossy(&out.stdout).trim()));
    }
    logi!("update: asked the update task for {} (build {build})", build_name(build));
    Ok(())
}

/// The updater's quiet way in, if this install has the task: request the build, then quit (the
/// helper opens the new bb1). None: the installer with an admin prompt it is.
pub fn quiet_install() -> Option<updater::QuietInstall> {
    if !task_installed() {
        return None;
    }
    Some(Box::new(|build| {
        request(build)?;
        // A moment for whoever is waiting to hear back (another computer asking), then quit.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            logi!("update: quitting so the update can install");
            std::process::exit(0);
        });
        Ok(())
    }))
}

// MARK: The task (SYSTEM, no desktop)

/// `bb1.exe --apply-update` (from the task). Logs to %ProgramData%\bb1\update.log.
pub fn apply_update(args: &[String]) {
    let dir = shared_dir();
    let _ = std::fs::create_dir_all(&dir);
    crate::core::log::setup(dir.clone(), "update");
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let request: Option<Request> = std::fs::read(dir.join("update-request.json")).ok().and_then(|d| serde_json::from_slice(&d).ok());
    let build = arg("--build").and_then(|b| b.parse().ok()).or(request.map(|r| r.build)).unwrap_or(0);
    // `--installer <file>` (only when an admin runs this by hand: the task never passes it) tests an
    // installer that isn't on GitHub yet.
    let local = arg("--installer").map(PathBuf::from);
    // `--check-only` (by hand, tests): download and check a release's installer, don't install it.
    let check_only = args.iter().any(|a| a == "--check-only");
    logi!("update task: asked for build {build}; this is build {BUILD}");
    let result = install(build, local.as_deref(), &dir, check_only);
    if let Err(e) = &result {
        logi!("update task: failed: {e}");
    }
    let outcome = Outcome { build, ok: result.is_ok(), error: result.err() };
    let _ = std::fs::write(dir.join("update-result.json"), serde_json::to_vec(&outcome).unwrap_or_default());
}

fn install(build: i64, local: Option<&Path>, dir: &Path, check_only: bool) -> Result<(), String> {
    let file = match local {
        Some(f) => f.to_path_buf(),
        None => {
            if build <= BUILD && !check_only {
                logi!("update task: already on build {BUILD}");
                return Ok(());
            }
            let json = updater::fetch(updater::FEED).ok_or("couldn't reach GitHub")?;
            let all = updater::parse(&json)?;
            let release = all.into_iter().find(|r| r.build == build).ok_or(format!("there's no release with build {build}"))?;
            let sums_url = release.sums_url.clone().ok_or("the release has no checksums")?;
            let file = dir.join(&release.installer_name);
            updater::download(&release.installer_url, &file, release.size, |_| {})?;
            let sums = updater::fetch(&sums_url).ok_or("couldn't download the checksums")?;
            let want = String::from_utf8_lossy(&sums)
                .lines()
                .find_map(|l| {
                    let (hash, name) = l.split_once(char::is_whitespace)?;
                    (name.trim().trim_start_matches('*') == release.installer_name).then(|| hash.to_lowercase())
                })
                .ok_or("the installer isn't in the checksums")?;
            let have: String = Sha256::digest(std::fs::read(&file).map_err(|e| e.to_string())?).iter().map(|b| format!("{b:02x}")).collect();
            if have != want {
                let _ = std::fs::remove_file(&file);
                return Err("the download doesn't match the release's checksum".into());
            }
            logi!("update task: {} downloaded and checked", release.installer_name);
            if check_only {
                let _ = std::fs::remove_file(&file);
                return Ok(());
            }
            file
        }
    };
    // /NORUN: the installer mustn't open bb1 itself (it would run as SYSTEM, out of sight).
    let status = hidden(Command::new(&file).args(["/S", "/NORUN"])).status().map_err(|e| format!("couldn't run the installer ({e})"))?;
    if local.is_none() {
        let _ = std::fs::remove_file(&file);
    }
    if !status.success() {
        return Err(format!("the installer stopped ({status})"));
    }
    logi!("update task: installed build {build}");
    Ok(())
}

// MARK: Opening bb1 again (the signed-in user's session)

/// `bb1-relaunch.exe --relaunch-after-update <build> <install dir> <started>`: wait for the task's
/// result, then open bb1 from the install folder (the new one, or the old one if it failed).
pub fn relaunch(args: &[String]) {
    let pos = args.iter().position(|a| a == "--relaunch-after-update").unwrap_or(0);
    let install_dir = args.get(pos + 2).map(PathBuf::from).unwrap_or_default();
    let started: u64 = args.get(pos + 3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let result = shared_dir().join("update-result.json");
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline {
        let fresh = std::fs::metadata(&result)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + 2 >= started);
        if fresh {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // A moment for the installer's last steps, then bb1 (as this user, on this desktop).
    std::thread::sleep(Duration::from_millis(800));
    let _ = Command::new(install_dir.join("bb1.exe")).spawn();
}
