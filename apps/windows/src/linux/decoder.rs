//! H.264 / HEVC → pictures, with FFmpeg's libavcodec. On the GPU when there is one (NVIDIA through
//! CUDA/NVDEC, Intel and AMD through VA-API), else on the CPU. Low delay: one frame in, one picture
//! out, no frame threading.
//!
//! Pictures come back as NV12 (GPU) or I420 (CPU) planes in memory, for the stream window's texture.

use crate::logi;
use ffmpeg_next::ffi::*;
use std::ptr::{null, null_mut};

const EAGAIN: i32 = -11;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Layout {
    /// Y plane, then interleaved U/V at half size.
    Nv12,
    /// Y, U, V planes (U and V at half size).
    I420,
}

pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    pub planes: [Vec<u8>; 3],
    pub strides: [usize; 3],
}

pub struct Decoder {
    ctx: *mut AVCodecContext,
    hw_device: *mut AVBufferRef,
    packet: *mut AVPacket,
    frame: *mut AVFrame,
    sw_frame: *mut AVFrame,
    codec: u8,
}

unsafe impl Send for Decoder {}

fn err(code: i32) -> String {
    let mut buf = [0i8; 128];
    unsafe { av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// The hardware format chosen when the decoder opened (stored in `opaque`).
unsafe extern "C" fn pick_format(ctx: *mut AVCodecContext, formats: *const AVPixelFormat) -> AVPixelFormat {
    unsafe {
        let want = (*ctx).opaque as usize as i32;
        let mut p = formats;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == want {
                return *p;
            }
            p = p.add(1);
        }
        // Not offered (the stream's profile isn't supported in hardware): the first software format.
        let mut p = formats;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p != AVPixelFormat::AV_PIX_FMT_CUDA && *p != AVPixelFormat::AV_PIX_FMT_VAAPI {
                return *p;
            }
            p = p.add(1);
        }
        AVPixelFormat::AV_PIX_FMT_NONE
    }
}

impl Decoder {
    /// `codec`: 1 H.264, 2 HEVC. Tries CUDA, then VA-API, then the CPU (BB1_DECODER=cpu forces it).
    pub fn new(codec: u8) -> Result<Decoder, String> {
        let force_cpu = std::env::var("BB1_DECODER").is_ok_and(|v| v == "cpu");
        let tries: &[(&str, AVHWDeviceType, AVPixelFormat)] = &[
            ("NVIDIA (CUDA)", AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, AVPixelFormat::AV_PIX_FMT_CUDA),
            ("VA-API", AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, AVPixelFormat::AV_PIX_FMT_VAAPI),
        ];
        if !force_cpu {
            for &(name, kind, format) in tries {
                match Self::open(codec, Some((kind, format)), name) {
                    Ok(d) => return Ok(d),
                    Err(e) => logi!("decoder: {name}: {e}"),
                }
            }
        }
        Self::open(codec, None, "CPU")
    }

    fn open(codec: u8, hw: Option<(AVHWDeviceType, AVPixelFormat)>, kind: &'static str) -> Result<Decoder, String> {
        unsafe {
            let id = if codec == 2 { AVCodecID::AV_CODEC_ID_HEVC } else { AVCodecID::AV_CODEC_ID_H264 };
            let c = avcodec_find_decoder(id);
            if c.is_null() {
                return Err("FFmpeg has no decoder for it".into());
            }
            let ctx = avcodec_alloc_context3(c);
            let mut d = Decoder {
                ctx,
                hw_device: null_mut(),
                packet: av_packet_alloc(),
                frame: av_frame_alloc(),
                sw_frame: av_frame_alloc(),
                codec,
            };
            if ctx.is_null() || d.packet.is_null() || d.frame.is_null() || d.sw_frame.is_null() {
                return Err("out of memory".into());
            }
            let cx = &mut *ctx;
            cx.flags |= AV_CODEC_FLAG_LOW_DELAY as i32;
            cx.flags2 |= AV_CODEC_FLAG2_FAST;
            if let Some((device_type, format)) = hw {
                let r = av_hwdevice_ctx_create(&mut d.hw_device, device_type, null(), null_mut(), 0);
                if r < 0 {
                    return Err(err(r));
                }
                cx.hw_device_ctx = av_buffer_ref(d.hw_device);
                cx.opaque = format as i32 as usize as *mut std::ffi::c_void;
                cx.get_format = Some(pick_format);
                cx.thread_count = 1;
            } else {
                // Slice threads only: frame threads would hold pictures back.
                cx.thread_type = FF_THREAD_SLICE;
                cx.thread_count = 0;
            }
            let r = avcodec_open2(ctx, c, null_mut());
            if r < 0 {
                return Err(err(r));
            }
            logi!("decoder: {} on {kind}", if codec == 2 { "HEVC" } else { "H.264" });
            Ok(d)
        }
    }

    pub fn codec(&self) -> u8 {
        self.codec
    }

    /// One frame (Annex B) in; its picture out (None: the decoder needs more, e.g. no keyframe yet).
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<Picture>, String> {
        unsafe {
            let r = av_new_packet(self.packet, annex_b.len() as i32);
            if r < 0 {
                return Err(err(r));
            }
            std::ptr::copy_nonoverlapping(annex_b.as_ptr(), (*self.packet).data, annex_b.len());
            let r = avcodec_send_packet(self.ctx, self.packet);
            av_packet_unref(self.packet);
            if r < 0 && r != EAGAIN {
                return Err(format!("decode: {}", err(r)));
            }
            let mut picture = None;
            loop {
                let r = avcodec_receive_frame(self.ctx, self.frame);
                if r == EAGAIN || r == AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    return Err(format!("decode: {}", err(r)));
                }
                picture = Some(self.take()?);
                av_frame_unref(self.frame);
            }
            Ok(picture)
        }
    }

    unsafe fn take(&mut self) -> Result<Picture, String> {
        unsafe {
            let mut f = self.frame;
            let hw = (*f).format == AVPixelFormat::AV_PIX_FMT_CUDA as i32 || (*f).format == AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            if hw {
                av_frame_unref(self.sw_frame);
                (*self.sw_frame).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
                let r = av_hwframe_transfer_data(self.sw_frame, f, 0);
                if r < 0 {
                    return Err(format!("copy from the GPU: {}", err(r)));
                }
                f = self.sw_frame;
            }
            let fr = &*f;
            let (w, h) = (fr.width as usize, fr.height as usize);
            let layout = if fr.format == AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
                Layout::Nv12
            } else if fr.format == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 || fr.format == AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32 {
                Layout::I420
            } else {
                return Err(format!("unexpected picture format {}", fr.format));
            };
            let rows = |plane: usize| if plane == 0 { h } else { h.div_ceil(2) };
            let planes_n = if layout == Layout::Nv12 { 2 } else { 3 };
            let mut planes: [Vec<u8>; 3] = Default::default();
            let mut strides = [0usize; 3];
            for (i, plane) in planes.iter_mut().enumerate().take(planes_n) {
                let stride = fr.linesize[i] as usize;
                let len = stride * rows(i);
                *plane = std::slice::from_raw_parts(fr.data[i], len).to_vec();
                strides[i] = stride;
            }
            if hw {
                av_frame_unref(self.sw_frame);
            }
            Ok(Picture { width: w as u32, height: h as u32, layout, planes, strides })
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            av_frame_free(&mut self.sw_frame);
            av_packet_free(&mut self.packet);
            avcodec_free_context(&mut self.ctx);
            av_buffer_unref(&mut self.hw_device);
        }
    }
}
