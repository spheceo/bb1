//! Shares the clipboard with the Mac: text and images. Copies that password managers mark as
//! private are never sent. Only new copies are sent, not what was on the clipboard before.

use crate::core::proto::ClipboardMsg;
use crate::logi;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{BI_BITFIELDS, BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};
use windows::core::w;

const MAX_TEXT: usize = 2_000_000;
const MAX_IMAGE: usize = 6_000_000;

static LAST_SEQ: AtomicU32 = AtomicU32::new(0);

/// From now on, report new copies only.
pub fn start() {
    LAST_SEQ.store(unsafe { GetClipboardSequenceNumber() }, Relaxed);
}

fn format(name: windows::core::PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

/// Something new was copied on this PC: what to send (None if nothing new, private, or too big).
pub fn local_change() -> Option<ClipboardMsg> {
    let seq = unsafe { GetClipboardSequenceNumber() };
    if seq == LAST_SEQ.load(Relaxed) {
        return None;
    }
    LAST_SEQ.store(seq, Relaxed);
    unsafe {
        // Password managers set these on copies that shouldn't leave the PC.
        for private in [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("Clipboard Viewer Ignore")] {
            if IsClipboardFormatAvailable(format(private)).is_ok() {
                logi!("clipboard: skipped a private copy (password manager)");
                return None;
            }
        }
        if !open() {
            return None;
        }
        let msg = read();
        let _ = CloseClipboard();
        match &msg {
            Some(ClipboardMsg { text: Some(t), .. }) => logi!("clipboard: sending text ({} characters)", t.chars().count()),
            Some(ClipboardMsg { png: Some(p), .. }) => logi!("clipboard: sending image ({} KB)", p.len() / 1000),
            _ => {}
        }
        msg
    }
}

/// Opens the clipboard for one of our windows: with no window, EmptyClipboard leaves it without an
/// owner and every SetClipboardData fails. Another app may have it open for a moment: retry.
fn open() -> bool {
    let owner = crate::win::app::event_window();
    for attempt in 0..10 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return true;
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    false
}

/// Clipboard open.
unsafe fn read() -> Option<ClipboardMsg> {
    unsafe {
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len)).replace("\r\n", "\n");
                let _ = GlobalUnlock(g);
                if text.len() > MAX_TEXT {
                    logi!("clipboard: text too large to send ({} KB)", text.len() / 1000);
                    return None;
                }
                return Some(ClipboardMsg { text: Some(text), png: None });
            }
        }
        let png = if let Some(d) = global_bytes(format(w!("PNG"))) {
            Some(d)
        } else {
            global_bytes(CF_DIB.0 as u32).and_then(|dib| dib_to_png(&dib))
        };
        match png {
            Some(p) if p.len() <= MAX_IMAGE => Some(ClipboardMsg { text: None, png: Some(p) }),
            Some(p) => {
                logi!("clipboard: image too large to send ({} KB)", p.len() / 1000);
                None
            }
            None => None,
        }
    }
}

unsafe fn global_bytes(fmt: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(v)
    }
}

/// Puts the Mac's copy on this PC's clipboard.
pub fn apply(m: &ClipboardMsg) {
    unsafe {
        if !open() {
            logi!("clipboard: couldn't open the clipboard");
            return;
        }
        let _ = EmptyClipboard();
        if let Some(text) = &m.text {
            let wide: Vec<u16> = text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(Some(0)).collect();
            set_global(CF_UNICODETEXT.0 as u32, bytemuck_u16(&wide));
            logi!("clipboard: received text ({} characters) from the Mac", text.chars().count());
        } else if let Some(png) = &m.png {
            set_global(format(w!("PNG")), png);
            if let Some(dib) = png_to_dib(png) {
                set_global(CF_DIB.0 as u32, &dib);
            }
            logi!("clipboard: received image ({} KB) from the Mac", png.len() / 1000);
        }
        let _ = CloseClipboard();
        // Don't send our own write straight back.
        LAST_SEQ.store(GetClipboardSequenceNumber(), Relaxed);
    }
}

fn bytemuck_u16(v: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) }
}

unsafe fn set_global(fmt: u32, data: &[u8]) {
    unsafe {
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, data.len()) else { return };
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = GlobalUnlock(g);
        if SetClipboardData(fmt, Some(HANDLE(g.0))).is_err() {
            let _ = GlobalFree(Some(g));
        }
    }
}

/// A 24/32-bit CF_DIB → PNG.
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < std::mem::size_of::<BITMAPINFOHEADER>() {
        return None;
    }
    let h: BITMAPINFOHEADER = unsafe { std::ptr::read_unaligned(dib.as_ptr() as *const BITMAPINFOHEADER) };
    let (w, height) = (h.biWidth, h.biHeight);
    if w <= 0 || height == 0 || !(h.biBitCount == 32 || h.biBitCount == 24) {
        return None;
    }
    let (w, rows) = (w as usize, height.unsigned_abs() as usize);
    let bottom_up = height > 0;
    let mut offset = h.biSize as usize;
    if h.biCompression == BI_BITFIELDS.0 {
        offset += 12;
    } else if h.biCompression != BI_RGB.0 {
        return None;
    }
    let bpp = h.biBitCount as usize / 8;
    let stride = (w * bpp + 3) & !3;
    if dib.len() < offset + stride * rows {
        return None;
    }
    // 32-bit DIBs often leave alpha at 0: treat all-zero alpha as opaque.
    let any_alpha = bpp == 4 && (0..rows).any(|r| (0..w).any(|x| dib[offset + r * stride + x * 4 + 3] != 0));
    let mut rgba = Vec::with_capacity(w * rows * 4);
    for r in 0..rows {
        let src_row = if bottom_up { rows - 1 - r } else { r };
        let row = &dib[offset + src_row * stride..];
        for x in 0..w {
            let p = &row[x * bpp..];
            let a = if bpp == 4 && any_alpha { p[3] } else { 255 };
            rgba.extend_from_slice(&[p[2], p[1], p[0], a]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, rows as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

/// PNG → a 32-bit top-down CF_DIB (for apps that don't read PNG from the clipboard).
fn png_to_dib(png_data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_data));
    // 16-bit and palette PNGs → 8-bit RGB(A).
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        _ => return None,
    };
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (w * h * 4) as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(std::mem::size_of::<BITMAPINFOHEADER>() + w * h * 4);
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(&header as *const _ as *const u8, std::mem::size_of::<BITMAPINFOHEADER>())
    });
    for px in buf[..w * h * channels].chunks(channels) {
        let a = if channels == 4 { px[3] } else { 255 };
        out.extend_from_slice(&[px[2], px[1], px[0], a]);
    }
    Some(out)
}
