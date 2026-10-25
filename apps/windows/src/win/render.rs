//! The video thread: decodes frames and draws them with Direct3D 11 (NV12 → RGB in a pixel shader),
//! letterboxed in the stream window. When frames queue up it decodes them all but draws only the
//! newest, so the picture never falls behind.

use crate::core::client::Client;
use crate::core::video::FramePayload;
use crate::logi;
use crate::win::decoder::{self, Decoder, Picture};
use crossbeam_channel::{Sender, unbounded};
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, IMFMediaBuffer};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Interface, PCSTR, Result, s};

pub enum VideoCmd {
    Attach(Client),
    Frame(FramePayload, u64),
    /// The Mac's screen size (from the stream config).
    Size(u32, u32),
    Resize,
    Redraw,
    ResetDecoder,
    Quit,
}

/// Picture rectangle (x, y, w, h) for a `vw`×`vh` video in a `cw`×`ch` window, keeping its shape.
pub fn fit(cw: u32, ch: u32, vw: u32, vh: u32) -> (u32, u32, u32, u32) {
    if vw == 0 || vh == 0 || cw == 0 || ch == 0 {
        return (0, 0, cw, ch);
    }
    let scale = (cw as f64 / vw as f64).min(ch as f64 / vh as f64);
    let w = ((vw as f64 * scale).round() as u32).min(cw);
    let h = ((vh as f64 * scale).round() as u32).min(ch);
    ((cw - w) / 2, (ch - h) / 2, w, h)
}

pub fn spawn(hwnd: HWND) -> Sender<VideoCmd> {
    let (tx, rx) = unbounded::<VideoCmd>();
    let hwnd_raw = hwnd.0 as isize;
    std::thread::Builder::new()
        .name("bb1 video".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let hwnd = HWND(hwnd_raw as _);
            let mut renderer = match Renderer::new(hwnd) {
                Ok(r) => r,
                Err(e) => {
                    logi!("video: Direct3D 11 didn't start: {e}");
                    crate::win::app::post(crate::win::app::UiEvent::Notice(format!("bb1 couldn't start Direct3D 11 on this PC, so it can't show the picture ({e}).")));
                    return;
                }
            };
            let mut client: Option<Client> = None;
            let mut decoder: Option<Decoder> = None;
            let mut parameter_sets: Vec<Vec<u8>> = Vec::new();
            let mut warned_hevc = false;
            let mut decode_avg_us = 0.0f64;
            let mut decoded_since_log = 0u64;
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let mut shown: Option<u64> = None;
                let mut redraw = false;
                for cmd in batch {
                    match cmd {
                        VideoCmd::Quit => return,
                        VideoCmd::Attach(c) => client = Some(c),
                        VideoCmd::Size(w, h) => {
                            renderer.video_size = (w, h);
                            redraw = true;
                        }
                        VideoCmd::Resize => {
                            renderer.resize();
                            redraw = true;
                        }
                        VideoCmd::Redraw => redraw = true,
                        VideoCmd::ResetDecoder => {
                            decoder = None;
                            parameter_sets.clear();
                        }
                        VideoCmd::Frame(payload, capture_us) => {
                            if payload.codec != crate::core::proto::CODEC_H264 {
                                if !warned_hevc {
                                    warned_hevc = true;
                                    logi!("video: ignoring HEVC frames (update bb1 on the Mac)");
                                }
                                continue;
                            }
                            // New parameter sets (new encoder, new size): start a fresh decoder.
                            if !payload.parameter_sets.is_empty() && payload.parameter_sets != parameter_sets {
                                let (w, h) = renderer.video_size;
                                let (w, h) = if w == 0 { (1920, 1080) } else { (w, h) };
                                decoder = None;
                                match Decoder::new(w, h, Some(&renderer.device)) {
                                    Ok(d) => {
                                        decoder = Some(d);
                                        parameter_sets = payload.parameter_sets.clone();
                                    }
                                    Err(e) => {
                                        logi!("video: couldn't create the H.264 decoder: {e}");
                                        crate::win::app::post(crate::win::app::UiEvent::Notice(format!("This PC couldn't start its H.264 video decoder ({e}).")));
                                    }
                                }
                            }
                            let Some(dec) = decoder.as_mut() else {
                                if let Some(c) = &client {
                                    c.decoder_failed("no decoder yet");
                                }
                                continue;
                            };
                            let Some(annex_b) = payload.annex_b() else {
                                logi!("video: a frame wasn't valid H.264 data");
                                if let Some(c) = &client {
                                    c.decoder_failed("bad frame data");
                                }
                                continue;
                            };
                            let started = std::time::Instant::now();
                            match dec.decode(&annex_b) {
                                Ok(pictures) => {
                                    let us = started.elapsed().as_micros() as f64;
                                    decode_avg_us = if decode_avg_us == 0.0 { us } else { decode_avg_us * 0.95 + us * 0.05 };
                                    for p in pictures {
                                        if let Some(c) = &client {
                                            c.mark_decoded();
                                        }
                                        decoded_since_log += 1;
                                        match renderer.upload(dec, &p) {
                                            Ok(()) => shown = Some(capture_us),
                                            Err(e) => logi!("video: couldn't copy a picture to the GPU: {e}"),
                                        }
                                    }
                                }
                                Err(e) => {
                                    logi!("video: decode failed ({e}); waiting for a keyframe");
                                    decoder = None;
                                    parameter_sets.clear();
                                    if let Some(c) = &client {
                                        c.decoder_failed("decode failed");
                                    }
                                }
                            }
                        }
                    }
                }
                if shown.is_some() || redraw {
                    if let Err(e) = renderer.draw() {
                        logi!("video: drawing failed ({e}); starting Direct3D again");
                        let size = renderer.video_size;
                        // The old swap chain must be gone first: a window takes only one flip-model
                        // swap chain. The decoder's textures belonged to the old device too.
                        decoder = None;
                        parameter_sets.clear();
                        drop(renderer);
                        let mut attempt = 0;
                        renderer = loop {
                            match Renderer::new(hwnd) {
                                Ok(r) => break r,
                                Err(e) if attempt < 20 => {
                                    attempt += 1;
                                    logi!("video: Direct3D 11 didn't restart ({e}); trying again");
                                    std::thread::sleep(std::time::Duration::from_millis(250));
                                }
                                Err(e) => {
                                    logi!("video: Direct3D 11 didn't restart: {e}");
                                    return;
                                }
                            }
                        };
                        renderer.video_size = size;
                        if let Some(c) = &client {
                            c.decoder_failed("display restarted");
                        }
                    }
                    if let (Some(capture_us), Some(c)) = (shown, &client) {
                        c.mark_displayed(capture_us);
                    }
                }
                if decoded_since_log >= 600 {
                    decoded_since_log = 0;
                    logi!("video: decode avg {:.1} ms ({})", decode_avg_us / 1000.0,
                          if decoder.as_ref().is_some_and(|d| d.hardware) { "GPU" } else { "CPU" });
                }
            }
        })
        .unwrap();
    tx
}

