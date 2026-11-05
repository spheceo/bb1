//! Lets your other devices read this one's bb1 logs over Tailscale (port 47802, plain HTTP):
//!
//!   curl http://pc:47802/                      what this device is, and its log files
//!   curl http://pc:47802/logs/client.log       the whole file (or ?tail=200000 for the last 200 KB)
//!
//! Only devices on the same Tailscale account get in (checked with `tailscale whois`), plus this
//! computer itself. Logs hold counts and events, never what was typed or shown. The Mac app serves
//! the same thing (apps/mac/Sources/bb1/App/LogService.swift); `scripts/bb1-logs.sh` fetches.
//! If another bb1 on this PC already serves (client and host both running), that one covers both:
//! they log to the same folder.

use crate::core::{now_us, tailscale};
use crate::logi;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub const LOG_PORT: u16 = 47802;
const DEFAULT_TAIL: u64 = 4_000_000;

/// Start serving `dir` in the background. `about`: a line or two describing this app.
pub fn start(dir: PathBuf, about: String) {
    let listener = match TcpListener::bind(("0.0.0.0", LOG_PORT)) {
        Ok(l) => l,
        Err(e) => {
            logi!("logs: not serving logs on port {LOG_PORT} ({e}); another bb1 here probably is");
            return;
        }
    };
    logi!("logs: devices on this Tailscale account can read these logs on port {LOG_PORT}");
    let allowed: &'static Mutex<HashMap<IpAddr, (bool, u64)>> = Box::leak(Box::new(Mutex::new(HashMap::new())));
    std::thread::Builder::new()
        .name("bb1 logs".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let (dir, about) = (dir.clone(), about.clone());
                std::thread::spawn(move || {
                    let _ = serve(conn, &dir, &about, allowed);
                });
            }
        })
        .unwrap();
}

fn allowed_peer(ip: IpAddr, cache: &Mutex<HashMap<IpAddr, (bool, u64)>>) -> Result<(), String> {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    if ip.is_loopback() {
        return Ok(());
    }
    let text = ip.to_string();
    if !tailscale::is_tailscale_ip(&text) {
        return Err("only over Tailscale".into());
    }
    if let Some((ok, at)) = cache.lock().unwrap().get(&ip) {
        if now_us() - at < 300_000_000 {
            return if *ok { Ok(()) } else { Err("not this device's Tailscale account".into()) };
        }
    }
    let ok = matches!((tailscale::whois_login(&text), tailscale::self_login()), (Some(a), Some(b)) if a == b);
    cache.lock().unwrap().insert(ip, (ok, now_us()));
    if ok { Ok(()) } else { Err("not this device's Tailscale account".into()) }
}

fn serve(mut conn: TcpStream, dir: &std::path::Path, about: &str, cache: &Mutex<HashMap<IpAddr, (bool, u64)>>) -> std::io::Result<()> {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = vec![0u8; 4096];
    let mut n = 0;
    while n < buf.len() {
        let got = conn.read(&mut buf[n..])?;
        if got == 0 {
            break;
        }
        n += got;
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || buf[..n].windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/").to_string();
    let peer = conn.peer_addr()?.ip();
    if let Err(why) = allowed_peer(peer, cache) {
        logi!("logs: refused {peer} ({why})");
        return respond(&mut conn, "403 Forbidden", format!("bb1: {why}\n").as_bytes());
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let tail = query.split('&').find_map(|kv| kv.strip_prefix("tail=")).and_then(|v| v.parse::<u64>().ok()).unwrap_or(DEFAULT_TAIL);
    if path == "/" || path.is_empty() {
        let mut text = format!("{about}\nLogs ({}):\n", dir.display());
        let mut files: Vec<_> = std::fs::read_dir(dir).map(|d| d.flatten().collect()).unwrap_or_default();
        files.sort_by_key(|e| e.file_name());
        for e in files {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".log") {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                text.push_str(&format!("  /logs/{name}  {} KB\n", size / 1000));
            }
        }
        return respond(&mut conn, "200 OK", text.as_bytes());
    }
    let Some(name) = path.strip_prefix("/logs/").filter(|n| n.ends_with(".log") && !n.contains(['/', '\\']) && !n.contains("..")) else {
        return respond(&mut conn, "404 Not Found", b"bb1: try / or /logs/<name>.log\n");
    };
    match std::fs::File::open(dir.join(name)) {
        Ok(mut f) => {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let skip = len.saturating_sub(tail.min(50_000_000));
            std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(skip))?;
            let mut body = Vec::new();
            f.read_to_end(&mut body)?;
            respond(&mut conn, "200 OK", &body)
        }
        Err(_) => respond(&mut conn, "404 Not Found", b"bb1: no such log\n"),
    }
}

fn respond(conn: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!("HTTP/1.0 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    conn.write_all(head.as_bytes())?;
    conn.write_all(body)?;
    conn.flush()
}
