//! bb1 host for Windows: shares this PC's screen, sound and input with bb1 clients (Windows or Mac),
//! built for playing games together. Design and reasons: docs/08-gaming-host.md.
//!
//! One capture (DXGI Desktop Duplication) feeds one NVENC encoder per viewer, so each viewer gets
//! its own resolution and bitrate; packets are sealed, split and paced per viewer. The protocol is
//! the Mac host's (apps/mac/Sources/BB1Core), so existing clients connect unchanged.

pub mod audio_in;
pub mod capture;
pub mod input;
pub mod nvenc;
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all)]
pub mod nvenc_sys;
pub mod server;
