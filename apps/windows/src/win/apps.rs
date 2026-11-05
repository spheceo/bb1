//! The apps the owner can leave out of the sound this PC shares (docs/08-gaming-host.md "Sound:
//! leave chosen apps out"): Discord, Google Chrome and Helium. Each is found from its install
//! folder or App Paths registration (or, failing that, a running copy) and comes with its icon.
//! `known_apps()` is cached for 30 s, icons for as long as the app's exe stays where it is.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, PrivateExtractIconsW};
use windows::core::{PCWSTR, PWSTR};

/// An app icon: premultiplied BGRA, top-down rows, `width × height × 4` bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct MutableApp {
    /// "discord" | "chrome" | "helium"
    pub id: &'static str,
    pub name: &'static str,
    /// Lowercase executable names whose sound is this app's.
    pub exe_names: &'static [&'static str],
    /// The installed exe; None = not installed.
    pub path: Option<PathBuf>,
    pub icon: Option<Icon>,
}

impl MutableApp {
    pub fn installed(&self) -> bool {
        self.path.is_some()
    }

    /// Whether a process with this exe (name or full path, any case) is this app.
    pub fn matches_exe(&self, exe: &str) -> bool {
        let name = exe_file_name(exe);
        self.exe_names.iter().any(|n| *n == name)
    }
}

/// (id, name, exe names)
const KNOWN: [(&str, &str, &[&str]); 3] =
    [("discord", "Discord", &["discord.exe"]), ("chrome", "Google Chrome", &["chrome.exe"]), ("helium", "Helium", &["helium.exe"])];

/// The executables to leave out for these app ids (unknown ids are ignored).
pub fn exe_names_for(ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> =
        KNOWN.iter().filter(|(id, _, _)| ids.iter().any(|i| i == id)).flat_map(|(_, _, exes)| exes.iter().map(|e| e.to_string())).collect();
    out.sort();
    out.dedup();
    out
}

/// Which known app this exe belongs to.
pub fn app_id_for_exe(exe: &str) -> Option<&'static str> {
    let name = exe_file_name(exe);
    KNOWN.iter().find(|(_, _, exes)| exes.contains(&name.as_str())).map(|(id, _, _)| *id)
}

const CACHE_FOR: Duration = Duration::from_secs(30);

struct Cache {
    at: Instant,
    apps: Vec<MutableApp>,
    icons: HashMap<PathBuf, Option<Icon>>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Discord, Google Chrome and Helium, in that order, installed or not (see `path`).
pub fn known_apps() -> Vec<MutableApp> {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(|| Cache { at: Instant::now(), apps: Vec::new(), icons: HashMap::new() });
    if !cache.apps.is_empty() && cache.at.elapsed() < CACHE_FOR {
        return cache.apps.clone();
    }
    let running = running_exe_paths();
    let mut apps = Vec::new();
    for (id, name, exe_names) in KNOWN {
        let path = match id {
            "discord" => find_discord(),
            "chrome" => find_chrome(),
            _ => find_helium(),
        }
        .or_else(|| exe_names.iter().find_map(|e| running.get(*e).cloned()));
        let icon = path.as_ref().and_then(|p| cache.icons.entry(p.clone()).or_insert_with(|| icon_of(p)).clone());
        apps.push(MutableApp { id, name, exe_names, path, icon });
    }
    cache.at = Instant::now();
    cache.apps = apps.clone();
    apps
}

// ---------------------------------------------------------------------------------------------
// Finding them

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn program_files() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(|v| env_dir(v)).collect();
    dirs.dedup();
    dirs
}

fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// %LOCALAPPDATA%\Discord\app-<newest>\Discord.exe, else its Update.exe.
fn find_discord() -> Option<PathBuf> {
    let dir = env_dir("LOCALAPPDATA")?.join("Discord");
    let names: Vec<String> = std::fs::read_dir(&dir).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    let mut versions: Vec<&String> = names.iter().filter(|n| parse_app_version(n).is_some()).collect();
    versions.sort_by_key(|n| parse_app_version(n));
    versions.iter().rev().map(|n| dir.join(n).join("Discord.exe")).find(|p| p.is_file()).or_else(|| first_file([dir.join("Update.exe")]))
}

