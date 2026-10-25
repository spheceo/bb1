//! Drawing in the Mac app's style (apps/mac/Sources/bb1/App/DashboardViews.swift): flat, solid
//! greys and white, like the app icon. Direct2D for shapes and pictures, DirectWrite for text,
//! WIC for decoding screen previews. All sizes are in points (DIPs), like on the Mac.

use std::collections::HashMap;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Result, w};

#[derive(Clone, Copy)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    pub fn white(alpha: f32) -> Color {
        Color(1.0, 1.0, 1.0, alpha)
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

/// The Mac app's graphite: a grey with a touch of blue.
pub fn graphite(v: f32) -> Color {
    Color(v, v * 1.03, v * 1.12, 1.0)
}

pub mod style {
    use super::{Color, graphite};
    pub fn background() -> Color { graphite(0.035) }
    pub fn card() -> Color { graphite(0.07) }
    pub fn card_hover() -> Color { graphite(0.09) }
    pub fn card_border() -> Color { Color::white(0.06) }
    pub fn screen() -> Color { graphite(0.055) }
    pub fn ink() -> Color { graphite(0.07) }
    pub fn online() -> Color { Color(0.18, 0.85, 0.53, 1.0) }
    pub fn label() -> Color { Color::white(0.88) }
}

/// COLORREF (0x00BBGGRR) for the title bar.
pub fn colorref(c: Color) -> u32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    b(c.0) | (b(c.1) << 8) | (b(c.2) << 16)
}

/// Dark title bar in the window's background colour (Windows 11; Windows 10 just gets dark mode).
pub fn dark_title_bar(hwnd: HWND) {
    unsafe {
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &on as *const _ as _, 4); // USE_IMMERSIVE_DARK_MODE
        let caption = colorref(style::background());
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as _, 4); // CAPTION_COLOR
        let text = colorref(Color::white(0.9));
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), &text as *const _ as _, 4); // TEXT_COLOR
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Font {
    /// "Your Macs"
    Title,
    /// Tile names
    Name,
    /// Status lines and messages
    Small,
    /// Pills
    Button,
    /// Empty-state heading
    Heading,
}

impl Font {
    fn spec(self) -> (f32, i32) {
        match self {
            Font::Title => (30.0, 700),
            Font::Name => (14.5, 600),
            Font::Small => (12.0, 400),
            Font::Button => (12.5, 600),
            Font::Heading => (15.0, 600),
        }
    }
}

