//! bb1 for Windows and Linux. The protocol, crypto and connection logic in `core` builds on any
//! platform (so it's tested on a Mac against a real host); the window, decoder and sound in `win`
//! are Windows-only, and `linux` is the Linux app (client and host).

pub mod core;

#[cfg(windows)]
pub mod win;

/// The Windows host. Only its NVENC output parsing (`host::nvenc`) builds elsewhere: the Linux host
/// uses it too.
#[cfg(any(windows, target_os = "linux"))]
pub mod host;

#[cfg(target_os = "linux")]
pub mod linux;