/// "app-1.0.9178" → [1, 0, 9178].
pub(crate) fn parse_app_version(dir_name: &str) -> Option<Vec<u32>> {
    let v = dir_name.strip_prefix("app-")?;
    v.split('.').map(|p| p.parse().ok()).collect::<Option<Vec<u32>>>().filter(|v| !v.is_empty())
}

fn find_chrome() -> Option<PathBuf> {
    let tail = Path::new("Google").join("Chrome").join("Application").join("chrome.exe");
    app_path("chrome.exe")
        .or_else(|| first_file(program_files().into_iter().chain(env_dir("LOCALAPPDATA")).map(|d| d.join(&tail))))
}

fn find_helium() -> Option<PathBuf> {
    if let Some(p) = app_path("helium.exe") {
        return Some(p);
    }
    let app = Path::new("Helium").join("Application").join("helium.exe");
    let roots: Vec<PathBuf> = env_dir("LOCALAPPDATA").into_iter().chain(program_files()).collect();
    let direct = roots.iter().flat_map(|r| [r.join("imput").join(&app), r.join(&app), r.join("Helium").join("helium.exe")]);
    if let Some(p) = first_file(direct) {
        return Some(p);
    }
    // Any vendor folder: <root>\*\Helium\Application\helium.exe.
    roots.iter().find_map(|r| {
        std::fs::read_dir(r).ok()?.flatten().map(|e| e.path().join(&app)).find(|p| p.is_file())
    })
}

/// HKLM / HKCU \SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\<exe>, if that file exists.
fn app_path(exe: &str) -> Option<PathBuf> {
    let key = wide(&format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"));
    [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER].into_iter().find_map(|root| {
        let value = reg_string(root, &key)?;
        let p = PathBuf::from(unquote_path(&value));
        p.is_file().then_some(p)
    })
}

/// A key's default value (REG_SZ, or REG_EXPAND_SZ expanded).
fn reg_string(root: HKEY, key: &[u16]) -> Option<String> {
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(root, PCWSTR(key.as_ptr()), PCWSTR::null(), RRF_RT_REG_SZ, None, None, Some(&mut size)) != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let mut size = (buf.len() * 2) as u32;
        if RegGetValueW(root, PCWSTR(key.as_ptr()), PCWSTR::null(), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut size)) != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// `"C:\x\chrome.exe" --flag` or `C:\x\chrome.exe` → `C:\x\chrome.exe`.
pub(crate) fn unquote_path(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    s.to_string()
}

/// "C:\X\Chrome.EXE" → "chrome.exe".
pub(crate) fn exe_file_name(path_or_name: &str) -> String {
    path_or_name.trim().rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
}

/// Lowercase exe name → full path, for the known apps' running processes.
fn running_exe_paths() -> HashMap<String, PathBuf> {
    let wanted: Vec<&str> = KNOWN.iter().flat_map(|(_, _, exes)| exes.iter().copied()).collect();
    let mut out = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
            if wanted.contains(&name.as_str())
                && !out.contains_key(&name)
                && let Some(p) = process_path(e.th32ProcessID)
            {
                out.insert(name, p);
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())])))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------------------------
// Icons

const ICON_SIZE: i32 = 32;

/// The exe's first icon at 32×32 (Windows picks the best size and scales).
fn icon_of(path: &Path) -> Option<Icon> {
    let w: Vec<u16> = path.as_os_str().encode_wide().collect();
    if w.len() >= 260 || w.contains(&0) {
        return None;
    }
    let mut name = [0u16; 260];
    name[..w.len()].copy_from_slice(&w);
    unsafe {
        let mut icons = [HICON::default(); 1];
        let n = PrivateExtractIconsW(&name, 0, ICON_SIZE, ICON_SIZE, Some(&mut icons), None, 0);
        let hicon = if n >= 1 && n != u32::MAX && !icons[0].is_invalid() {
            icons[0]
        } else {
            let mut large = HICON::default();
            if ExtractIconExW(PCWSTR(name.as_ptr()), 0, Some(&mut large as *mut HICON), None, 1) == 0 || large.is_invalid() {
                return None;
            }
            large
        };
        let icon = icon_to_bgra(hicon);
        let _ = DestroyIcon(hicon);
        icon
    }
}

