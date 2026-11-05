//! NVIDIA hardware encoder (NVENC), driven directly through `nvEncodeAPI64.dll` (part of the NVIDIA
//! driver) with D3D11 textures as input, so the captured picture goes from the GPU's memory to its
//! encoder block without passing through the CPU. The host makes one `Encoder` per viewer, each with
//! its own size, frame rate and bitrate (docs/08-gaming-host.md).
//!
//! Tuned for streaming games, where latency matters most: preset P1 with NVIDIA's ultra-low-latency
//! tuning, constant bitrate with a buffer of about one frame, no B-frames, no lookahead, and an
//! endless GOP (a keyframe only when one is asked for). `encode` is synchronous and returns the
//! frame it was given.
//!
//! NVENC API 12.0 (bindings in `nvenc_sys.rs`): needs NVIDIA driver 522.25 or newer.
//!
//! Input: a BGRA (or RGBA) texture on the device passed to `Encoder::new`. NVENC converts it to
//! YUV 4:2:0 itself, signalled as BT.709 video range (what bb1's clients expect). When the texture's
//! size differs from the configured size, the D3D11 video processor scales it on the GPU first
//! (stretching; keep the aspect ratio when choosing a size).
//!
//! The Annex-B splitting and classification at the top is plain Rust, tested on every platform;
//! the encoder itself is Windows-only.

use super::nvenc_sys as sys;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Codec {
    H264,
    Hevc,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
}

/// One encoded picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EncodedFrame {
    /// An IDR picture: a decoder can start here.
    pub keyframe: bool,
    /// SPS, PPS (HEVC: VPS, SPS, PPS) without start codes, in stream order. Present on keyframes.
    pub parameter_sets: Vec<Vec<u8>>,
    /// The picture's NAL units without start codes; no parameter sets, access unit delimiters, SEI
    /// or filler data.
    pub nals: Vec<Vec<u8>>,
}

// ---------------------------------------------------------------------------------------------
// Versions and GUIDs. nvEncodeAPI.h defines these with function-like macros and `static const GUID`,
// which bindgen doesn't carry over, so they are written out here from the header (SDK 12.0).

/// `NVENCAPI_VERSION`: major | minor << 24.
pub const API_VERSION: u32 = sys::NVENCAPI_MAJOR_VERSION | (sys::NVENCAPI_MINOR_VERSION << 24);

/// `NVENCAPI_STRUCT_VERSION(ver)`.
const fn struct_version(ver: u32) -> u32 {
    API_VERSION | (ver << 16) | (0x7 << 28)
}
/// The `| (1u << 31)` the header adds to some structs' versions.
const EXT: u32 = 1 << 31;

pub const NV_ENC_CONFIG_VER: u32 = struct_version(8) | EXT;
pub const NV_ENC_INITIALIZE_PARAMS_VER: u32 = struct_version(5) | EXT;
pub const NV_ENC_RECONFIGURE_PARAMS_VER: u32 = struct_version(1) | EXT;
pub const NV_ENC_PRESET_CONFIG_VER: u32 = struct_version(4) | EXT;
pub const NV_ENC_PIC_PARAMS_VER: u32 = struct_version(6) | EXT;
pub const NV_ENC_LOCK_BITSTREAM_VER: u32 = struct_version(2);
pub const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
pub const NV_ENC_REGISTER_RESOURCE_VER: u32 = struct_version(4);
pub const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = struct_version(2);
pub const NV_ENC_RC_PARAMS_VER: u32 = struct_version(1);

/// What `NvEncodeAPIGetMaxSupportedVersion` must report at least: major << 4 | minor.
pub const MIN_DRIVER_API: u32 = (sys::NVENCAPI_MAJOR_VERSION << 4) | sys::NVENCAPI_MINOR_VERSION;

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> sys::GUID {
    sys::GUID { Data1: d1, Data2: d2, Data3: d3, Data4: d4 }
}

pub const NV_ENC_CODEC_H264_GUID: sys::GUID =
    guid(0x6bc82762, 0x4e63, 0x4ca4, [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf]);
pub const NV_ENC_CODEC_HEVC_GUID: sys::GUID =
    guid(0x790cdc88, 0x4522, 0x4d7b, [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03]);

pub const NV_ENC_CODEC_PROFILE_AUTOSELECT_GUID: sys::GUID =
    guid(0xbfd6f8e7, 0x233c, 0x4341, [0x8b, 0x3e, 0x48, 0x18, 0x52, 0x38, 0x03, 0xf4]);
pub const NV_ENC_H264_PROFILE_BASELINE_GUID: sys::GUID =
    guid(0x0727bcaa, 0x78c4, 0x4c83, [0x8c, 0x2f, 0xef, 0x3d, 0xff, 0x26, 0x7c, 0x6a]);
pub const NV_ENC_H264_PROFILE_MAIN_GUID: sys::GUID =
    guid(0x60b5c1d4, 0x67fe, 0x4790, [0x94, 0xd5, 0xc4, 0x72, 0x6d, 0x7b, 0x6e, 0x6d]);
pub const NV_ENC_H264_PROFILE_HIGH_GUID: sys::GUID =
    guid(0xe7cbc309, 0x4f7a, 0x4b89, [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10]);
pub const NV_ENC_HEVC_PROFILE_MAIN_GUID: sys::GUID =
    guid(0xb514c39a, 0xb55b, 0x40fa, [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec]);

/// Presets P1 (fastest) to P7 (best quality).
pub const NV_ENC_PRESET_P1_GUID: sys::GUID =
    guid(0xfc0a8d3e, 0x45f8, 0x4cf8, [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf]);
pub const NV_ENC_PRESET_P2_GUID: sys::GUID =
    guid(0xf581cfb8, 0x88d6, 0x4381, [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab]);
pub const NV_ENC_PRESET_P3_GUID: sys::GUID =
    guid(0x36850110, 0x3a07, 0x441f, [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6]);
pub const NV_ENC_PRESET_P4_GUID: sys::GUID =
    guid(0x90a7b826, 0xdf06, 0x4862, [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81]);
pub const NV_ENC_PRESET_P5_GUID: sys::GUID =
    guid(0x21c6e6b4, 0x297a, 0x4cba, [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3]);
pub const NV_ENC_PRESET_P6_GUID: sys::GUID =
    guid(0x8e75c279, 0x6299, 0x4ab6, [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5]);
pub const NV_ENC_PRESET_P7_GUID: sys::GUID =
    guid(0x84848c12, 0x6f71, 0x4c13, [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74]);

/// The preset every encoder uses: the fastest, so encoding keeps up even while a game loads the GPU.
pub const PRESET: sys::GUID = NV_ENC_PRESET_P1_GUID;
pub const TUNING: sys::NV_ENC_TUNING_INFO = sys::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;

