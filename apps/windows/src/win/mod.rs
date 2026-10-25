//! The Windows app: a window listing Macs to connect to, the stream window (Direct3D 11), the
//! hardware H.264 decoder (Media Foundation), sound (WASAPI), keyboard hook and clipboard.

pub mod app;
mod audio_out;
mod clipboard;
mod connect;
mod cursor;
mod decoder;
mod gfx;
mod render;
mod stream;
mod util;