const SHADER: &str = r#"
cbuffer C : register(b0) { float2 uvScale; float2 pad; };
Texture2D<float> texY : register(t0);
Texture2D<float2> texUV : register(t1);
SamplerState samp : register(s0);
struct VO { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VO vs_main(uint id : SV_VertexID) {
    VO o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = t * uvScale;
    return o;
}
float4 ps_main(VO i) : SV_Target {
    // BT.709, video range (what the Mac's encoder produces).
    float y = (texY.Sample(samp, i.uv) - 16.0 / 255.0) * (255.0 / 219.0);
    float2 c = (texUV.Sample(samp, i.uv) - 128.0 / 255.0) * (255.0 / 224.0);
    float3 rgb = float3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    return float4(saturate(rgb), 1);
}
"#;

struct Frame {
    texture: ID3D11Texture2D,
    y: ID3D11ShaderResourceView,
    uv: ID3D11ShaderResourceView,
    size: (u32, u32),
    staging: Option<ID3D11Texture2D>,
}

struct Renderer {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    target: Option<ID3D11RenderTargetView>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    frame: Option<Frame>,
    /// The Mac's screen size; the decoded frame can be a little bigger (rounded up to 16).
    video_size: (u32, u32),
    tearing: bool,
    back_size: (u32, u32),
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(SHADER.as_ptr() as _, SHADER.len(), s!("bb1"), None, None, entry, target, 0, 0, &mut code, Some(&mut errors));
        if let Err(e) = r {
            if let Some(err) = errors {
                let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
                logi!("video: shader didn't compile: {}", String::from_utf8_lossy(msg));
            }
            return Err(e);
        }
        let code = code.unwrap();
        Ok(std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()).to_vec())
    }
}