/// The NVENCSTATUS constant's name, for error messages.
pub fn status_name(status: sys::NVENCSTATUS) -> &'static str {
    macro_rules! names {
        ($($n:ident),* $(,)?) => {
            match status {
                $(sys::$n => stringify!($n),)*
                _ => "unknown NVENCSTATUS",
            }
        };
    }
    names!(
        NV_ENC_SUCCESS,
        NV_ENC_ERR_NO_ENCODE_DEVICE,
        NV_ENC_ERR_UNSUPPORTED_DEVICE,
        NV_ENC_ERR_INVALID_ENCODERDEVICE,
        NV_ENC_ERR_INVALID_DEVICE,
        NV_ENC_ERR_DEVICE_NOT_EXIST,
        NV_ENC_ERR_INVALID_PTR,
        NV_ENC_ERR_INVALID_EVENT,
        NV_ENC_ERR_INVALID_PARAM,
        NV_ENC_ERR_INVALID_CALL,
        NV_ENC_ERR_OUT_OF_MEMORY,
        NV_ENC_ERR_ENCODER_NOT_INITIALIZED,
        NV_ENC_ERR_UNSUPPORTED_PARAM,
        NV_ENC_ERR_LOCK_BUSY,
        NV_ENC_ERR_NOT_ENOUGH_BUFFER,
        NV_ENC_ERR_INVALID_VERSION,
        NV_ENC_ERR_MAP_FAILED,
        NV_ENC_ERR_NEED_MORE_INPUT,
        NV_ENC_ERR_ENCODER_BUSY,
        NV_ENC_ERR_EVENT_NOT_REGISTERD,
        NV_ENC_ERR_GENERIC,
        NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY,
        NV_ENC_ERR_UNIMPLEMENTED,
        NV_ENC_ERR_RESOURCE_REGISTER_FAILED,
        NV_ENC_ERR_RESOURCE_NOT_REGISTERED,
        NV_ENC_ERR_RESOURCE_NOT_MAPPED,
    )
}

/// Checks a configuration before anything touches the GPU.
pub fn validate(cfg: &EncoderConfig) -> Result<(), String> {
    let max = match cfg.codec {
        Codec::H264 => 4096,
        Codec::Hevc => 8192,
    };
    if cfg.width < 64 || cfg.height < 64 || cfg.width > max || cfg.height > max {
        return Err(format!("NVENC: {}x{} is outside 64..{max} for {:?}", cfg.width, cfg.height, cfg.codec));
    }
    if !cfg.width.is_multiple_of(2) || !cfg.height.is_multiple_of(2) {
        return Err(format!("NVENC: {}x{} must be even (4:2:0)", cfg.width, cfg.height));
    }
    if cfg.fps == 0 || cfg.fps > 240 {
        return Err(format!("NVENC: frame rate {} is outside 1..240", cfg.fps));
    }
    if cfg.bitrate_kbps == 0 {
        return Err("NVENC: bitrate must be above 0".into());
    }
    Ok(())
}

/// Bits per second for a kbps setting (saturating; NVENC takes a u32).
pub fn bits_per_second(kbps: u32) -> u32 {
    kbps.saturating_mul(1000)
}

/// VBV (rate control buffer) size in bits: one frame's worth, so no frame can be much bigger than
/// the average and the network never sees a burst it has to queue.
pub fn vbv_bits(kbps: u32, fps: u32) -> u32 {
    (bits_per_second(kbps) / fps.max(1)).max(1)
}

// ---------------------------------------------------------------------------------------------
// Annex-B parsing.

