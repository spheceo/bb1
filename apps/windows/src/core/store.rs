//! What this PC remembers: its bb1 identity, the Macs it's paired with, and settings.
//! %APPDATA%\bb1 on Windows (`BB1_SUPPORT_DIR` overrides it, for tests).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn support_dir() -> PathBuf {
    let dir = if let Ok(d) = std::env::var("BB1_SUPPORT_DIR") {
        PathBuf::from(d)
    } else if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("bb1")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".bb1-windows")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn log_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local).join("bb1").join("Logs")
    } else {
        support_dir().join("logs")
    }
}

/// This PC's permanent bb1 ID (a UUID, like the Macs use).
pub fn device_id() -> String {
    let path = support_dir().join("device-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let t = text.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let mut b = crate::core::random_bytes::<16>();
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    let id = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
    let _ = std::fs::write(&path, &id);
    id
}

pub fn device_name() -> String {
    std::env::var("BB1_DEVICE_NAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Windows PC".into())
}

mod b64 {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// A Mac this PC has paired with. From then on it signs in with `secret`.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KnownHost {
    pub host_id: String,
    pub name: String,
    #[serde(with = "b64")]
    pub secret: Vec<u8>,
    #[serde(default)]
    pub tailscale_ip: Option<String>,
    /// The address used when pairing.
    #[serde(default)]
    pub address: Option<String>,
    /// Where it was last seen on the home network ("192.168.3.10:47800"), so it can be tried even when
    /// Bonjour is quiet (a firewall blocking it, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_address: Option<String>,
}

impl KnownHost {
    fn path() -> PathBuf {
        support_dir().join("known-hosts.json")
    }

    pub fn load() -> Vec<KnownHost> {
        std::fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }

    pub fn save(hosts: &[KnownHost]) {
        if let Ok(d) = serde_json::to_vec_pretty(hosts) {
            let tmp = Self::path().with_extension("tmp");
            if std::fs::write(&tmp, d).is_ok() {
                let _ = std::fs::rename(&tmp, Self::path());
            }
        }
    }

    pub fn remember(host: KnownHost) {
        let mut hosts = Self::load();
        hosts.retain(|h| h.host_id != host.host_id);
        hosts.push(host);
        Self::save(&hosts);
    }

    pub fn forget(host_id: &str) {
        let mut hosts = Self::load();
        hosts.retain(|h| h.host_id != host_id);
        Self::save(&hosts);
    }

    /// Remember where a paired Mac is on the home network (only writes when it changed).
    pub fn note_lan_address(host_id: &str, address: &str) {
        let mut hosts = Self::load();
        let Some(h) = hosts.iter_mut().find(|h| h.host_id == host_id) else { return };
        if h.lan_address.as_deref() == Some(address) {
            return;
        }
        h.lan_address = Some(address.to_string());
        Self::save(&hosts);
    }

    /// Every address this Mac can be reached at, home network first.
    pub fn routes(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        let home = self.lan_address.clone().or_else(|| self.address.clone().filter(|a| !crate::core::tailscale::is_tailscale_ip(&host_of(a))));
        for a in [home, self.tailscale_ip.clone()].into_iter().flatten() {
            if !v.iter().any(|x| host_of(x) == host_of(&a)) {
                v.push(a);
            }
        }
        v
    }

    /// The pairing for a Mac reached at this address or with this name, if any.
    pub fn find(address: &str, name: &str) -> Option<KnownHost> {
        let host_part = address.rsplit_once(':').map(|(h, _)| h).unwrap_or(address);
        Self::load().into_iter().find(|h| {
            h.tailscale_ip.as_deref() == Some(host_part)
                || h.address.as_deref() == Some(address)
                || h.address.as_deref() == Some(host_part)
                || h.lan_address.as_deref() == Some(address)
                || (!name.is_empty() && h.name.eq_ignore_ascii_case(name))
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub modifier_layout: String,
    pub sound: bool,
    pub clipboard: bool,
    pub last_address: Option<String>,
    /// Lines per wheel notch.
    pub wheel_lines: f64,
    /// Update channel: nightlies (true) or releases only.
    pub nightly: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { modifier_layout: "standard".into(), sound: true, clipboard: true, last_address: None, wheel_lines: 3.0, nightly: true }
    }
}

impl Settings {
    fn path() -> PathBuf {
        support_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(d) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), d);
        }
    }
}

/// "192.168.3.10:47800" → "192.168.3.10", "[fd7a::1]:47800" → "fd7a::1", "kenobi" → "kenobi"
pub fn host_of(address: &str) -> String {
    let a = address.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    if a.matches(':').count() == 1 {
        return a.split(':').next().unwrap_or(a).to_string();
    }
    a.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_put_the_home_network_first() {
        let k = KnownHost {
            host_id: "x".into(),
            name: "Mac".into(),
            secret: vec![1],
            tailscale_ip: Some("100.80.66.124".into()),
            address: Some("100.80.66.124:47800".into()),
            lan_address: Some("192.168.3.10:47800".into()),
        };
        assert_eq!(k.routes(), vec!["192.168.3.10:47800".to_string(), "100.80.66.124".to_string()]);
        let paired_over_tailscale = KnownHost { lan_address: None, ..k.clone() };
        assert_eq!(paired_over_tailscale.routes(), vec!["100.80.66.124".to_string()]);
        assert_eq!(host_of("[fd7a::1]:47800"), "fd7a::1");
        assert_eq!(host_of("192.168.3.10:47800"), "192.168.3.10");
        assert_eq!(host_of("kenobi"), "kenobi");
    }
}
