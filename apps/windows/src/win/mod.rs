//! The Windows app: a window listing Macs to connect to, the stream window (Direct3D 11), the
//! hardware H.264 decoder (Media Foundation), sound (WASAPI), keyboard hook and clipboard.

pub mod app;
pub mod apps;
mod audio_out;
mod clipboard;
mod connect;
mod cursor;
mod decoder;
pub mod gamepad;
pub mod selfupdate;
pub(crate) mod gfx;
mod render;
mod stream;
pub(crate) mod tray;
mod panels;
pub(crate) mod util;
mod webui;