/// HICON → premultiplied BGRA: its 32-bit colour bitmap with alpha, or (no alpha in it) the
/// colour bitmap with the AND mask as alpha. Monochrome icons: None.
unsafe fn icon_to_bgra(hicon: HICON) -> Option<Icon> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(hicon, &mut info).ok()?;
        let (color, mask) = (info.hbmColor, info.hbmMask);
        let icon = (|| {
            if color.is_invalid() {
                return None;
            }
            let mut bm = BITMAP::default();
            if GetObjectW(HGDIOBJ(color.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            if !(1..=256).contains(&w) || !(1..=256).contains(&h) {
                return None;
            }
            let dc = GetDC(None);
            if dc.is_invalid() {
                return None;
            }
            let px = read_bits(dc, color, w, h);
            let mask_px = if px.as_deref().is_some_and(|p| !has_alpha(p)) && !mask.is_invalid() { read_bits(dc, mask, w, h) } else { None };
            ReleaseDC(None, dc);
            let mut px = px?;
            if !has_alpha(&px) {
                alpha_from_mask(&mut px, &mask_px?);
            }
            premultiply(&mut px);
            Some(Icon { bgra: px, width: w as u32, height: h as u32 })
        })();
        if !color.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(color.0));
        }
        if !mask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(mask.0));
        }
        icon
    }
}

/// A bitmap's pixels as 32-bit BGRA, top-down.
unsafe fn read_bits(dc: HDC, bitmap: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    let lines = unsafe { GetDIBits(dc, bitmap, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS) };
    (lines == h).then_some(buf)
}

pub(crate) fn has_alpha(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).any(|p| p[3] != 0)
}

/// AND mask (as 32-bit pixels: black = opaque, white = transparent) → alpha.
pub(crate) fn alpha_from_mask(bgra: &mut [u8], mask: &[u8]) {
    for (p, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        p[3] = if m[0] == 0 && m[1] == 0 && m[2] == 0 { 255 } else { 0 };
    }
}

pub(crate) fn premultiply(bgra: &mut [u8]) {
    for p in bgra.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_names() {
        assert_eq!(exe_file_name("C:\\Program Files\\Google\\Chrome\\Application\\Chrome.EXE"), "chrome.exe");
        assert_eq!(exe_file_name("helium.exe"), "helium.exe");
        assert_eq!(app_id_for_exe("C:/x/Discord.exe"), Some("discord"));
        assert_eq!(app_id_for_exe("msedge.exe"), None);
        assert_eq!(exe_names_for(&["helium".into(), "discord".into(), "nope".into()]), vec!["discord.exe", "helium.exe"]);
        let app = MutableApp { id: "chrome", name: "Google Chrome", exe_names: &["chrome.exe"], path: None, icon: None };
        assert!(app.matches_exe("D:\\CHROME.exe") && !app.matches_exe("chromedriver.exe") && !app.installed());
    }

    #[test]
    fn discord_versions() {
        assert_eq!(parse_app_version("app-1.0.9178"), Some(vec![1, 0, 9178]));
        assert_eq!(parse_app_version("app-"), None);
        assert_eq!(parse_app_version("packages"), None);
        assert!(parse_app_version("app-1.0.10000") > parse_app_version("app-1.0.9999"));
    }

    #[test]
    fn app_paths() {
        assert_eq!(unquote_path("\"C:\\a b\\chrome.exe\" --x"), "C:\\a b\\chrome.exe");
        assert_eq!(unquote_path("  C:\\a\\helium.exe "), "C:\\a\\helium.exe");
    }

    #[test]
    fn pixels() {
        let mut px = vec![200, 100, 50, 0, 10, 20, 30, 0];
        assert!(!has_alpha(&px));
        alpha_from_mask(&mut px, &[0, 0, 0, 0, 255, 255, 255, 0]);
        assert_eq!(px[3], 255);
        assert_eq!(px[7], 0);
        premultiply(&mut px);
        assert_eq!(&px, &[200, 100, 50, 255, 0, 0, 0, 0]);
        let mut half = vec![255, 128, 0, 128];
        premultiply(&mut half);
        assert_eq!(half, vec![128, 64, 0, 128]);
    }
}
