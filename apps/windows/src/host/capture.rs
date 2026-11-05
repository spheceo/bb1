//! Screen capture with DXGI Desktop Duplication, on the GPU from start to finish.
//!
//! `Capture::new(output)` finds the monitor (0 = the primary one, then the others in DXGI order),
//! creates a Direct3D 11 device on the graphics card that drives it (BGRA + video support, made
//! multithread-safe because encoders on other threads use the same device and immediate context),
//! and starts duplicating that monitor. `next()` waits for the screen to change, copies the new
//! picture into one of our own textures (a ring of three, so the frame an encoder is still reading
//! isn't overwritten by the next one), draws the mouse pointer into the copy with a tiny shader, and
//! releases the duplicated frame straight away. Nothing is read back to the CPU.
//!
//! Games: bb1 asks for the highest GPU priority it may have (`SetGPUThreadPriority(7)` on the
//! device, and the process's GPU scheduling class through `D3DKMTSetProcessSchedulingPriorityClass`)
//! so capture doesn't queue behind a game that keeps the GPU at 100%, and the thread calling
//! `next()` runs at time-critical priority. REALTIME scheduling is only asked for when bb1 runs as
//! admin and the GPU isn't NVIDIA: NVIDIA drivers are known to stall encoding with REALTIME when
//! hardware-accelerated GPU scheduling is on (Sunshine found the same), so NVIDIA gets HIGH.
//!
//! Recovery: a resolution change, a full-screen game switching modes, UAC / the lock screen (the
//! secure desktop) all end the duplication with ACCESS_LOST. `next()` then opens it again (and
//! makes new textures when the size changed) and returns None meanwhile; it returns an error only
//! when the screen has been unavailable for 30 s, or the graphics device itself is gone (driver
//! update or crash), which needs a new `Capture` (and new encoders, since they hold the device).
//!
//! Locking: our copy + pointer drawing runs inside `ID3D11Multithread::Enter/Leave`, so it's never
//! interleaved with another thread's use of the immediate context. Code on other threads that sets
//! pipeline state over several calls should hold the same lock.
//!
//! Not handled yet: rotated (portrait) monitors are captured unrotated; HDR desktops are converted
//! to 8-bit by Windows (DuplicateOutput1 is asked for BGRA only).

use crate::logi;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetCurrentThreadId, OpenProcessToken, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::{Interface, PCSTR, s, w};

/// Our textures handed to encoders. Three: the one being filled, the one handed out last, and one
/// spare for an encoder that runs a little behind.
const RING: usize = 3;
/// How long the screen may stay unavailable (secure desktop, a monitor switched off) before
/// `next()` reports an error.
const GIVE_UP_AFTER: Duration = Duration::from_secs(30);

const SHADER: &str = r#"
cbuffer Rect : register(b0) { float4 rect; };   // left, top, right, bottom in clip space
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs_main(uint id : SV_VertexID) {
    float2 t = float2(id & 1, id >> 1);           // 4-vertex triangle strip
    V o;
    o.pos = float4(lerp(rect.x, rect.z, t.x), lerp(rect.y, rect.w, t.y), 0.0, 1.0);
    o.uv = t;
    return o;
}
Texture2D shape : register(t0);
SamplerState point_sampler : register(s0);
float4 ps_main(V i) : SV_Target { return shape.Sample(point_sampler, i.uv); }
"#;

struct Slot {
    texture: ID3D11Texture2D,
    target: ID3D11RenderTargetView,
}

/// The pointer as two small textures: `normal` is alpha-blended (straight alpha), `invert` (when the
/// shape has any) XORs the screen: white pixels invert what's under them, black leave it alone.
struct PointerShape {
    width: u32,
    height: u32,
    normal: ID3D11ShaderResourceView,
    invert: Option<ID3D11ShaderResourceView>,
}

struct Pointer {
    visible: bool,
    x: i32,
    y: i32,
    shape: Option<PointerShape>,
    buffer: Vec<u8>,
}

