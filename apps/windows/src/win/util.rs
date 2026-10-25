use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{CreateFontW, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW, SetWindowTextW};
use windows::core::{HSTRING, PCWSTR};

/// A NUL-terminated UTF-16 copy of `s`, kept alive by the returned Vec.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(w: &[u16]) -> PCWSTR {
    PCWSTR(w.as_ptr())
}

pub fn set_text(hwnd: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(text));
    }
}

pub fn get_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn dpi(hwnd: HWND) -> u32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 { 96 } else { d }
}

/// Pixels for `points` at this window's DPI.
pub fn scale(hwnd: HWND, v: i32) -> i32 {
    v * dpi(hwnd) as i32 / 96
}

/// Segoe UI at this window's DPI (the standard Windows UI font).
pub fn ui_font(hwnd: HWND, points: i32, bold: bool) -> HFONT {
    let height = -(points * dpi(hwnd) as i32 / 72);
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { 600 } else { 400 },
            0,
            0,
            0,
            FONT_CHARSET(1),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5), // ClearType
            0,
            pcwstr(&face),
        )
    }
}