pub struct Gfx {
    pub factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
    pub target: Option<ID2D1HwndRenderTarget>,
    formats: HashMap<(Font, bool), IDWriteTextFormat>,
    brushes: HashMap<u32, ID2D1SolidColorBrush>,
    /// Decoded pictures by key, with the version they were made from.
    bitmaps: HashMap<String, (u64, ID2D1Bitmap, f32, f32)>,
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            Ok(Gfx {
                factory: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                wic: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?,
                target: None,
                formats: HashMap::new(),
                brushes: HashMap::new(),
                bitmaps: HashMap::new(),
            })
        }
    }

    /// Starts a frame: (re)creates the render target at the window's size and DPI. Returns the size in points.
    pub fn begin(&mut self, hwnd: HWND) -> Result<(f32, f32)> {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let size = D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 };
        let dpi = crate::win::util::dpi(hwnd) as f32;
        if self.target.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
            let t = unsafe { self.factory.CreateHwndRenderTarget(&props, &hwnd_props)? };
            self.target = Some(t);
            self.brushes.clear();
            self.bitmaps.clear();
        }
        let t = self.target.as_ref().unwrap();
        unsafe {
            t.SetDpi(dpi, dpi);
            let _ = t.Resize(&size);
            t.BeginDraw();
            t.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        Ok((size.width as f32 * 96.0 / dpi, size.height as f32 * 96.0 / dpi))
    }

    /// Ends a frame. A lost device (driver update, remote desktop…) is made again next time.
    pub fn end(&mut self) {
        if let Some(t) = &self.target {
            if unsafe { t.EndDraw(None, None) }.is_err() {
                self.target = None;
            }
        }
    }

    fn t(&self) -> &ID2D1HwndRenderTarget {
        self.target.as_ref().unwrap()
    }

    pub fn brush(&mut self, c: Color) -> ID2D1SolidColorBrush {
        let key = colorref(c) | (((c.3 * 255.0) as u32) << 24);
        if let Some(b) = self.brushes.get(&key) {
            return b.clone();
        }
        let b = unsafe { self.t().CreateSolidColorBrush(&c.d2d(), None).unwrap() };
        self.brushes.insert(key, b.clone());
        b
    }

    pub fn clear(&mut self, c: Color) {
        unsafe { self.t().Clear(Some(&c.d2d())) };
    }

    fn format(&mut self, font: Font, centered: bool) -> IDWriteTextFormat {
        if let Some(f) = self.formats.get(&(font, centered)) {
            return f.clone();
        }
        let (size, weight) = font.spec();
        unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT(weight), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))
                .unwrap();
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if centered {
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            }
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                let _ = f.SetTrimming(&trimming, &sign);
            }
            self.formats.insert((font, centered), f.clone());
            f
        }
    }

    /// Width of `text` in points.
    pub fn measure(&mut self, text: &str, font: Font) -> f32 {
        let f = self.format(font, false);
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide(text), &f, 10_000.0, 1000.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }

    pub fn text(&mut self, text: &str, font: Font, r: D2D_RECT_F, c: Color) {
        self.text_aligned(text, font, r, c, false);
    }

    pub fn text_centered(&mut self, text: &str, font: Font, r: D2D_RECT_F, c: Color) {
        self.text_aligned(text, font, r, c, true);
    }

    fn text_aligned(&mut self, text: &str, font: Font, r: D2D_RECT_F, c: Color, centered: bool) {
        let f = self.format(font, centered);
        let b = self.brush(c);
        unsafe {
            self.t().DrawText(&wide(text), &f, &r, &b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn fill_round(&mut self, r: D2D_RECT_F, radius: f32, c: Color) {
        let b = self.brush(c);
        unsafe { self.t().FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &b) };
    }

    pub fn stroke_round(&mut self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
        let b = self.brush(c);
        let inset = D2D_RECT_F { left: r.left + width / 2.0, top: r.top + width / 2.0, right: r.right - width / 2.0, bottom: r.bottom - width / 2.0 };
        unsafe {
            self.t().DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: inset, radiusX: radius, radiusY: radius }, &b, width, None);
        }
    }

    pub fn fill_rect(&mut self, r: D2D_RECT_F, c: Color) {
        let b = self.brush(c);
        unsafe { self.t().FillRectangle(&r, &b) };
    }

    pub fn fill_circle(&mut self, x: f32, y: f32, radius: f32, c: Color) {
        let b = self.brush(c);
        let e = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: x, Y: y }, radiusX: radius, radiusY: radius };
        unsafe { self.t().FillEllipse(&e, &b) };
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        let b = self.brush(c);
        unsafe {
            self.t().DrawLine(windows_numerics::Vector2 { X: x0, Y: y0 }, windows_numerics::Vector2 { X: x1, Y: y1 }, &b, width, None);
        }
    }

    /// Draws inside a rounded rectangle only (for pictures in rounded cards).
    pub fn push_round_clip(&mut self, r: D2D_RECT_F, radius: f32) {
        unsafe {
            let Ok(g) = self.factory.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }) else { return };
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: r,
                geometricMask: std::mem::ManuallyDrop::new(Some(g.into())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            self.t().PushLayer(&params, None);
            drop(std::mem::ManuallyDrop::into_inner(params.geometricMask));
        }
    }

    pub fn pop_clip(&mut self) {
        unsafe { self.t().PopLayer() };
    }

    pub fn push_rect_clip(&mut self, r: D2D_RECT_F) {
        unsafe { self.t().PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_rect_clip(&mut self) {
        unsafe { self.t().PopAxisAlignedClip() };
    }

    /// The bb1 mark: a rounded screen outline with its left half solid, at `opacity` white.
    pub fn mark(&mut self, r: D2D_RECT_F, opacity: f32, behind: Color) {
        // Blend with what's behind once, so the fill and the outline don't double up.
        let c = Color(behind.0 + (1.0 - behind.0) * opacity, behind.1 + (1.0 - behind.1) * opacity, behind.2 + (1.0 - behind.2) * opacity, 1.0);
        let w = r.right - r.left;
        let line = w * 7.5 / 76.0;
        let radius = w * 14.0 / 76.0;
        self.stroke_round(r, radius, c, line);
        self.push_rect_clip(D2D_RECT_F { left: r.left - line, top: r.top - line, right: r.left + w / 2.0, bottom: r.bottom + line });
        self.fill_round(r, radius, c);
        self.pop_rect_clip();
    }

    /// A pill button: solid white (primary) or a faint outline (secondary). Returns its rectangle.
    pub fn pill(&mut self, label: &str, right: f32, center_y: f32, primary: bool, hovering: bool, small: bool) -> D2D_RECT_F {
        let height = if small { 26.0 } else { 32.0 };
        let width = self.measure(label, Font::Button) + if small { 22.0 } else { 32.0 };
        let r = rect(right - width, center_y - height / 2.0, width, height);
        if primary {
            self.fill_round(r, height / 2.0, Color::white(if hovering { 0.84 } else { 0.95 }));
            self.text_centered(label, Font::Button, text_box(r, Font::Button), style::ink());
        } else {
            self.fill_round(r, height / 2.0, Color::white(if hovering { 0.1 } else { 0.05 }));
            self.stroke_round(r, height / 2.0, Color::white(0.16), 1.0);
            self.text_centered(label, Font::Button, text_box(r, Font::Button), style::label());
        }
        r
    }

    /// A JPEG as a bitmap, decoded once per `version`. `dim`: grey and dark (an offline Mac).
    pub fn jpeg(&mut self, key: &str, version: u64, jpeg: &[u8], dim: bool) -> Option<(ID2D1Bitmap, f32, f32)> {
        let key = format!("{key}{}", if dim { "#dim" } else { "" });
        if let Some((v, b, w, h)) = self.bitmaps.get(&key) {
            if *v == version {
                return Some((b.clone(), *w, *h));
            }
        }
        let (mut pixels, w, h) = decode_jpeg(&self.wic, jpeg).ok()?;
        if dim {
            for px in pixels.chunks_mut(4) {
                let grey = (0.299 * px[2] as f32 + 0.587 * px[1] as f32 + 0.114 * px[0] as f32) * 0.42;
                px[0] = grey as u8;
                px[1] = grey as u8;
                px[2] = grey as u8;
            }
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bitmap = unsafe { self.t().CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(pixels.as_ptr() as _), w * 4, &props).ok()? };
        self.bitmaps.insert(key, (version, bitmap.clone(), w as f32, h as f32));
        Some((bitmap, w as f32, h as f32))
    }

    /// Fills `dest` with the picture, cropping to keep its shape (like the Mac's aspect fill).
    pub fn picture_fill(&mut self, bitmap: &ID2D1Bitmap, bw: f32, bh: f32, dest: D2D_RECT_F) {
        let (dw, dh) = (dest.right - dest.left, dest.bottom - dest.top);
        let scale = (dw / bw).max(dh / bh);
        let (sw, sh) = (dw / scale, dh / scale);
        let src = rect((bw - sw) / 2.0, (bh - sh) / 2.0, sw, sh);
        unsafe {
            self.t().DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, Some(&src));
        }
    }
}

/// The text box for a label vertically centred in `r`.
fn text_box(r: D2D_RECT_F, font: Font) -> D2D_RECT_F {
    let (size, _) = font.spec();
    let h = size * 1.4;
    let y = (r.top + r.bottom) / 2.0 - h / 2.0;
    D2D_RECT_F { left: r.left, top: y, right: r.right, bottom: y + h }
}

/// A text box of one line at `y` (top) for `font`.
pub fn line_box(x: f32, y: f32, w: f32, font: Font) -> D2D_RECT_F {
    rect(x, y, w, font.spec().0 * 1.45)
}

fn decode_jpeg(wic: &IWICImagingFactory, jpeg: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    unsafe {
        let stream = wic.CreateStream()?;
        stream.InitializeFromMemory(jpeg)?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let (mut w, mut h) = (0, 0);
        converter.GetSize(&mut w, &mut h)?;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        converter.CopyPixels(std::ptr::null(), w * 4, &mut pixels)?;
        Ok((pixels, w, h))
    }
}