/// The shader pipeline that draws the pointer.
struct PointerPipeline {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend_alpha: ID3D11BlendState,
    blend_invert: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    rect: ID3D11Buffer,
}

pub struct Capture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    lock: Option<ID3D11Multithread>,
    adapter: IDXGIAdapter1,
    /// The monitor's name (\\.\DISPLAY1) and its index on `adapter`, to find it again after a mode change.
    output_name: [u16; 32],
    output_local: u32,
    duplication: Option<IDXGIOutputDuplication>,
    size: (u32, u32),
    ring: Vec<Slot>,
    next_slot: usize,
    latest: Option<usize>,
    pointer: Pointer,
    pipeline: PointerPipeline,
    draw_cursor: bool,
    /// Produce the next acquired frame even if only the pointer (or nothing) changed: after the
    /// duplication was (re)opened, or drawing the pointer was switched.
    need_frame: bool,
    failing_since: Option<Instant>,
    failures_logged: u32,
    priority_thread: u32,
}

/// `Err` from a step of `next()`: either the duplication is gone (open it again) or worse.
enum Failure {
    Lost(String),
    Fatal(String),
}

impl Capture {
    pub fn new(output: usize) -> Result<Capture, String> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
            let found = find_output(&factory, output)?;
            let adesc = found.adapter.GetDesc1().map_err(|e| format!("adapter: {e}"))?;
            let gpu = wide(&adesc.Description);
            let (device, context) = create_device(&found.adapter).map_err(|e| format!("Direct3D 11 on {gpu}: {e}"))?;
            // Encoders use this device from their own threads. ID3D11Multithread is documented on
            // the immediate context; some drivers also answer it on the device.
            let lock = context.cast::<ID3D11Multithread>().or_else(|_| device.cast::<ID3D11Multithread>()).ok();
            match &lock {
                Some(mt) => {
                    let _ = mt.SetMultithreadProtected(true);
                }
                None => logi!("capture: no ID3D11Multithread; the device isn't protected against use from several threads"),
            }
            raise_gpu_priority(&device, adesc.VendorId);
            let pipeline = PointerPipeline::new(&device).map_err(|e| format!("pointer shader: {e}"))?;
            let r = found.desc.DesktopCoordinates;
            logi!(
                "capture: {} ({}×{} at {},{}) on {gpu}",
                wide(&found.desc.DeviceName),
                r.right - r.left,
                r.bottom - r.top,
                r.left,
                r.top
            );
            let mut capture = Capture {
                device,
                context,
                lock,
                adapter: found.adapter,
                output_name: found.desc.DeviceName,
                output_local: found.local,
                duplication: None,
                size: ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32),
                ring: Vec::new(),
                next_slot: 0,
                latest: None,
                pointer: Pointer { visible: false, x: 0, y: 0, shape: None, buffer: Vec::new() },
                pipeline,
                draw_cursor: true,
                need_frame: true,
                failing_since: None,
                failures_logged: 0,
                priority_thread: 0,
            };
            // The first open must work, so a host that can't capture at all says why up front.
            capture.open().map_err(|e| match e {
                Failure::Lost(e) | Failure::Fatal(e) => e,
            })?;
            Ok(capture)
        }
    }

    /// The device every encoder must use (same adapter as the monitor, so no copies between GPUs).
    pub fn device(&self) -> ID3D11Device {
        self.device.clone()
    }

    /// Current desktop size in pixels (changes after a resolution change).
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The last frame `next()` produced, for keyframes while the screen is still. None after a size
    /// change until the next frame.
    pub fn latest(&self) -> Option<ID3D11Texture2D> {
        self.latest.map(|i| self.ring[i].texture.clone())
    }

    pub fn set_draw_cursor(&mut self, on: bool) {
        if on != self.draw_cursor {
            self.draw_cursor = on;
            self.need_frame = true;
        }
    }

    /// Waits up to `timeout_ms` for the screen to change. Some(texture) = a new frame: our own copy
    /// (BGRA, `size()`, on `device()`), pointer drawn in, safe for encoders until the next call.
    /// None = nothing changed (or the screen is briefly unavailable).
    pub fn next(&mut self, timeout_ms: u32) -> Result<Option<ID3D11Texture2D>, String> {
        self.raise_thread_priority();
        if self.duplication.is_none()
            && let Err(f) = self.open()
        {
            return self.failed(f, timeout_ms);
        }
        let Some(dup) = self.duplication.clone() else { return Ok(None) };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        if let Err(e) = unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            let code = e.code();
            if code == DXGI_ERROR_WAIT_TIMEOUT {
                self.recovered();
                return Ok(None);
            }
            if code == DXGI_ERROR_INVALID_CALL {
                // A frame is still held (shouldn't happen: we always release). Release and go on.
                unsafe {
                    let _ = dup.ReleaseFrame();
                }
                return Ok(None);
            }
            self.duplication = None;
            return self.failed(self.classify(format!("AcquireNextFrame: {e}")), timeout_ms);
        }
        let produced = self.process(&dup, &info, resource);
        let released = unsafe { dup.ReleaseFrame() };
        match produced {
            Ok(frame) => {
                if let Err(e) = released
                    && e.code() == DXGI_ERROR_ACCESS_LOST
                {
                    self.duplication = None;
                }
                self.recovered();
                Ok(frame)
            }
            Err(f) => {
                self.duplication = None;
                self.failed(f, timeout_ms)
            }
        }
    }

    /// Copies the acquired frame (if something worth sending changed) and draws the pointer.
    fn process(&mut self, dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>) -> Result<Option<ID3D11Texture2D>, Failure> {
        let pointer_changed = self.update_pointer(dup, info)?;
        let image_changed = info.LastPresentTime != 0;
        if !image_changed && !self.need_frame && !(self.draw_cursor && pointer_changed) {
            return Ok(None);
        }
        // With only the pointer changed the desktop image is still the whole, current picture.
        let Some(resource) = resource else { return Ok(None) };
        let source: ID3D11Texture2D = resource.cast().map_err(|e| Failure::Lost(format!("desktop texture: {e}")))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(Failure::Lost(format!("desktop format {} isn't BGRA", desc.Format.0)));
        }
        if (desc.Width, desc.Height) != self.size || self.ring.is_empty() {
            self.make_ring(desc.Width, desc.Height)?;
        }
        let slot = self.next_slot;
        unsafe {
            if let Some(mt) = &self.lock {
                mt.Enter();
            }
            self.context.CopyResource(&self.ring[slot].texture, &source);
            if self.draw_cursor && self.pointer.visible {
                self.draw_pointer(slot);
            }
            if let Some(mt) = &self.lock {
                mt.Leave();
            }
        }
        self.next_slot = (slot + 1) % RING;
        self.latest = Some(slot);
        self.need_frame = false;
        Ok(Some(self.ring[slot].texture.clone()))
    }

    /// Takes the pointer's position and shape from the frame info. True when what's drawn changes.
    fn update_pointer(&mut self, dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) -> Result<bool, Failure> {
        let mut changed = false;
        if info.LastMouseUpdateTime != 0 {
            let visible = info.PointerPosition.Visible.as_bool();
            let p = info.PointerPosition.Position;
            if visible != self.pointer.visible || (visible && (p.x, p.y) != (self.pointer.x, self.pointer.y)) {
                changed = true;
            }
            self.pointer.visible = visible;
            if visible {
                self.pointer.x = p.x;
                self.pointer.y = p.y;
            }
        }
        if info.PointerShapeBufferSize > 0 {
            let size = info.PointerShapeBufferSize as usize;
            if self.pointer.buffer.len() < size {
                self.pointer.buffer.resize(size, 0);
            }
            let mut required = 0u32;
            let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            let got = unsafe {
                dup.GetFramePointerShape(size as u32, self.pointer.buffer.as_mut_ptr() as _, &mut required, &mut shape)
            };
            match got {
                Ok(()) => {
                    let bytes = &self.pointer.buffer[..size.min(required as usize)];
                    match pointer_images(shape.Type, shape.Width, shape.Height, shape.Pitch, bytes) {
                        Some(images) => match self.make_shape(&images) {
                            Ok(s) => self.pointer.shape = Some(s),
                            Err(e) => logi!("capture: pointer texture: {e}"),
                        },
                        None => logi!("capture: pointer shape type {} ({}×{}) not drawn", shape.Type, shape.Width, shape.Height),
                    }
                    changed = true;
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Err(Failure::Lost(format!("pointer shape: {e}"))),
                Err(e) => logi!("capture: pointer shape: {e}"),
            }
        }
        Ok(changed)
    }

    /// Draws the pointer into ring slot `slot`. Caller holds the device lock.
    unsafe fn draw_pointer(&self, slot: usize) {
        let Some(shape) = &self.pointer.shape else { return };
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (x, y) = (self.pointer.x as f32, self.pointer.y as f32);
        let rect = [
            x / w * 2.0 - 1.0,
            1.0 - y / h * 2.0,
            (x + shape.width as f32) / w * 2.0 - 1.0,
            1.0 - (y + shape.height as f32) / h * 2.0,
        ];
        let p = &self.pipeline;
        let ctx = &self.context;
        unsafe {
            ctx.UpdateSubresource(&p.rect, 0, None, rect.as_ptr() as _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(self.ring[slot].target.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w, Height: h, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetState(&p.raster);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&p.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(p.rect.clone())]));
            ctx.PSSetShader(&p.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(p.sampler.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(shape.normal.clone())]));
            ctx.OMSetBlendState(&p.blend_alpha, None, 0xFFFF_FFFF);
            ctx.Draw(4, 0);
            if let Some(invert) = &shape.invert {
                ctx.PSSetShaderResources(0, Some(&[Some(invert.clone())]));
                ctx.OMSetBlendState(&p.blend_invert, None, 0xFFFF_FFFF);
                ctx.Draw(4, 0);
            }
            // Leave nothing bound, so encoders can read the texture right away.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
        }
    }

    fn make_shape(&self, images: &PointerImages) -> windows::core::Result<PointerShape> {
        let normal = texture_view(&self.device, images.width, images.height, &images.normal)?;
        let invert = match &images.invert {
            Some(data) => Some(texture_view(&self.device, images.width, images.height, data)?),
            None => None,
        };
        Ok(PointerShape { width: images.width, height: images.height, normal, invert })
    }

    fn make_ring(&mut self, width: u32, height: u32) -> Result<(), Failure> {
        self.ring.clear();
        self.latest = None;
        self.next_slot = 0;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        for _ in 0..RING {
            let made = unsafe {
                let mut texture = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut texture)).and_then(|_| {
                    let texture = texture.unwrap();
                    let mut target = None;
                    self.device.CreateRenderTargetView(&texture, None, Some(&mut target)).map(|_| Slot { texture, target: target.unwrap() })
                })
            };
            match made {
                Ok(slot) => self.ring.push(slot),
                Err(e) => {
                    self.ring.clear();
                    return Err(self.classify(format!("frame texture {width}×{height}: {e}")));
                }
            }
        }
        if self.size != (width, height) {
            logi!("capture: screen is now {width}×{height}");
        }
        self.size = (width, height);
        Ok(())
    }

    /// (Re)opens the duplication of our monitor.
    fn open(&mut self) -> Result<(), Failure> {
        unsafe {
            let output = self.find_again().map_err(|e| self.classify(format!("monitor: {e}")))?;
            let dup = match output.cast::<IDXGIOutput5>() {
                Ok(o5) => match o5.DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) {
                    Ok(d) => d,
                    // E_ACCESSDENIED on the secure desktop fails both ways; anything else (an
                    // unaware DPI context, an old driver) may still work with the older call.
                    Err(e) if e.code() == windows::Win32::Foundation::E_ACCESSDENIED => {
                        return Err(Failure::Lost(format!("DuplicateOutput1: {e}")));
                    }
                    Err(e) => {
                        if self.failures_logged == 0 {
                            logi!("capture: DuplicateOutput1 failed ({e}); trying DuplicateOutput");
                        }
                        self.duplicate_old(&output)?
                    }
                },
                Err(_) => self.duplicate_old(&output)?,
            };
            let desc = dup.GetDesc();
            if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
                logi!("capture: the monitor is rotated ({}); the picture is sent unrotated", desc.Rotation.0);
            }
            let (w, h) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
            if self.ring.is_empty() || (w, h) != self.size {
                self.make_ring(w, h)?;
            }
            self.duplication = Some(dup);
            self.need_frame = true;
            // The shape is sent again with the first frame of a new duplication.
            self.pointer.shape = None;
            self.pointer.visible = false;
            if self.failing_since.is_some() || self.failures_logged > 0 {
                logi!("capture: screen back ({w}×{h})");
            }
            Ok(())
        }
    }

    unsafe fn duplicate_old(&self, output: &IDXGIOutput) -> Result<IDXGIOutputDuplication, Failure> {
        unsafe {
            let o1: IDXGIOutput1 = output.cast().map_err(|e| Failure::Fatal(format!("IDXGIOutput1: {e}")))?;
            o1.DuplicateOutput(&self.device).map_err(|e| {
                // Too many apps duplicating, or no duplication on this GPU at all.
                if e.code() == DXGI_ERROR_UNSUPPORTED {
                    Failure::Fatal(format!("Desktop Duplication isn't supported on this graphics card/driver ({e})"))
                } else if e.code() == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
                    Failure::Lost(format!("DuplicateOutput: too many apps capturing the screen ({e})"))
                } else {
                    self.classify(format!("DuplicateOutput: {e}"))
                }
            })
        }
    }

    /// Our monitor on our adapter: by name, else by its old index.
    unsafe fn find_again(&self) -> windows::core::Result<IDXGIOutput> {
        unsafe {
            let mut i = 0;
            while let Ok(output) = self.adapter.EnumOutputs(i) {
                if let Ok(desc) = output.GetDesc()
                    && desc.DeviceName == self.output_name
                {
                    return Ok(output);
                }
                i += 1;
            }
            self.adapter.EnumOutputs(self.output_local)
        }
    }

    /// A failure is fatal when the device itself is gone; otherwise it's worth opening again.
    fn classify(&self, message: String) -> Failure {
        match unsafe { self.device.GetDeviceRemovedReason() } {
            Ok(()) => Failure::Lost(message),
            Err(e) => Failure::Fatal(format!("graphics device removed ({e}); {message}")),
        }
    }

    fn failed(&mut self, failure: Failure, timeout_ms: u32) -> Result<Option<ID3D11Texture2D>, String> {
        let message = match failure {
            Failure::Fatal(m) => {
                logi!("capture: {m}");
                return Err(m);
            }
            Failure::Lost(m) => m,
        };
        let since = *self.failing_since.get_or_insert_with(Instant::now);
        if self.failures_logged < 3 {
            logi!("capture: screen unavailable ({message}); trying again");
        }
        self.failures_logged += 1;
        // Opening fails at once while the secure desktop is up: don't spin.
        std::thread::sleep(Duration::from_millis(timeout_ms.clamp(5, 200) as u64));
        if since.elapsed() > GIVE_UP_AFTER {
            return Err(format!("screen unavailable for {} s: {message}", since.elapsed().as_secs()));
        }
        Ok(None)
    }

    fn recovered(&mut self) {
        self.failing_since = None;
        self.failures_logged = 0;
    }

    fn raise_thread_priority(&mut self) {
        let id = unsafe { GetCurrentThreadId() };
        if id == self.priority_thread {
            return;
        }
        self.priority_thread = id;
        if let Err(e) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) } {
            logi!("capture: thread priority not raised ({e})");
        }
    }
}

