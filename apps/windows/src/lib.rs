//! bb1 client for Windows. The protocol, crypto and connection logic in `core` builds on any
//! platform (so it's tested on a Mac against a real host); the window, decoder and sound in `win`
//! are Windows-only.

pub mod core;

#[cfg(windows)]
pub mod win;
