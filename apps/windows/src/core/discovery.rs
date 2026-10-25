//! Macs sharing their screen on the home network (Bonjour `_bb1._tcp`).

use crate::core::BONJOUR_TYPE;
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
        let daemon = ServiceDaemon::new().ok();
        if let Some(d) = daemon.as_ref() {
            if let Ok(rx) = d.browse(BONJOUR_TYPE) {
                let found = found.clone();
                std::thread::spawn(move || {
                    while let Ok(event) = rx.recv() {
                        match event {
                            ServiceEvent::ServiceResolved(info) => {
                                let name = info.get_fullname().trim_end_matches(BONJOUR_TYPE).trim_end_matches('.').to_string();
                                // Prefer IPv4: it's what the Mac's listener is reachable on for sure.
                                let mut addrs: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
                                addrs.sort_by_key(|a| a.contains(':'));
                                if let Some(ip) = addrs.first() {
                                    let address = if ip.contains(':') {
                                        format!("[{ip}]:{}", info.get_port())
                                    } else {
                                        format!("{ip}:{}", info.get_port())
                                    };
                                    let name = unescape(&name);
                                    found.lock().unwrap().insert(info.get_fullname().to_string(), LanHost { name, address });
                                    changed();
                                }
                            }
                            ServiceEvent::ServiceRemoved(_, fullname) => {
                                if found.lock().unwrap().remove(&fullname).is_some() {
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