impl PointerPipeline {
    fn new(device: &ID3D11Device) -> windows::core::Result<PointerPipeline> {
        unsafe {
            let vs_code = compile(s!("vs_main"), s!("vs_4_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_4_0"))?;
            let mut vs = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            // Colour: over the screen by alpha. Invert: src·(1−dst) + dst·(1−src), which is XOR for
            // black and white. Both keep the screen's alpha.
            let blend = |src: D3D11_BLEND, dst: D3D11_BLEND| -> windows::core::Result<ID3D11BlendState> {
                let mut desc = D3D11_BLEND_DESC::default();
                desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: true.into(),
                    SrcBlend: src,
                    DestBlend: dst,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ZERO,
                    DestBlendAlpha: D3D11_BLEND_ONE,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                let mut state = None;
                device.CreateBlendState(&desc, Some(&mut state))?;
                Ok(state.unwrap())
            };
            let blend_alpha = blend(D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA)?;
            let blend_invert = blend(D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_INV_SRC_COLOR)?;
            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut rect = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut rect))?;
            Ok(PointerPipeline {
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                sampler: sampler.unwrap(),
                blend_alpha,
                blend_invert,
                raster: raster.unwrap(),
                rect: rect.unwrap(),
            })
        }
    }
}

fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(SHADER.as_ptr() as _, SHADER.len(), s!("bb1-pointer"), None, None, entry, target, 0, 0, &mut code, Some(&mut errors));
        if let Err(e) = r {
            if let Some(err) = errors {
                let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
                logi!("capture: shader didn't compile: {}", String::from_utf8_lossy(msg));
            }
            return Err(e);
        }
        let code = code.unwrap();
        Ok(std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()).to_vec())
    }
}

