//! H.264 decoding with Windows' built-in Media Foundation decoder. With a Direct3D 11 device it
//! decodes on the GPU and hands back textures; otherwise it decodes on the CPU into memory.
//! Low-latency mode makes it return each frame as soon as it's decoded (the host never sends
//! B-frames, so there's nothing to reorder).

use crate::logi;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{Interface, Result};

pub struct Decoder {
    mft: IMFTransform,
    pub hardware: bool,
    /// Coded frame size (e.g. 1920×1088 for a 1080p stream).
    pub frame_size: (u32, u32),
    /// Bytes per row in memory (software decoding).
    pub stride: u32,
    provides_samples: bool,
    output_bytes: u32,
    time: i64,
    _manager: Option<IMFDXGIDeviceManager>,
}

/// One decoded picture.
pub struct Picture {
    pub sample: IMFSample,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl Decoder {
    /// `device`: decode on the GPU through it when the decoder supports that.
    pub fn new(width: u32, height: u32, device: Option<&ID3D11Device>) -> Result<Decoder> {
        if let Some(d) = device {
            match Self::create(width, height, Some(d)) {
                Ok(dec) => return Ok(dec),
                Err(e) => logi!("decoder: GPU decoding unavailable ({e}); decoding on the CPU"),
            }
        }
        Self::create(width, height, None)
    }

    fn create(width: u32, height: u32, device: Option<&ID3D11Device>) -> Result<Decoder> {
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            let attrs = mft.GetAttributes()?;
            let _ = attrs.SetUINT32(&CODECAPI_AVLowLatencyMode, 1);
            let mut manager = None;
            if let Some(device) = device {
                if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                    return Err(windows::core::Error::new(MF_E_UNSUPPORTED_D3D_TYPE, "decoder isn't Direct3D 11 aware"));
                }
                let mut token = 0u32;
                let mut m: Option<IMFDXGIDeviceManager> = None;
                MFCreateDXGIDeviceManager(&mut token, &mut m)?;
                let m = m.unwrap();
                m.ResetDevice(device, token)?;
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, m.as_raw() as usize)?;
                manager = Some(m);
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(60, 1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &input, 0)?;

            let mut dec = Decoder {
                mft,
                hardware: manager.is_some(),
                frame_size: (width, height),
                stride: width,
                provides_samples: false,
                output_bytes: 0,
                time: 0,
                _manager: manager,
            };
            dec.choose_output()?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            logi!("decoder: H.264 {} {}x{}", if dec.hardware { "on the GPU" } else { "on the CPU" }, width, height);
            Ok(dec)
        }
    }

    /// Picks NV12 output (what the renderer draws), and reads the frame size and buffer layout.
    fn choose_output(&mut self) -> Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        self.frame_size = ((size >> 32) as u32, size as u32);
                    }
                    self.stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(self.frame_size.0 as i32).unsigned_abs();
                    break;
                }
                i += 1;
            }
            let info = self.mft.GetOutputStreamInfo(0)?;
            let flags = info.dwFlags as i32;
            self.provides_samples = flags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) != 0;
            self.output_bytes = info.cbSize;
            Ok(())
        }
    }

    /// Decodes one frame (Annex B, parameter sets included on keyframes) and returns what came out.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<Picture>> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(annex_b.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(annex_b.as_ptr(), ptr, annex_b.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(annex_b.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(166_667)?;
            self.time += 166_667;

            let mut out = Vec::new();
            loop {
                match self.mft.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    // It still has a picture for us: take it, then feed the frame again.
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        let before = out.len();
                        self.drain(&mut out)?;
                        if out.len() == before {
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            self.drain(&mut out)?;
            Ok(out)
        }
    }

    fn drain(&mut self, out: &mut Vec<Picture>) -> Result<()> {
        unsafe {
            loop {
                let own = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.output_bytes.max(1))?)?;
                    Some(s)
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(own),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.mft.ProcessOutput(0, &mut buffers, &mut status);
                let sample = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
                drop(std::mem::ManuallyDrop::take(&mut buffers[0].pEvents));
                match result {
                    Ok(()) => {
                        if let Some(sample) = sample {
                            out.push(Picture { sample });
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.choose_output()?;
                        logi!("decoder: format is now {}x{} (stride {})", self.frame_size.0, self.frame_size.1, self.stride);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}

/// The Direct3D texture inside a GPU-decoded picture: (texture, array slice).
pub fn texture_of(p: &Picture) -> Option<(windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, u32)> {
    unsafe {
        let buffer = p.sample.GetBufferByIndex(0).ok()?;
        let dxgi: IMFDXGIBuffer = buffer.cast().ok()?;
        let mut raw = std::ptr::null_mut();
        dxgi.GetResource(&windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::IID, &mut raw).ok()?;
        let texture = windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::from_raw(raw);
        let index = dxgi.GetSubresourceIndex().ok()?;
        Some((texture, index))
    }
}