/// Splits an Annex-B byte stream (NAL units after 00 00 01 or 00 00 00 01) into NAL units without
/// their start codes. Bytes before the first start code and trailing zero bytes are dropped.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    fn push<'a>(out: &mut Vec<&'a [u8]>, mut nal: &'a [u8]) {
        while let [rest @ .., 0] = nal {
            nal = rest;
        }
        if !nal.is_empty() {
            out.push(nal);
        }
    }
    let mut out = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i + 2] > 1 {
            i += 3; // can't be part of a start code ending at i+2 or before
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                push(&mut out, &data[s..i]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push(&mut out, &data[s..]);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NalKind {
    /// SPS, PPS (and VPS for HEVC).
    ParameterSet,
    /// A slice of an IDR (HEVC: any random-access) picture.
    KeySlice,
    /// Anything else the decoder needs (other slices, end of sequence…).
    Other,
    /// Access unit delimiter, SEI, filler data: not sent.
    Drop,
}

/// The NAL unit's type field.
pub fn nal_type(codec: Codec, nal: &[u8]) -> u8 {
    match (codec, nal.first()) {
        (_, None) => 0xff,
        (Codec::H264, Some(b)) => b & 0x1f,
        (Codec::Hevc, Some(b)) => (b >> 1) & 0x3f,
    }
}

pub fn classify(codec: Codec, nal: &[u8]) -> NalKind {
    let t = nal_type(codec, nal);
    match codec {
        Codec::H264 => match t {
            7 | 8 => NalKind::ParameterSet,
            5 => NalKind::KeySlice,
            6 | 9 | 12 => NalKind::Drop,
            _ => NalKind::Other,
        },
        Codec::Hevc => match t {
            32..=34 => NalKind::ParameterSet,
            16..=21 => NalKind::KeySlice,
            35 | 38 | 39 | 40 => NalKind::Drop,
            _ => NalKind::Other,
        },
    }
}

/// One encoded access unit (Annex-B, as NVENC writes it) as an `EncodedFrame`.
pub fn parse_access_unit(codec: Codec, data: &[u8]) -> EncodedFrame {
    let mut frame = EncodedFrame::default();
    for nal in split_annex_b(data) {
        match classify(codec, nal) {
            NalKind::ParameterSet => frame.parameter_sets.push(nal.to_vec()),
            NalKind::KeySlice => {
                frame.keyframe = true;
                frame.nals.push(nal.to_vec());
            }
            NalKind::Other => frame.nals.push(nal.to_vec()),
            NalKind::Drop => {}
        }
    }
    frame
}

#[cfg(windows)]
pub use win::Encoder;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::{CStr, c_void};
    use std::mem::{ManuallyDrop, zeroed};
    use std::ptr::null_mut;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::core::{Interface, s, w};

    /// How many input textures stay registered with NVENC at once. Capture reuses a few textures;
    /// the oldest registration goes when a new texture shows up.
    const MAX_REGISTERED: usize = 8;

    /// The driver's function table, loaded once per process (the DLL stays loaded).
    struct Api(sys::NV_ENCODE_API_FUNCTION_LIST);
    // Plain function pointers (plus reserved null pointers); safe to share between threads.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    static API: OnceLock<Result<Api, String>> = OnceLock::new();

    fn api() -> Result<&'static Api, String> {
        API.get_or_init(|| unsafe { load() }).as_ref().map_err(Clone::clone)
    }

    /// Calls a function from the table. `load` checked every function used here is present.
    macro_rules! nv {
        ($api:expr, $f:ident($($arg:expr),* $(,)?)) => {
            unsafe { ($api.0.$f.expect(stringify!($f)))($($arg),*) }
        };
    }

    unsafe fn load() -> Result<Api, String> {
        let lib = unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| format!("NVENC: nvEncodeAPI64.dll didn't load ({e}); this needs an NVIDIA GPU with its driver installed"))?;
        let get_max = unsafe { GetProcAddress(lib, s!("NvEncodeAPIGetMaxSupportedVersion")) }
            .ok_or("NVENC: NvEncodeAPIGetMaxSupportedVersion missing from nvEncodeAPI64.dll; update the NVIDIA driver")?;
        let create = unsafe { GetProcAddress(lib, s!("NvEncodeAPICreateInstance")) }
            .ok_or("NVENC: NvEncodeAPICreateInstance missing from nvEncodeAPI64.dll; update the NVIDIA driver")?;
        let get_max: unsafe extern "system" fn(*mut u32) -> sys::NVENCSTATUS = unsafe { std::mem::transmute(get_max) };
        let create: unsafe extern "system" fn(*mut sys::NV_ENCODE_API_FUNCTION_LIST) -> sys::NVENCSTATUS =
            unsafe { std::mem::transmute(create) };

        let mut max = 0u32;
        let st = unsafe { get_max(&mut max) };
        if st != sys::NV_ENC_SUCCESS {
            return Err(format!("NVENC: NvEncodeAPIGetMaxSupportedVersion: {}", status_name(st)));
        }
        if max < MIN_DRIVER_API {
            return Err(format!(
                "NVENC: the NVIDIA driver supports encoder API {}.{}, bb1 needs {}.{}: update the NVIDIA driver (522.25 or newer)",
                max >> 4,
                max & 0xf,
                sys::NVENCAPI_MAJOR_VERSION,
                sys::NVENCAPI_MINOR_VERSION
            ));
        }

        let mut fns: sys::NV_ENCODE_API_FUNCTION_LIST = unsafe { zeroed() };
        fns.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        let st = unsafe { create(&mut fns) };
        if st != sys::NV_ENC_SUCCESS {
            return Err(format!("NVENC: NvEncodeAPICreateInstance: {}", status_name(st)));
        }
        let required = [
            ("nvEncOpenEncodeSessionEx", fns.nvEncOpenEncodeSessionEx.is_some()),
            ("nvEncGetEncodeGUIDCount", fns.nvEncGetEncodeGUIDCount.is_some()),
            ("nvEncGetEncodeGUIDs", fns.nvEncGetEncodeGUIDs.is_some()),
            ("nvEncGetEncodePresetConfigEx", fns.nvEncGetEncodePresetConfigEx.is_some()),
            ("nvEncInitializeEncoder", fns.nvEncInitializeEncoder.is_some()),
            ("nvEncReconfigureEncoder", fns.nvEncReconfigureEncoder.is_some()),
            ("nvEncCreateBitstreamBuffer", fns.nvEncCreateBitstreamBuffer.is_some()),
            ("nvEncDestroyBitstreamBuffer", fns.nvEncDestroyBitstreamBuffer.is_some()),
            ("nvEncRegisterResource", fns.nvEncRegisterResource.is_some()),
            ("nvEncUnregisterResource", fns.nvEncUnregisterResource.is_some()),
            ("nvEncMapInputResource", fns.nvEncMapInputResource.is_some()),
            ("nvEncUnmapInputResource", fns.nvEncUnmapInputResource.is_some()),
            ("nvEncEncodePicture", fns.nvEncEncodePicture.is_some()),
            ("nvEncLockBitstream", fns.nvEncLockBitstream.is_some()),
            ("nvEncUnlockBitstream", fns.nvEncUnlockBitstream.is_some()),
            ("nvEncDestroyEncoder", fns.nvEncDestroyEncoder.is_some()),
        ];
        if let Some((name, _)) = required.iter().find(|(_, present)| !present) {
            return Err(format!("NVENC: the driver doesn't provide {name}; update the NVIDIA driver"));
        }
        Ok(Api(fns))
    }

    /// "NVENC <what>: <status name> (<driver's explanation>)".
    fn error(api: &Api, enc: *mut c_void, what: &str, st: sys::NVENCSTATUS) -> String {
        let mut msg = format!("NVENC {what}: {}", status_name(st));
        if !enc.is_null()
            && let Some(last) = api.0.nvEncGetLastErrorString
        {
            let p = unsafe { last(enc) };
            if !p.is_null() {
                let detail = unsafe { CStr::from_ptr(p) }.to_string_lossy();
                if !detail.trim().is_empty() {
                    msg.push_str(&format!(" ({})", detail.trim()));
                }
            }
        }
        msg
    }

    fn check(api: &Api, enc: *mut c_void, what: &str, st: sys::NVENCSTATUS) -> Result<(), String> {
        if st == sys::NV_ENC_SUCCESS { Ok(()) } else { Err(error(api, enc, what, st)) }
    }

    fn open_session(api: &Api, device: &ID3D11Device) -> Result<*mut c_void, String> {
        let mut p: sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { zeroed() };
        p.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        p.deviceType = sys::NV_ENC_DEVICE_TYPE_DIRECTX;
        p.device = device.as_raw();
        p.apiVersion = API_VERSION;
        let mut enc = null_mut();
        let st = nv!(api, nvEncOpenEncodeSessionEx(&mut p, &mut enc));
        if st != sys::NV_ENC_SUCCESS {
            // The header: "If the creation of encoder session fails, the client must call
            // NvEncDestroyEncoder before exiting."
            if !enc.is_null() {
                nv!(api, nvEncDestroyEncoder(enc));
            }
            let hint = match st {
                sys::NV_ENC_ERR_OUT_OF_MEMORY | sys::NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY => {
                    "; too many encoders open at once? (GeForce cards limit how many run together)"
                }
                sys::NV_ENC_ERR_UNSUPPORTED_DEVICE | sys::NV_ENC_ERR_NO_ENCODE_DEVICE => {
                    "; this device isn't an NVIDIA GPU with an encoder"
                }
                sys::NV_ENC_ERR_INVALID_VERSION => "; update the NVIDIA driver (522.25 or newer)",
                _ => "",
            };
            return Err(format!("NVENC open session: {}{hint}", status_name(st)));
        }
        Ok(enc)
    }

    /// Lets the capture thread and encoders share the device: every immediate-context call takes the
    /// device's lock. Harmless if already on.
    fn protect(device: &ID3D11Device) {
        unsafe {
            if let Ok(ctx) = device.GetImmediateContext()
                && let Ok(mt) = ctx.cast::<ID3D11Multithread>()
            {
                let _ = mt.SetMultithreadProtected(true);
            }
        }
    }

    struct Registration {
        /// Held so the texture (and so its address, the cache key) stays alive while registered.
        texture: ID3D11Texture2D,
        handle: sys::NV_ENC_REGISTERED_PTR,
    }

    pub struct Encoder {
        api: &'static Api,
        enc: *mut c_void,
        cfg: EncoderConfig,
        device: ID3D11Device,
        /// `init.encodeConfig` points here (boxed, so the address stays put).
        nv_config: Box<sys::NV_ENC_CONFIG>,
        init: Box<sys::NV_ENC_INITIALIZE_PARAMS>,
        initialized: bool,
        bitstream: sys::NV_ENC_OUTPUT_PTR,
        registered: Vec<Registration>,
        scaler: Option<Scaler>,
        frame_idx: u32,
    }

    // The session and textures are only touched through &mut self, one thread at a time; NVENC
    // sessions and D3D11 devices may move between threads.
    unsafe impl Send for Encoder {}

    impl Encoder {
        /// True when the NVIDIA driver loads and an encode session opens on an NVIDIA GPU. A yes is
        /// remembered; a no is checked again next time (all sessions may just have been busy).
        pub fn is_available() -> bool {
            static YES: AtomicBool = AtomicBool::new(false);
            if YES.load(Ordering::Relaxed) {
                return true;
            }
            let ok = probe().is_ok();
            if ok {
                YES.store(true, Ordering::Relaxed);
            }
            ok
        }

        pub fn new(device: &ID3D11Device, cfg: EncoderConfig) -> Result<Encoder, String> {
            validate(&cfg)?;
            let api = api()?;
            protect(device);
            let enc = open_session(api, device)?;
            // From here on Drop cleans up whatever was made.
            let mut e = Encoder {
                api,
                enc,
                cfg,
                device: device.clone(),
                nv_config: Box::new(unsafe { zeroed() }),
                init: Box::new(unsafe { zeroed() }),
                initialized: false,
                bitstream: null_mut(),
                registered: Vec::new(),
                scaler: None,
                frame_idx: 0,
            };
            e.check_codec()?;
            e.initialize()?;
            let mut buf: sys::NV_ENC_CREATE_BITSTREAM_BUFFER = unsafe { zeroed() };
            buf.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            let st = nv!(api, nvEncCreateBitstreamBuffer(e.enc, &mut buf));
            check(api, e.enc, "create bitstream buffer", st)?;
            e.bitstream = buf.bitstreamBuffer;
            Ok(e)
        }

        pub fn config(&self) -> &EncoderConfig {
            &self.cfg
        }

        fn codec_guid(&self) -> sys::GUID {
            match self.cfg.codec {
                Codec::H264 => NV_ENC_CODEC_H264_GUID,
                Codec::Hevc => NV_ENC_CODEC_HEVC_GUID,
            }
        }

        fn check_codec(&self) -> Result<(), String> {
            let mut count = 0u32;
            let st = nv!(self.api, nvEncGetEncodeGUIDCount(self.enc, &mut count));
            check(self.api, self.enc, "codec count", st)?;
            let mut guids = vec![sys::GUID::default(); count as usize];
            let mut got = 0u32;
            let st = nv!(self.api, nvEncGetEncodeGUIDs(self.enc, guids.as_mut_ptr(), count, &mut got));
            check(self.api, self.enc, "codec list", st)?;
            guids.truncate(got as usize);
            if guids.contains(&self.codec_guid()) {
                Ok(())
            } else {
                Err(format!("NVENC: this GPU can't encode {:?}", self.cfg.codec))
            }
        }

        /// Preset P1 + ultra-low-latency tuning, then bb1's changes on top.
        fn initialize(&mut self) -> Result<(), String> {
            let api = self.api;
            let cfg = self.cfg.clone();
            let mut preset: Box<sys::NV_ENC_PRESET_CONFIG> = Box::new(unsafe { zeroed() });
            preset.version = NV_ENC_PRESET_CONFIG_VER;
            preset.presetCfg.version = NV_ENC_CONFIG_VER;
            let st = nv!(api, nvEncGetEncodePresetConfigEx(self.enc, self.codec_guid(), PRESET, TUNING, &mut *preset));
            check(api, self.enc, "preset", st)?;

            let c = &mut *self.nv_config;
            *c = preset.presetCfg;
            c.version = NV_ENC_CONFIG_VER;
            c.profileGUID = match cfg.codec {
                Codec::H264 => NV_ENC_H264_PROFILE_HIGH_GUID,
                Codec::Hevc => NV_ENC_HEVC_PROFILE_MAIN_GUID,
            };
            c.gopLength = sys::NVENC_INFINITE_GOPLENGTH;
            c.frameIntervalP = 1; // I and P frames only
            c.frameFieldMode = sys::NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;

            let rc = &mut c.rcParams;
            rc.version = NV_ENC_RC_PARAMS_VER;
            rc.rateControlMode = sys::NV_ENC_PARAMS_RC_CBR;
            rc.averageBitRate = bits_per_second(cfg.bitrate_kbps);
            rc.maxBitRate = bits_per_second(cfg.bitrate_kbps);
            rc.vbvBufferSize = vbv_bits(cfg.bitrate_kbps, cfg.fps);
            rc.vbvInitialDelay = rc.vbvBufferSize;
            rc.set_enableLookahead(0);
            rc.lookaheadDepth = 0;
            rc.set_zeroReorderDelay(1);
            rc.set_disableBadapt(1);
            rc.set_strictGOPTarget(0);

            // The client converts with BT.709 video range; NVENC uses the VUI's matrix and range for
            // its RGB -> YUV conversion and writes them into the SPS.
            let vui = sys::NV_ENC_CONFIG_H264_VUI_PARAMETERS {
                videoSignalTypePresentFlag: 1,
                videoFormat: sys::NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED,
                videoFullRangeFlag: 0,
                colourDescriptionPresentFlag: 1,
                colourPrimaries: sys::NV_ENC_VUI_COLOR_PRIMARIES_BT709,
                transferCharacteristics: sys::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709,
                colourMatrix: sys::NV_ENC_VUI_MATRIX_COEFFS_BT709,
                ..unsafe { zeroed() }
            };
            match cfg.codec {
                Codec::H264 => {
                    let h = unsafe { &mut c.encodeCodecConfig.h264Config };
                    h.set_repeatSPSPPS(1); // SPS/PPS before every IDR
                    h.set_disableSPSPPS(0);
                    h.set_outputAUD(0);
                    h.set_outputBufferingPeriodSEI(0);
                    h.set_outputPictureTimingSEI(0);
                    h.set_outputRecoveryPointSEI(0);
                    h.set_enableIntraRefresh(0);
                    h.set_enableFillerDataInsertion(0);
                    h.idrPeriod = sys::NVENC_INFINITE_GOPLENGTH;
                    h.chromaFormatIDC = 1;
                    h.h264VUIParameters = vui;
                }
                Codec::Hevc => {
                    let h = unsafe { &mut c.encodeCodecConfig.hevcConfig };
                    h.set_repeatSPSPPS(1); // VPS/SPS/PPS before every IDR
                    h.set_disableSPSPPS(0);
                    h.set_outputAUD(0);
                    h.set_outputBufferingPeriodSEI(0);
                    h.set_outputPictureTimingSEI(0);
                    h.set_outputRecoveryPointSEI(0);
                    h.set_enableIntraRefresh(0);
                    h.set_enableFillerDataInsertion(0);
                    h.set_chromaFormatIDC(1);
                    h.set_pixelBitDepthMinus8(0);
                    h.idrPeriod = sys::NVENC_INFINITE_GOPLENGTH;
                    h.hevcVUIParameters = vui;
                }
            }

            let codec = self.codec_guid();
            let p = &mut *self.init;
            p.version = NV_ENC_INITIALIZE_PARAMS_VER;
            p.encodeGUID = codec;
            p.presetGUID = PRESET;
            p.tuningInfo = TUNING;
            p.encodeWidth = cfg.width;
            p.encodeHeight = cfg.height;
            p.darWidth = cfg.width;
            p.darHeight = cfg.height;
            p.maxEncodeWidth = cfg.width;
            p.maxEncodeHeight = cfg.height;
            p.frameRateNum = cfg.fps;
            p.frameRateDen = 1;
            p.enableEncodeAsync = 0;
            p.enablePTD = 1;
            p.encodeConfig = &mut *self.nv_config;
            let st = nv!(api, nvEncInitializeEncoder(self.enc, p));
            check(api, self.enc, "initialize", st)?;
            self.initialized = true;
            Ok(())
        }

        /// Changes the bitrate from the next frame on, without a keyframe.
        pub fn set_bitrate(&mut self, kbps: u32) -> Result<(), String> {
            if kbps == 0 {
                return Err("NVENC: bitrate must be above 0".into());
            }
            if kbps == self.cfg.bitrate_kbps {
                return Ok(());
            }
            let old = self.nv_config.rcParams;
            let rc = &mut self.nv_config.rcParams;
            rc.averageBitRate = bits_per_second(kbps);
            rc.maxBitRate = bits_per_second(kbps);
            rc.vbvBufferSize = vbv_bits(kbps, self.cfg.fps);
            rc.vbvInitialDelay = rc.vbvBufferSize;

            let mut re: Box<sys::NV_ENC_RECONFIGURE_PARAMS> = Box::new(unsafe { zeroed() });
            re.version = NV_ENC_RECONFIGURE_PARAMS_VER;
            // Plain C data (bindgen doesn't derive Copy for it): a bitwise copy is a copy.
            re.reInitEncodeParams = unsafe { std::ptr::read(&*self.init) };
            re.reInitEncodeParams.encodeConfig = &mut *self.nv_config;
            re.set_resetEncoder(0);
            re.set_forceIDR(0);
            let st = nv!(self.api, nvEncReconfigureEncoder(self.enc, &mut *re));
            if let Err(e) = check(self.api, self.enc, "set bitrate", st) {
                self.nv_config.rcParams = old;
                return Err(e);
            }
            self.cfg.bitrate_kbps = kbps;
            Ok(())
        }

        /// Encodes one picture. The first picture, and any with `force_keyframe`, is an IDR with
        /// parameter sets.
        pub fn encode(&mut self, input: &ID3D11Texture2D, force_keyframe: bool) -> Result<EncodedFrame, String> {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { input.GetDesc(&mut desc) };
            let format = match desc.Format {
                DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB | DXGI_FORMAT_B8G8R8X8_UNORM => {
                    sys::NV_ENC_BUFFER_FORMAT_ARGB // NVENC's ARGB is word order: B, G, R, A in memory
                }
                DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => sys::NV_ENC_BUFFER_FORMAT_ABGR,
                f => return Err(format!("NVENC: input texture format {} isn't BGRA or RGBA", f.0)),
            };

            let source = if (desc.Width, desc.Height) == (self.cfg.width, self.cfg.height) {
                input.clone()
            } else {
                let fits = self.scaler.as_ref().is_some_and(|s| s.fits(&desc));
                if !fits {
                    if let Some(old) = self.scaler.take() {
                        self.unregister(&old.output);
                    }
                    self.scaler = Some(Scaler::new(&self.device, &desc, &self.cfg)?);
                }
                let scaler = self.scaler.as_mut().expect("scaler");
                scaler.scale(input)?;
                scaler.output.clone()
            };
            let resource = self.registration(&source, format)?;

            let mut map: sys::NV_ENC_MAP_INPUT_RESOURCE = unsafe { zeroed() };
            map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
            map.registeredResource = resource;
            let st = nv!(self.api, nvEncMapInputResource(self.enc, &mut map));
            check(self.api, self.enc, "map input", st)?;

            let result = self.encode_mapped(map.mappedResource, map.mappedBufferFmt, force_keyframe);

            let st = nv!(self.api, nvEncUnmapInputResource(self.enc, map.mappedResource));
            let unmapped = check(self.api, self.enc, "unmap input", st);
            let frame = result?;
            unmapped?;
            self.frame_idx = self.frame_idx.wrapping_add(1);
            Ok(frame)
        }

        fn encode_mapped(
            &mut self,
            input: sys::NV_ENC_INPUT_PTR,
            format: sys::NV_ENC_BUFFER_FORMAT,
            force_keyframe: bool,
        ) -> Result<EncodedFrame, String> {
            let mut pic: Box<sys::NV_ENC_PIC_PARAMS> = Box::new(unsafe { zeroed() });
            pic.version = NV_ENC_PIC_PARAMS_VER;
            pic.inputWidth = self.cfg.width;
            pic.inputHeight = self.cfg.height;
            pic.inputPitch = self.cfg.width;
            pic.frameIdx = self.frame_idx;
            pic.inputTimeStamp = self.frame_idx as u64;
            pic.inputBuffer = input;
            pic.outputBitstream = self.bitstream;
            pic.bufferFmt = format;
            pic.pictureStruct = sys::NV_ENC_PIC_STRUCT_FRAME;
            if force_keyframe || self.frame_idx == 0 {
                pic.encodePicFlags = sys::NV_ENC_PIC_FLAG_FORCEIDR | sys::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
            }
            let st = nv!(self.api, nvEncEncodePicture(self.enc, &mut *pic));
            // Without B-frames or lookahead every picture comes out at once; NEED_MORE_INPUT would
            // mean the session isn't set up the way it was asked to be.
            check(self.api, self.enc, "encode", st)?;

            let mut lock: Box<sys::NV_ENC_LOCK_BITSTREAM> = Box::new(unsafe { zeroed() });
            lock.version = NV_ENC_LOCK_BITSTREAM_VER;
            lock.set_doNotWait(0);
            lock.outputBitstream = self.bitstream;
            let st = nv!(self.api, nvEncLockBitstream(self.enc, &mut *lock));
            check(self.api, self.enc, "lock bitstream", st)?;
            let data: &[u8] = if lock.bitstreamBufferPtr.is_null() || lock.bitstreamSizeInBytes == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(lock.bitstreamBufferPtr as *const u8, lock.bitstreamSizeInBytes as usize) }
            };
            let mut frame = parse_access_unit(self.cfg.codec, data);
            frame.keyframe |= lock.pictureType == sys::NV_ENC_PIC_TYPE_IDR;
            let st = nv!(self.api, nvEncUnlockBitstream(self.enc, self.bitstream));
            check(self.api, self.enc, "unlock bitstream", st)?;
            Ok(frame)
        }

        /// The texture's NVENC registration, registering it (and dropping the oldest) if new.
        fn registration(&mut self, texture: &ID3D11Texture2D, format: sys::NV_ENC_BUFFER_FORMAT) -> Result<sys::NV_ENC_REGISTERED_PTR, String> {
            if let Some(r) = self.registered.iter().find(|r| r.texture.as_raw() == texture.as_raw()) {
                return Ok(r.handle);
            }
            if self.registered.len() >= MAX_REGISTERED {
                let old = self.registered.remove(0);
                nv!(self.api, nvEncUnregisterResource(self.enc, old.handle));
            }
            let mut reg: Box<sys::NV_ENC_REGISTER_RESOURCE> = Box::new(unsafe { zeroed() });
            reg.version = NV_ENC_REGISTER_RESOURCE_VER;
            reg.resourceType = sys::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg.width = self.cfg.width;
            reg.height = self.cfg.height;
            reg.pitch = 0;
            reg.subResourceIndex = 0;
            reg.resourceToRegister = texture.as_raw();
            reg.bufferFormat = format;
            reg.bufferUsage = sys::NV_ENC_INPUT_IMAGE;
            let st = nv!(self.api, nvEncRegisterResource(self.enc, &mut *reg));
            check(self.api, self.enc, "register texture", st)?;
            self.registered.push(Registration { texture: texture.clone(), handle: reg.registeredResource });
            Ok(reg.registeredResource)
        }

        fn unregister(&mut self, texture: &ID3D11Texture2D) {
            if let Some(i) = self.registered.iter().position(|r| r.texture.as_raw() == texture.as_raw()) {
                let r = self.registered.remove(i);
                nv!(self.api, nvEncUnregisterResource(self.enc, r.handle));
            }
        }
    }

    impl Drop for Encoder {
        fn drop(&mut self) {
            if self.enc.is_null() {
                return;
            }
            if self.initialized {
                // End of stream: flushes the (synchronous, so empty) queue, as NVIDIA's samples do.
                let mut pic: Box<sys::NV_ENC_PIC_PARAMS> = Box::new(unsafe { zeroed() });
                pic.version = NV_ENC_PIC_PARAMS_VER;
                pic.encodePicFlags = sys::NV_ENC_PIC_FLAG_EOS;
                nv!(self.api, nvEncEncodePicture(self.enc, &mut *pic));
            }
            for r in self.registered.drain(..) {
                nv!(self.api, nvEncUnregisterResource(self.enc, r.handle));
            }
            if !self.bitstream.is_null() {
                nv!(self.api, nvEncDestroyBitstreamBuffer(self.enc, self.bitstream));
                self.bitstream = null_mut();
            }
            nv!(self.api, nvEncDestroyEncoder(self.enc));
            self.enc = null_mut();
        }
    }

    /// Opens (and closes) a session on the first NVIDIA GPU.
    fn probe() -> Result<(), String> {
        let api = api()?;
        let device = nvidia_device().ok_or("NVENC: no NVIDIA GPU")?;
        let enc = open_session(api, &device)?;
        nv!(api, nvEncDestroyEncoder(enc));
        Ok(())
    }

    /// A D3D11 device on the first NVIDIA adapter.
    fn nvidia_device() -> Option<ID3D11Device> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                if adapter.GetDesc1().map(|d| d.VendorId) != Ok(0x10de) {
                    continue;
                }
                let mut device = None;
                let made = D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                );
                if made.is_ok() && device.is_some() {
                    return device;
                }
            }
        }
        None
    }

    /// Scales BGRA/RGBA textures to the encoder's size with the D3D11 video processor (on the GPU's
    /// video engine, not its 3D engine, so a game at full load doesn't hold it up much).
    struct Scaler {
        in_width: u32,
        in_height: u32,
        in_format: DXGI_FORMAT,
        video_context: ID3D11VideoContext,
        video_device: ID3D11VideoDevice,
        enumerator: ID3D11VideoProcessorEnumerator,
        processor: ID3D11VideoProcessor,
        output: ID3D11Texture2D,
        output_view: ID3D11VideoProcessorOutputView,
        /// Input views of recently seen textures (held, so their addresses stay unique).
        input_views: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    }

    impl Scaler {
        fn new(device: &ID3D11Device, input: &D3D11_TEXTURE2D_DESC, cfg: &EncoderConfig) -> Result<Scaler, String> {
            let err = |what: &str, e: windows::core::Error| format!("NVENC scaler {what}: {e}");
            unsafe {
                let video_device: ID3D11VideoDevice = device.cast().map_err(|e| err("video device", e))?;
                let video_context: ID3D11VideoContext = device
                    .GetImmediateContext()
                    .and_then(|c| c.cast())
                    .map_err(|e| err("video context", e))?;
                let rate = DXGI_RATIONAL { Numerator: cfg.fps, Denominator: 1 };
                let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: rate,
                    InputWidth: input.Width,
                    InputHeight: input.Height,
                    OutputFrameRate: rate,
                    OutputWidth: cfg.width,
                    OutputHeight: cfg.height,
                    Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                };
                let enumerator = video_device.CreateVideoProcessorEnumerator(&content).map_err(|e| err("enumerator", e))?;
                let support = enumerator.CheckVideoProcessorFormat(input.Format).map_err(|e| err("format", e))?;
                let both = (D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 | D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0) as u32;
                if support & both != both {
                    return Err(format!("NVENC scaler: the video processor can't scale format {}", input.Format.0));
                }
                let processor = video_device.CreateVideoProcessor(&enumerator, 0).map_err(|e| err("processor", e))?;
                video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);

                let out_desc = D3D11_TEXTURE2D_DESC {
                    Width: cfg.width,
                    Height: cfg.height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: input.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut output = None;
                device.CreateTexture2D(&out_desc, None, Some(&mut output)).map_err(|e| err("output texture", e))?;
                let output = output.ok_or("NVENC scaler: no output texture")?;
                let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut output_view = None;
                video_device
                    .CreateVideoProcessorOutputView(&output, &enumerator, &view_desc, Some(&mut output_view))
                    .map_err(|e| err("output view", e))?;
                let output_view = output_view.ok_or("NVENC scaler: no output view")?;
                Ok(Scaler {
                    in_width: input.Width,
                    in_height: input.Height,
                    in_format: input.Format,
                    video_context,
                    video_device,
                    enumerator,
                    processor,
                    output,
                    output_view,
                    input_views: Vec::new(),
                })
            }
        }

        fn fits(&self, input: &D3D11_TEXTURE2D_DESC) -> bool {
            (self.in_width, self.in_height, self.in_format) == (input.Width, input.Height, input.Format)
        }

        fn scale(&mut self, input: &ID3D11Texture2D) -> Result<(), String> {
            let view = match self.input_views.iter().find(|(t, _)| t.as_raw() == input.as_raw()) {
                Some((_, v)) => v.clone(),
                None => {
                    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                        },
                    };
                    let mut view = None;
                    unsafe {
                        self.video_device.CreateVideoProcessorInputView(input, &self.enumerator, &desc, Some(&mut view))
                    }
                    .map_err(|e| format!("NVENC scaler input view: {e}"))?;
                    let view = view.ok_or("NVENC scaler: no input view")?;
                    if self.input_views.len() >= MAX_REGISTERED {
                        self.input_views.remove(0);
                    }
                    self.input_views.push((input.clone(), view.clone()));
                    view
                }
            };
            let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(view)),
                ..Default::default()
            }];
            let r = unsafe { self.video_context.VideoProcessorBlt(&self.processor, &self.output_view, 0, &streams) };
            unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
            r.map_err(|e| format!("NVENC scaler: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn struct_versions_match_the_header() {
        // NVENCAPI_VERSION = 12 | 0 << 24; NVENCAPI_STRUCT_VERSION(v) = 12 | v << 16 | 7 << 28.
        assert_eq!(API_VERSION, 12);
        assert_eq!(sys::NVENCAPI_VERSION, API_VERSION);
        assert_eq!(NV_ENC_CONFIG_VER, 0xF008_000C); // (8) | 1<<31
        assert_eq!(NV_ENC_INITIALIZE_PARAMS_VER, 0xF005_000C); // (5) | 1<<31
        assert_eq!(NV_ENC_RECONFIGURE_PARAMS_VER, 0xF001_000C); // (1) | 1<<31
        assert_eq!(NV_ENC_PRESET_CONFIG_VER, 0xF004_000C); // (4) | 1<<31
        assert_eq!(NV_ENC_PIC_PARAMS_VER, 0xF006_000C); // (6) | 1<<31
        assert_eq!(NV_ENC_LOCK_BITSTREAM_VER, 0x7002_000C); // (2)
        assert_eq!(NV_ENC_MAP_INPUT_RESOURCE_VER, 0x7004_000C); // (4)
        assert_eq!(NV_ENC_REGISTER_RESOURCE_VER, 0x7004_000C); // (4)
        assert_eq!(NV_ENC_CREATE_BITSTREAM_BUFFER_VER, 0x7001_000C); // (1)
        assert_eq!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER, 0x7001_000C); // (1)
        assert_eq!(NV_ENCODE_API_FUNCTION_LIST_VER, 0x7002_000C); // (2)
        assert_eq!(NV_ENC_RC_PARAMS_VER, 0x7001_000C); // (1)
        assert_eq!(MIN_DRIVER_API, 0xC0); // 12 << 4 | 0
    }

    fn guid_string(g: &sys::GUID) -> String {
        let d = g.Data4;
        format!(
            "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            g.Data1, g.Data2, g.Data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }

    #[test]
    fn guids_match_the_header_comments() {
        // The header writes each GUID twice: as a {…} comment and as the initializer.
        let cases = [
            (NV_ENC_CODEC_H264_GUID, "6BC82762-4E63-4ca4-AA85-1E50F321F6BF"),
            (NV_ENC_CODEC_HEVC_GUID, "790CDC88-4522-4d7b-9425-BDA9975F7603"),
            (NV_ENC_CODEC_PROFILE_AUTOSELECT_GUID, "BFD6F8E7-233C-4341-8B3E-4818523803F4"),
            (NV_ENC_H264_PROFILE_BASELINE_GUID, "0727BCAA-78C4-4c83-8C2F-EF3DFF267C6A"),
            (NV_ENC_H264_PROFILE_MAIN_GUID, "60B5C1D4-67FE-4790-94D5-C4726D7B6E6D"),
            (NV_ENC_H264_PROFILE_HIGH_GUID, "E7CBC309-4F7A-4b89-AF2A-D537C92BE310"),
            (NV_ENC_HEVC_PROFILE_MAIN_GUID, "B514C39A-B55B-40fa-878F-F1253B4DFDEC"),
            (NV_ENC_PRESET_P1_GUID, "FC0A8D3E-45F8-4CF8-80C7-298871590EBF"),
            (NV_ENC_PRESET_P2_GUID, "F581CFB8-88D6-4381-93F0-DF13F9C27DAB"),
            (NV_ENC_PRESET_P3_GUID, "36850110-3A07-441F-94D5-3670631F91F6"),
            (NV_ENC_PRESET_P4_GUID, "90A7B826-DF06-4862-B9D2-CD6D73A08681"),
            (NV_ENC_PRESET_P5_GUID, "21C6E6B4-297A-4CBA-998F-B6CBDE72ADE3"),
            (NV_ENC_PRESET_P6_GUID, "8E75C279-6299-4AB6-8302-0B215A335CF5"),
            (NV_ENC_PRESET_P7_GUID, "84848C12-6F71-4C13-931B-53E283F57974"),
        ];
        for (g, s) in cases {
            assert_eq!(guid_string(&g), s.to_uppercase());
        }
    }

    #[test]
    fn struct_layouts() {
        // Worked out by hand from nvEncodeAPI.h for 64-bit Windows (4-byte enums and u32s, 8-byte
        // pointers). bindgen's own checks in nvenc_sys.rs cover every field; these guard the structs
        // passed to the driver against a regenerated file going wrong.
        assert_eq!(size_of::<sys::GUID>(), 16);
        // version, deviceType, device*, reserved*, apiVersion, reserved1[253], reserved2[64]*
        assert_eq!(size_of::<sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS>(), 8 + 8 + 8 + 4 + 253 * 4 + 64 * 8);
        assert_eq!(offset_of!(sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, apiVersion), 24);
        // version, size, memoryHeap, reserved, bitstreamBuffer*, bitstreamBufferPtr*, reserved1[58], reserved2[64]*
        assert_eq!(size_of::<sys::NV_ENC_CREATE_BITSTREAM_BUFFER>(), 16 + 16 + 58 * 4 + 64 * 8);
        // version, subResourceIndex, inputResource*, registeredResource*, mappedResource*,
        // mappedBufferFmt, reserved1[251], reserved2[63]*
        assert_eq!(size_of::<sys::NV_ENC_MAP_INPUT_RESOURCE>(), 8 + 24 + 4 + 251 * 4 + 63 * 8);
        assert_eq!(offset_of!(sys::NV_ENC_MAP_INPUT_RESOURCE, mappedBufferFmt), 32);
        // version, resourceType, width, height, pitch, subResourceIndex, resource*, registered*,
        // bufferFormat, bufferUsage, pInputFencePoint*, reserved1[247], (pad 4), reserved2[61]*
        assert_eq!(offset_of!(sys::NV_ENC_REGISTER_RESOURCE, resourceToRegister), 24);
        assert_eq!(offset_of!(sys::NV_ENC_REGISTER_RESOURCE, bufferFormat), 40);
        assert_eq!(size_of::<sys::NV_ENC_REGISTER_RESOURCE>(), 24 + 16 + 8 + 8 + 247 * 4 + 4 + 61 * 8);
        // NV_ENC_RC_PARAMS: version, mode, constQP(12), avg, max, vbv, vbvDelay, bitfields → 40;
        // minQP, maxQP, initialRCQP (36) → 76; temporallayerIdxMask → 80; temporalLayerQP[8] → 88;
        // targetQuality, LSB, lookaheadDepth(2) → 92; lowDelayKeyFrameScale, 3 × i8 → 96; qpMapMode,
        // multiPass, alphaLayerBitrateRatio → 108; 2 × i8 + u16 → 112; reserved[4] → 128.
        assert_eq!(size_of::<sys::NV_ENC_RC_PARAMS>(), 128);
        assert_eq!(offset_of!(sys::NV_ENC_RC_PARAMS, averageBitRate), 20);
        assert_eq!(offset_of!(sys::NV_ENC_RC_PARAMS, vbvBufferSize), 28);
        // The codec config union is the size of its biggest member, at least uint32_t[320].
        assert!(size_of::<sys::NV_ENC_CODEC_CONFIG>() >= 320 * 4);
        assert_eq!(align_of::<sys::NV_ENC_CONFIG>(), 8);
        // version, GUID(16) → 20, gopLength, frameIntervalP, monoChrome, frameFieldMode, mvPrecision → 40
        assert_eq!(offset_of!(sys::NV_ENC_CONFIG, rcParams), 40);
        assert_eq!(offset_of!(sys::NV_ENC_CONFIG, encodeCodecConfig), 168);
        // version, encodeGUID, presetGUID → 36; width, height, darW, darH, rateNum, rateDen, async, PTD → 68;
        // bitfields, privDataSize → 76; (pad 4) privData* at 80, encodeConfig* at 88, maxEncodeWidth at 96.
        assert_eq!(offset_of!(sys::NV_ENC_INITIALIZE_PARAMS, encodeConfig), 88);
        assert_eq!(offset_of!(sys::NV_ENC_INITIALIZE_PARAMS, maxEncodeWidth), 96);
        assert_eq!(offset_of!(sys::NV_ENC_RECONFIGURE_PARAMS, reInitEncodeParams), 8);
        // version, inW, inH, pitch, flags, frameIdx → 24; timestamp, duration → 40; input*, output*,
        // event* → 64; bufferFmt, pictureStruct, pictureType → 76; pad → 80; codecPicParams.
        assert_eq!(offset_of!(sys::NV_ENC_PIC_PARAMS, inputBuffer), 40);
        assert_eq!(offset_of!(sys::NV_ENC_PIC_PARAMS, bufferFmt), 64);
        assert_eq!(offset_of!(sys::NV_ENC_PIC_PARAMS, codecPicParams), 80);
        // version, bitfields, outputBitstream* → 16, sliceOffsets* → 24, frameIdx, status, numSlices,
        // size → 40, timestamp, duration → 56, bitstreamBufferPtr* → 64, pictureType → 68.
        assert_eq!(offset_of!(sys::NV_ENC_LOCK_BITSTREAM, outputBitstream), 8);
        assert_eq!(offset_of!(sys::NV_ENC_LOCK_BITSTREAM, bitstreamSizeInBytes), 36);
        assert_eq!(offset_of!(sys::NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr), 56);
        assert_eq!(offset_of!(sys::NV_ENC_LOCK_BITSTREAM, pictureType), 64);
        // version, reserved, then 41 pointer slots (40 functions and reserved1 at index 33), then
        // reserved2[277]: nvEncOpenEncodeSessionEx is index 29, nvEncGetEncodePresetConfigEx index 39.
        assert_eq!(offset_of!(sys::NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSession), 8);
        assert_eq!(offset_of!(sys::NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSessionEx), 8 + 29 * 8);
        assert_eq!(offset_of!(sys::NV_ENCODE_API_FUNCTION_LIST, nvEncReconfigureEncoder), 8 + 32 * 8);
        assert_eq!(offset_of!(sys::NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodePresetConfigEx), 8 + 39 * 8);
        assert_eq!(size_of::<sys::NV_ENCODE_API_FUNCTION_LIST>(), 8 + 41 * 8 + 277 * 8);
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(sys::NV_ENC_SUCCESS), "NV_ENC_SUCCESS");
        assert_eq!(status_name(10), "NV_ENC_ERR_OUT_OF_MEMORY");
        assert_eq!(status_name(25), "NV_ENC_ERR_RESOURCE_NOT_MAPPED");
        assert_eq!(status_name(999), "unknown NVENCSTATUS");
    }

    #[test]
    fn splits_annex_b() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS, 4-byte start code
            0, 0, 1, 0x68, 3, // PPS, 3-byte start code
            0, 0, 0, 1, 0x65, 4, 0, 0, 3, 0, 5, // IDR slice with an emulation-prevention byte
            0, 0, // trailing zeros
        ];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 0, 0, 3, 0, 5][..]]);
        assert!(split_annex_b(&[]).is_empty());
        assert!(split_annex_b(&[1, 2, 3]).is_empty());
        assert!(split_annex_b(&[0, 0, 1]).is_empty());
        assert_eq!(split_annex_b(&[9, 0, 0, 1, 0x41, 0, 0, 1]), vec![&[0x41][..]]);
        // A start code straddling the skip-ahead.
        assert_eq!(split_annex_b(&[0x41, 7, 0, 0, 1, 0x41, 8]), vec![&[0x41, 8][..]]);
        assert_eq!(split_annex_b(&[0, 0, 1, 0x41, 7, 9, 0, 0, 0, 1, 0x41, 8]), vec![&[0x41, 7, 9][..], &[0x41, 8][..]]);
    }

    #[test]
    fn parses_h264_access_units() {
        let key = [
            0, 0, 0, 1, 0x09, 0xf0, // AUD
            0, 0, 0, 1, 0x67, 0x64, 0, 0x28, // SPS
            0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, // PPS
            0, 0, 0, 1, 0x06, 5, 1, 0x80, // SEI
            0, 0, 0, 1, 0x65, 0x88, 0x84, // IDR slice
            0, 0, 1, 0x65, 0x00, 0x11, // second IDR slice
        ];
        let f = parse_access_unit(Codec::H264, &key);
        assert!(f.keyframe);
        assert_eq!(f.parameter_sets, vec![vec![0x67, 0x64, 0, 0x28], vec![0x68, 0xee, 0x3c, 0x80]]);
        assert_eq!(f.nals, vec![vec![0x65, 0x88, 0x84], vec![0x65, 0x00, 0x11]]);

        let p = parse_access_unit(Codec::H264, &[0, 0, 0, 1, 0x41, 0x9a, 0x02]);
        assert!(!p.keyframe);
        assert!(p.parameter_sets.is_empty());
        assert_eq!(p.nals, vec![vec![0x41, 0x9a, 0x02]]);
    }

    #[test]
    fn parses_hevc_access_units() {
        let key = [
            0, 0, 0, 1, 0x46, 0x01, 0x10, // AUD (35)
            0, 0, 0, 1, 0x40, 0x01, 0x0c, // VPS (32)
            0, 0, 0, 1, 0x42, 0x01, 0x01, // SPS (33)
            0, 0, 0, 1, 0x44, 0x01, 0xc1, // PPS (34)
            0, 0, 0, 1, 0x4e, 0x01, 0x05, // prefix SEI (39)
            0, 0, 0, 1, 0x26, 0x01, 0xaf, // IDR_W_RADL (19)
        ];
        let f = parse_access_unit(Codec::Hevc, &key);
        assert!(f.keyframe);
        assert_eq!(f.parameter_sets, vec![vec![0x40, 0x01, 0x0c], vec![0x42, 0x01, 0x01], vec![0x44, 0x01, 0xc1]]);
        assert_eq!(f.nals, vec![vec![0x26, 0x01, 0xaf]]);

        let p = parse_access_unit(Codec::Hevc, &[0, 0, 0, 1, 0x02, 0x01, 0xd0]); // TRAIL_R (1)
        assert!(!p.keyframe);
        assert_eq!(p.nals, vec![vec![0x02, 0x01, 0xd0]]);
        assert_eq!(nal_type(Codec::Hevc, &[0x28, 0x01]), 20); // IDR_N_LP
        assert_eq!(classify(Codec::Hevc, &[0x28, 0x01]), NalKind::KeySlice);
        assert_eq!(classify(Codec::Hevc, &[0x50, 0x01]), NalKind::Drop); // suffix SEI (40)
    }

    #[test]
    fn rate_control_numbers() {
        assert_eq!(bits_per_second(20_000), 20_000_000);
        assert_eq!(bits_per_second(u32::MAX), u32::MAX);
        assert_eq!(vbv_bits(20_000, 60), 333_333);
        assert_eq!(vbv_bits(1, 240), 4);
    }

    #[test]
    fn validates_configs() {
        let ok = EncoderConfig { width: 1920, height: 1080, fps: 60, bitrate_kbps: 20_000, codec: Codec::H264 };
        assert!(validate(&ok).is_ok());
        assert!(validate(&EncoderConfig { width: 1921, ..ok.clone() }).is_err());
        assert!(validate(&EncoderConfig { width: 5120, ..ok.clone() }).is_err());
        assert!(validate(&EncoderConfig { width: 5120, codec: Codec::Hevc, ..ok.clone() }).is_ok());
        assert!(validate(&EncoderConfig { fps: 0, ..ok.clone() }).is_err());
        assert!(validate(&EncoderConfig { bitrate_kbps: 0, ..ok }).is_err());
    }
}