/// A small immutable BGRA texture (a pointer image) and its shader view.
fn texture_view(device: &ID3D11Device, width: u32, height: u32, bgra: &[u8]) -> windows::core::Result<ID3D11ShaderResourceView> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: bgra.as_ptr() as _, SysMemPitch: width * 4, SysMemSlicePitch: 0 };
    unsafe {
        let mut texture = None;
        device.CreateTexture2D(&desc, Some(&data), Some(&mut texture))?;
        let mut view = None;
        device.CreateShaderResourceView(&texture.unwrap(), None, Some(&mut view))?;
        Ok(view.unwrap())
    }
}

fn create_device(adapter: &IDXGIAdapter1) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let create = |flags: D3D11_CREATE_DEVICE_FLAG| -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
        let mut device = None;
        let mut context = None;
        unsafe {
            // With an explicit adapter the driver type must be UNKNOWN.
            D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), flags, Some(&levels), D3D11_SDK_VERSION,
                              Some(&mut device), None, Some(&mut context))?;
        }
        Ok((device.unwrap(), context.unwrap()))
    };
    match create(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT) {
        Ok(r) => Ok(r),
        Err(e) => {
            logi!("capture: no Direct3D 11 video support ({e}); scaling on the GPU won't be available");
            create(D3D11_CREATE_DEVICE_BGRA_SUPPORT)
        }
    }
}