impl Renderer {
    fn new(hwnd: HWND) -> Result<Renderer> {
        unsafe {
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
            let create = |flags: D3D11_CREATE_DEVICE_FLAG| -> Result<(Option<ID3D11Device>, Option<ID3D11DeviceContext>)> {
                let mut device = None;
                let mut context = None;
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, Some(&levels), D3D11_SDK_VERSION,
                                  Some(&mut device), None, Some(&mut context))?;
                Ok((device, context))
            };
            // Without a video-capable driver (a VM, the basic display driver) VIDEO_SUPPORT fails the
            // whole device: then draw anyway and decode on the CPU.
            let (device, context) = match create(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT) {
                Ok(r) => r,
                Err(e) => {
                    logi!("video: no Direct3D 11 video support ({e}); decoding on the CPU");
                    create(D3D11_CREATE_DEVICE_BGRA_SUPPORT)?
                }
            };
            let device: ID3D11Device = device.unwrap();
            let context: ID3D11DeviceContext = context.unwrap();
            // The decoder uses the device from its own threads.
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let dxgi_device: IDXGIDevice1 = device.cast()?;
            let _ = dxgi_device.SetMaximumFrameLatency(1);
            let adapter = dxgi_device.GetAdapter()?;
            if let Ok(desc) = adapter.GetDesc() {
                let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
                logi!("video: Direct3D 11 on {name}");
            }
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let mut tearing = windows::core::BOOL(0);
            if let Ok(f5) = factory.cast::<IDXGIFactory5>() {
                let _ = f5.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut tearing as *mut _ as _, 4);
            }
            let tearing = tearing.as_bool();
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 0,
                Height: 0,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);

            let vs_code = compile(s!("vs_main"), s!("vs_4_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_4_0"))?;
            let mut vs = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut constants = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut constants))?;
            let mut r = Renderer {
                hwnd,
                device,
                context,
                swap,
                target: None,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                sampler: sampler.unwrap(),
                constants: constants.unwrap(),
                frame: None,
                video_size: (0, 0),
                tearing,
                back_size: (0, 0),
            };
            r.make_target()?;
            Ok(r)
        }
    }

    fn make_target(&mut self) -> Result<()> {
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            self.back_size = (desc.Width, desc.Height);
            let mut rtv = None;
            self.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            self.target = rtv;
            Ok(())
        }
    }

    fn resize(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (w, h) = (rc.right.max(1) as u32, rc.bottom.max(1) as u32);
        if (w, h) == self.back_size {
            return;
        }
        self.target = None;
        unsafe {
            self.context.OMSetRenderTargets(None, None);
            self.context.Flush();
            let flags = if self.tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING } else { DXGI_SWAP_CHAIN_FLAG(0) };
            if let Err(e) = self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, flags) {
                logi!("video: resize failed: {e}");
            }
        }
        if let Err(e) = self.make_target() {
            logi!("video: couldn't get the back buffer after resizing: {e}");
        }
    }

    /// Makes (or reuses) the NV12 texture the shader samples from.
    fn frame_texture(&mut self, size: (u32, u32), staging: bool) -> Result<&mut Frame> {
        let size = (size.0 & !1, size.1 & !1);
        if self.frame.as_ref().map(|f| f.size) != Some(size) || (staging && self.frame.as_ref().is_some_and(|f| f.staging.is_none())) {
            unsafe {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: size.0,
                    Height: size.1,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                let mut texture = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = texture.unwrap();
                let view = |format: DXGI_FORMAT| -> Result<ID3D11ShaderResourceView> {
                    let d = D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: format,
                        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
                    };
                    let mut v = None;
                    self.device.CreateShaderResourceView(&texture, Some(&d), Some(&mut v))?;
                    Ok(v.unwrap())
                };
                let y = view(DXGI_FORMAT_R8_UNORM)?;
                let uv = view(DXGI_FORMAT_R8G8_UNORM)?;
                let staging_tex = if staging {
                    let sd = D3D11_TEXTURE2D_DESC {
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        ..desc
                    };
                    let mut st = None;
                    self.device.CreateTexture2D(&sd, None, Some(&mut st))?;
                    st
                } else {
                    None
                };
                logi!("video: picture texture {}x{}", size.0, size.1);
                self.frame = Some(Frame { texture, y, uv, size, staging: staging_tex });
            }
        }
        Ok(self.frame.as_mut().unwrap())
    }

    /// Copies a decoded picture into the texture that gets drawn.
    fn upload(&mut self, dec: &Decoder, p: &Picture) -> Result<()> {
        let size = dec.frame_size;
        if dec.hardware {
            let Some((src, index)) = decoder::texture_of(p) else {
                return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "decoded picture has no texture"));
            };
            let context = self.context.clone();
            let mut src_desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { src.GetDesc(&mut src_desc) };
            let frame = self.frame_texture(size, false)?;
            // The box has to fit inside both textures (or nothing is copied).
            let b = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: frame.size.0.min(src_desc.Width & !1),
                bottom: frame.size.1.min(src_desc.Height & !1),
                back: 1,
            };
            unsafe {
                context.CopySubresourceRegion(&frame.texture, 0, 0, 0, 0, &src, index, Some(&b));
            }
            return Ok(());
        }
        // CPU-decoded: lock the buffer, copy rows into a staging texture, then onto the GPU.
        unsafe {
            let buffer: IMFMediaBuffer = p.sample.GetBufferByIndex(0)?;
            let two_d: Option<IMF2DBuffer> = buffer.cast().ok();
            let mut data = std::ptr::null_mut();
            let mut pitch = dec.stride as i32;
            let locked_2d = match &two_d {
                Some(b) => b.Lock2D(&mut data, &mut pitch).is_ok(),
                None => false,
            };
            if !locked_2d {
                buffer.Lock(&mut data, None, None)?;
            }
            let context = self.context.clone();
            let result = (|| -> Result<()> {
                let frame = self.frame_texture(size, true)?;
                let staging = frame.staging.clone().unwrap();
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context.Map(&staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))?;
                let (w, h) = frame.size;
                let src_pitch = pitch.unsigned_abs() as usize;
                let dst_pitch = mapped.RowPitch as usize;
                let dst = mapped.pData as *mut u8;
                // Y plane, then the interleaved UV plane (half height) right after `size.1` rows.
                for row in 0..h as usize {
                    std::ptr::copy_nonoverlapping(data.add(row * src_pitch), dst.add(row * dst_pitch), w as usize);
                }
                let src_uv = data.add(src_pitch * size.1 as usize);
                let dst_uv = dst.add(dst_pitch * h as usize);
                for row in 0..(h / 2) as usize {
                    std::ptr::copy_nonoverlapping(src_uv.add(row * src_pitch), dst_uv.add(row * dst_pitch), w as usize);
                }
                context.Unmap(&staging, 0);
                context.CopyResource(&frame.texture, &staging);
                Ok(())
            })();
            if locked_2d {
                let _ = two_d.unwrap().Unlock2D();
            } else {
                let _ = buffer.Unlock();
            }
            result
        }
    }

    fn draw(&mut self) -> Result<()> {
        if self.target.is_none() {
            self.resize();
        }
        let Some(target) = self.target.clone() else { return Ok(()) };
        unsafe {
            let ctx = &self.context;
            ctx.ClearRenderTargetView(&target, &[0.0, 0.0, 0.0, 1.0]);
            if let Some(frame) = &self.frame {
                let (vw, vh) = if self.video_size.0 > 0 { self.video_size } else { frame.size };
                let (x, y, w, h) = fit(self.back_size.0, self.back_size.1, vw, vh);
                // Only the Mac's screen, not the decoder's padding rows.
                let scale = [vw.min(frame.size.0) as f32 / frame.size.0 as f32, vh.min(frame.size.1) as f32 / frame.size.1 as f32, 0.0, 0.0];
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if ctx.Map(&self.constants, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
                    std::ptr::copy_nonoverlapping(scale.as_ptr(), mapped.pData as *mut f32, 4);
                    ctx.Unmap(&self.constants, 0);
                }
                ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
                ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    TopLeftX: x as f32,
                    TopLeftY: y as f32,
                    Width: w as f32,
                    Height: h as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]));
                ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                ctx.IASetInputLayout(None);
                ctx.VSSetShader(&self.vs, None);
                ctx.VSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
                ctx.PSSetShader(&self.ps, None);
                ctx.PSSetShaderResources(0, Some(&[Some(frame.y.clone()), Some(frame.uv.clone())]));
                ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
                ctx.Draw(3, 0);
            }
            let flags = if self.tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) };
            self.swap.Present(0, flags).ok()
        }
    }
}
