//! The Linux app: the same bb1 as on Windows (src/win, src/host), built on Linux's parts.
//!
//! - Main window (ui.rs, egui): your computers, connect, share this computer.
//! - Stream window (stream.rs, SDL3): the other computer's screen, keyboard grab on Wayland and X11.
//!   It runs as its own process (`bb1 --stream`), started by the main window.
//! - Decoder (decoder.rs): FFmpeg's libavcodec, on the GPU (NVIDIA through CUDA, else VA-API) or CPU.
//! - Sharing (sharing.rs): the desktop portal for permission (portal.rs), PipeWire for the screen
//!   (capture.rs) and sound (audio_in.rs), NVENC or x264 (encoder.rs), portal input (input.rs).

pub mod app;
mod audio_in;
mod capture;
mod decoder;
mod encoder;
mod input;
mod portal;
pub mod sharing;

/// FFmpeg and PipeWire set up once per process.
pub fn init_libraries() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        pipewire::init();
        unsafe { ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_ERROR) };
    });
}
pub mod stream;
pub mod ui;
