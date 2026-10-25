//! Reads the local Tailscale app through its command-line tool (blocks for up to a few seconds).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub os: String,
    /// Some when Tailscale has a direct path.
    pub direct: Option<String>,
    pub relay: Option<String>,
}

fn cli() -> Option<String> {
    let candidates = [
        r"C:\Program Files\Tailscale\tailscale.exe",
        r"C:\Program Files (x86)\Tailscale\tailscale.exe",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ];
    candidates.iter().find(|p| std::path::Path::new(p).exists()).map(|p| p.to_string())
}

fn run(args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new(cli().unwrap_or_else(|| "tailscale".into()));
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if started.elapsed() < Duration::from_secs(4) => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

/// Peers on this tailnet (None if Tailscale isn't installed or isn't running).
pub fn peers() -> Option<Vec<Peer>> {
    let data = run(&["status", "--json"])?;
    let root: serde_json::Value = serde_json::from_slice(&data).ok()?;
    let peers = root.get("Peer")?.as_object()?;
    let mut out: Vec<Peer> = peers
        .values()
        .filter_map(|p| {
            let ip = p.get("TailscaleIPs")?.as_array()?.first()?.as_str()?.to_string();
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cur = s("CurAddr");
            let relay = s("Relay");
            Some(Peer {
                name: p.get("HostName").and_then(|v| v.as_str()).unwrap_or(&ip).to_string(),
                ip,
                online: p.get("Online").and_then(|v| v.as_bool()).unwrap_or(false),
                os: s("OS"),
                direct: (!cur.is_empty()).then_some(cur),
                relay: (!relay.is_empty()).then_some(relay),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Some(out)
}

/// 100.64.0.0/10 or fd7a:115c:a1e0::/48
pub fn is_tailscale_ip(address: &str) -> bool {
    let a = address.trim_start_matches('[').split(['%', ']']).next().unwrap_or(address);
    if a.to_lowercase().starts_with("fd7a:115c:a1e0:") {
        return true;
    }
    let parts: Vec<u32> = a.split('.').filter_map(|p| p.parse().ok()).collect();
    parts.len() == 4 && parts[0] == 100 && (64..=127).contains(&parts[1])
}

/// "direct", "relayed via jnb", or None if it isn't a Tailscale peer.
pub fn path_description(ip: &str) -> Option<String> {
    let peer = peers()?.into_iter().find(|p| p.ip == ip)?;
    Some(if peer.direct.is_some() {
        "direct".into()
    } else {
        format!("relayed{}", peer.relay.map(|r| format!(" via {r}")).unwrap_or_default())
    })
}
