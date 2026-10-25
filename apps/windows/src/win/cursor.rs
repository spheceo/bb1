//! The Mac's cursor shape (a PNG, sized in points) → a Windows cursor at this screen's scale.

use crate::core::proto::CursorMsg;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HCURSOR, ICONINFO};

pub fn make(c: &CursorMsg, dpi: u32) -> Option<HCURSOR> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(&c.png));
    // 16-bit and palette PNGs → 8-bit, which is what the code below reads.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..sw * sh * 4].to_vec(),
        png::ColorType::Rgb => buf[..sw * sh * 3].chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..sw * sh * 2].chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf[..sw * sh].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    // Points → pixels on this screen.
    let scale = dpi as f64 / 96.0;
    let dw = ((c.width * scale).round() as usize).clamp(1, 256);
    let dh = ((c.height * scale).round() as usize).clamp(1, 256);
    let hot_x = ((c.hot_x * scale).round() as u32).min(dw as u32 - 1);
    let hot_y = ((c.hot_y * scale).round() as u32).min(dh as u32 - 1);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: dw as i32,
                biHeight: -(dh as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, dw * dh * 4);
        // Box-filter scale (the PNG is usually 2× for Retina, so this mostly averages 2×2 blocks).
        for y in 0..dh {
            for x in 0..dw {
                let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1).min(sw));
                let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1).min(sh));
                let mut acc = [0u32; 4];
                let mut n = 0;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let p = &rgba[(yy * sw + xx) * 4..][..4];
                        let a = p[3] as u32;
                        // Premultiply while averaging so edges don't go dark.
                        acc[0] += p[0] as u32 * a;
                        acc[1] += p[1] as u32 * a;
                        acc[2] += p[2] as u32 * a;
                        acc[3] += a;
                        n += 1;
                    }
                }
                let a = acc[3] / n.max(1);
                let (r, g, b) = if acc[3] > 0 { (acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]) } else { (0, 0, 0) };
                let o = &mut out[(y * dw + x) * 4..][..4];
                o.copy_from_slice(&[b as u8, g as u8, r as u8, a as u8]);
            }
        }
        // An all-zero AND mask (CreateBitmap without bits leaves it undefined): the alpha does the shape.
        let mask_bits = vec![0u8; dw.div_ceil(16) * 2 * dh];
        let mask = CreateBitmap(dw as i32, dh as i32, 1, 1, Some(mask_bits.as_ptr() as _));
        let info = ICONINFO { fIcon: false.into(), xHotspot: hot_x, yHotspot: hot_y, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon.map(|i| HCURSOR(i.0))
    }
}