struct FoundOutput {
    adapter: IDXGIAdapter1,
    local: u32,
    desc: DXGI_OUTPUT_DESC,
}

/// Monitor `index` across all adapters: the primary one (the monitor at 0,0) first, then the
/// others in DXGI order.
fn find_output(factory: &IDXGIFactory1, index: usize) -> Result<FoundOutput, String> {
    let mut all = Vec::new();
    unsafe {
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(desc) = output.GetDesc()
                    && desc.AttachedToDesktop.as_bool()
                {
                    all.push(FoundOutput { adapter: adapter.clone(), local: o, desc });
                }
                o += 1;
            }
            a += 1;
        }
    }
    if all.is_empty() {
        return Err("no monitor is attached to a graphics card (a remote desktop session, or no display driver)".into());
    }
    if let Some(p) = all.iter().position(|f| f.desc.DesktopCoordinates.left == 0 && f.desc.DesktopCoordinates.top == 0) {
        let primary = all.remove(p);
        all.insert(0, primary);
    }
    let count = all.len();
    if index >= count {
        return Err(format!("there's no monitor {index} ({count} attached)"));
    }
    Ok(all.swap_remove(index))
}

/// Raises the GPU priority of our device and process. Logs what worked; never fails.
fn raise_gpu_priority(device: &ID3D11Device, vendor: u32) {
    unsafe {
        match device.cast::<IDXGIDevice>().and_then(|d| d.SetGPUThreadPriority(7)) {
            Ok(()) => logi!("capture: GPU thread priority 7"),
            Err(e) => logi!("capture: GPU thread priority not raised ({e})"),
        }
        type SetClass = unsafe extern "system" fn(HANDLE, i32) -> i32;
        const HIGH: i32 = 4; // D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH
        const REALTIME: i32 = 5;
        let Ok(gdi) = LoadLibraryW(w!("gdi32.dll")) else { return };
        let Some(f) = GetProcAddress(gdi, s!("D3DKMTSetProcessSchedulingPriorityClass")) else {
            logi!("capture: no D3DKMTSetProcessSchedulingPriorityClass on this Windows");
            return;
        };
        let set: SetClass = std::mem::transmute(f);
        let nvidia = vendor == 0x10DE;
        let elevated = is_elevated();
        let mut tries = Vec::new();
        if elevated && !nvidia {
            tries.push((REALTIME, "realtime"));
        }
        tries.push((HIGH, "high"));
        for (class, name) in tries {
            let status = set(GetCurrentProcess(), class);
            if status >= 0 {
                logi!("capture: GPU scheduling class {name}{}", if nvidia && elevated { " (not realtime: NVIDIA drivers can stall with it)" } else { "" });
                return;
            }
            logi!("capture: GPU scheduling class {name} refused (NTSTATUS {status:#010x})");
        }
    }
}

fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// A pointer shape as BGRA images: `normal` (straight alpha, blended over the screen) and, when the
/// shape inverts anything, `invert` (white where the screen is inverted, black elsewhere).
pub(crate) struct PointerImages {
    pub width: u32,
    pub height: u32,
    pub normal: Vec<u8>,
    pub invert: Option<Vec<u8>>,
}

const SHAPE_MONOCHROME: u32 = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
const SHAPE_COLOR: u32 = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32;
const SHAPE_MASKED_COLOR: u32 = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32;

/// Converts a DXGI pointer shape. None for an unknown type or a buffer too small for its size.
///
/// - COLOR: 32-bit BGRA with alpha, drawn as it is.
/// - MASKED_COLOR: alpha 0 = this colour replaces the screen; alpha 0xFF = XOR the colour with it.
/// - MONOCHROME: 1 bit per pixel, an AND mask (top half) over an XOR mask (bottom half):
///   AND 0 XOR 0 black, AND 0 XOR 1 white, AND 1 XOR 0 transparent, AND 1 XOR 1 invert.
pub(crate) fn pointer_images(kind: u32, width: u32, height: u32, pitch: u32, buf: &[u8]) -> Option<PointerImages> {
    let (w, p) = (width as usize, pitch as usize);
    let h = if kind == SHAPE_MONOCHROME { height as usize / 2 } else { height as usize };
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }
    let mut normal = vec![0u8; w * h * 4];
    let mut invert = vec![0u8; w * h * 4];
    let mut inverts = false;
    match kind {
        SHAPE_COLOR | SHAPE_MASKED_COLOR => {
            if p < w * 4 || buf.len() < p * (h - 1) + w * 4 {
                return None;
            }
            for y in 0..h {
                for x in 0..w {
                    let s = &buf[y * p + x * 4..y * p + x * 4 + 4];
                    let d = (y * w + x) * 4;
                    if kind == SHAPE_COLOR {
                        normal[d..d + 4].copy_from_slice(s);
                    } else if s[3] == 0 {
                        normal[d..d + 4].copy_from_slice(&[s[0], s[1], s[2], 0xFF]);
                    } else {
                        invert[d..d + 4].copy_from_slice(&[s[0], s[1], s[2], 0xFF]);
                        inverts |= s[0] | s[1] | s[2] != 0;
                    }
                }
            }
        }
        SHAPE_MONOCHROME => {
            if p * 8 < w || buf.len() < p * h * 2 {
                return None;
            }
            let bit = |row: usize, x: usize| buf[row * p + x / 8] >> (7 - x % 8) & 1 != 0;
            for y in 0..h {
                for x in 0..w {
                    let d = (y * w + x) * 4;
                    match (bit(y, x), bit(y + h, x)) {
                        (false, false) => normal[d..d + 4].copy_from_slice(&[0, 0, 0, 0xFF]),
                        (false, true) => normal[d..d + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]),
                        (true, false) => {}
                        (true, true) => {
                            invert[d..d + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
                            inverts = true;
                        }
                    }
                }
            }
        }
        _ => return None,
    }
    Some(PointerImages { width: w as u32, height: h as u32, normal, invert: inverts.then_some(invert) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_pointer() {
        // 8×2 pixels, pitch 1: AND rows then XOR rows. Pixels 0..3 of row 0: the four combinations.
        let and = [0b0011_1111u8, 0xFF];
        let xor = [0b0101_0000u8, 0x00];
        let buf = [and[0], and[1], xor[0], xor[1]];
        let img = pointer_images(SHAPE_MONOCHROME, 8, 4, 1, &buf).unwrap();
        assert_eq!((img.width, img.height), (8, 2));
        let px = |v: &Vec<u8>, i: usize| [v[i * 4], v[i * 4 + 1], v[i * 4 + 2], v[i * 4 + 3]];
        assert_eq!(px(&img.normal, 0), [0, 0, 0, 0xFF]); // AND 0 XOR 0: black
        assert_eq!(px(&img.normal, 1), [0xFF; 4]); // AND 0 XOR 1: white
        assert_eq!(px(&img.normal, 2), [0; 4]); // AND 1 XOR 0: transparent
        let inv = img.invert.unwrap();
        assert_eq!(px(&img.normal, 3), [0; 4]); // AND 1 XOR 1: invert
        assert_eq!(px(&inv, 3), [0xFF; 4]);
        assert_eq!(px(&inv, 2), [0; 4]);
        assert_eq!(px(&img.normal, 8), [0; 4]); // row 1 all transparent
    }

    #[test]
    fn masked_and_color_pointer() {
        // 2×1, pitch 12 (padding).
        let mut buf = vec![0u8; 12];
        buf[0..4].copy_from_slice(&[10, 20, 30, 0]); // replace
        buf[4..8].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // XOR white
        let img = pointer_images(SHAPE_MASKED_COLOR, 2, 1, 12, &buf).unwrap();
        assert_eq!(&img.normal[0..4], &[10, 20, 30, 0xFF]);
        assert_eq!(&img.normal[4..8], &[0, 0, 0, 0]);
        assert_eq!(&img.invert.as_ref().unwrap()[4..8], &[0xFF; 4]);
        let img = pointer_images(SHAPE_COLOR, 2, 1, 12, &buf).unwrap();
        assert_eq!(&img.normal[0..8], &buf[0..8]);
        assert!(img.invert.is_none());
        assert!(pointer_images(SHAPE_COLOR, 4, 1, 12, &buf).is_none()); // pitch too small
        assert!(pointer_images(99, 2, 1, 12, &buf).is_none());
    }
}
