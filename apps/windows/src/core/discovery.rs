//! Macs sharing their screen on the home network (Bonjour `_bb1._tcp`).

use crate::core::BONJOUR_TYPE;
use crate::logi;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct LanHost {
    pub name: String,
    /// "192.168.3.10:47800"
    pub address: String,
}

/// Keeps browsing in the background; `hosts()` is what's visible right now.
pub struct Browser {
    _daemon: Option<ServiceDaemon>,
    found: Arc<Mutex<HashMap<String, LanHost>>>,
}

impl Browser {
    /// `changed` is called (on a background thread) whenever the list changes.
    pub fn start(changed: impl Fn() + Send + 'static) -> Browser {
        let found: Arc<Mutex<HashMap<String, LanHost>>> = Arc::default();
        let daemon = match ServiceDaemon::new() {
            Ok(d) => Some(d),
            Err(e) => {
                logi!("bonjour: can't look for Macs on the home network ({e})");
                None
            }
        };
        if let Some(d) = daemon.as_ref() {
            let rx = d.browse(BONJOUR_TYPE);
            if let Err(e) = &rx {
                logi!("bonjour: browsing failed ({e})");
            }
            if let Ok(rx) = rx {
                logi!("bonjour: looking for Macs on the home network");
                let found = found.clone();
                std::thread::spawn(move || {
                    while let Ok(event) = rx.recv() {
                        match event {
                            ServiceEvent::ServiceResolved(info) => {
                                let name = info.get_fullname().trim_end_matches(BONJOUR_TYPE).trim_end_matches('.').to_string();
                                // This PC's own sharing: not something to connect to.
                                let raw = unescape(info.get_fullname().trim_end_matches(BONJOUR_TYPE).trim_end_matches('.'));
                                if raw.eq_ignore_ascii_case(&crate::core::store::device_name()) {
                                    continue;
                                }
                                // The most home-network-like address first (192.168.x, 10.x, 172.16–31.x),
                                // then Tailscale; never a loopback or link-local one.
                                let mut addrs: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
                                addrs.retain(|a| !a.starts_with("127.") && a != "::1" && !a.starts_with("fe80"));
                                addrs.sort_by_key(|a| address_rank(a));
                                if let Some(ip) = addrs.first() {
                                    let address = if ip.contains(':') {
                                        format!("[{ip}]:{}", info.get_port())
                                    } else {
                                        format!("{ip}:{}", info.get_port())
                                    };
                                    let name = unescape(&name);
                                    let all: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
                                    let fresh = found.lock().unwrap().get(info.get_fullname()).is_none_or(|h| h.address != address);
                                    if fresh {
                                        logi!("bonjour: found {name} at {address} (addresses: {})", all.join(", "));
                                    }
                                    found.lock().unwrap().insert(info.get_fullname().to_string(), LanHost { name, address });
                                    changed();
                                }
                            }
                            ServiceEvent::ServiceRemoved(_, fullname) => {
                                if let Some(h) = found.lock().unwrap().remove(&fullname) {
                                    logi!("bonjour: {} left the home network", h.name);
                                    changed();
                                }
                            }
                            _ => {}
                        }
                    }
                });
            }
        }
        Browser { _daemon: daemon, found }
    }

    pub fn hosts(&self) -> Vec<LanHost> {
        let mut v: Vec<LanHost> = self.found.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

/// Lower is better for reaching a computer at home.
fn address_rank(a: &str) -> u8 {
    let Ok(ip) = a.parse::<std::net::Ipv4Addr>() else { return 8 }; // IPv6 last
    let o = ip.octets();
    match o {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        [172, 17, ..] => 6, // WSL / Docker's virtual network, not the home network
        [172, b, ..] if (16..=31).contains(&b) => 2,
        [100, b, ..] if (64..=127).contains(&b) => 4, // Tailscale
        _ => 5,
    }
}

/// Bonjour names escape some characters as \DDD (decimal) or \X.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 0 && b[i + 1..i + 4].iter().all(|c| c.is_ascii_digit()) {
            let n: u32 = std::str::from_utf8(&b[i + 1..i + 4]).unwrap().parse().unwrap();
            out.push(n as u8);
            i += 4;
        } else if b[i] == b'\\' && i + 1 < b.len() {
            out.push(b[i + 1]);
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
