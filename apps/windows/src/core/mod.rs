pub mod audio;
pub mod client;
pub mod crypto;
pub mod directory;
pub mod discovery;
pub mod keymap;
pub mod log;
pub mod proto;
pub mod store;
pub mod tailscale;
pub mod video;

use std::sync::OnceLock;
use std::time::Instant;

/// Same numbers as the Mac app (BB1Core/Basics.swift).
pub const PROTOCOL_VERSION: i64 = 4;
pub const CONTROL_PORT: u16 = 47800;
pub const MAX_DATAGRAM: usize = 1200;
pub const BONJOUR_TYPE: &str = "_bb1._tcp.local.";

/// Microseconds on this machine's monotonic clock. Only compared with itself (and with the host's
/// clock through the ping offset), so the zero point doesn't matter.
pub fn now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64 + 1_000_000
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("no system random source");
    b
}
