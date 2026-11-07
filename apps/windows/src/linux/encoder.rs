//! H.264 for one viewer, with FFmpeg's libavcodec: NVENC on an NVIDIA card (the same low-latency
//! settings as the Windows host's src/host/nvenc.rs), else x264 on the CPU.
//!
//! NVENC takes the screen's BGRx/RGBx frames as they are and converts them to YUV on the GPU; x264
//! gets them converted by swscale. Both: I and P frames only, an IDR (with SPS and PPS) only when
//! asked, constant bitrate with a one-frame VBV, BT.709 limited range (what every client expects).
//! The bitrate changes between frames without a keyframe (libavcodec reconfigures both encoders).

use crate::host::nvenc::{Codec, EncodedFrame, parse_access_unit};
use crate::logi;
use ffmpeg_next::ffi::*;
use std::ffi::CString;
use std::ptr::{null, null_mut};

const EAGAIN: i32 = -11;

/// The free function of a borrowed buffer: nothing to free.
unsafe extern "C" fn keep(_opaque: *mut std::ffi::c_void, _data: *mut u8) {}

pub struct Encoder {
    ctx: *mut AVCodecContext,
    frame: *mut AVFrame,
    packet: *mut AVPacket,
    /// x264 only: BGRx → YUV 4:2:0.
    sws: *mut SwsContext,
    yuv: *mut AVFrame,
    input: AVPixelFormat,
    pub width: u32,
    pub height: u32,
    fps: u32,
    kbps: u32,
    pts: i64,
    pub name: &'static str,
}

// Used only from the host's video thread.
unsafe impl Send for Encoder {}

fn err(code: i32) -> String {
    let mut buf = [0i8; 128];
    unsafe { av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

unsafe fn set(obj: *mut std::ffi::c_void, key: &str, value: &str) {
    let (k, v) = (CString::new(key).unwrap(), CString::new(value).unwrap());
    let r = unsafe { av_opt_set(obj, k.as_ptr(), v.as_ptr(), AV_OPT_SEARCH_CHILDREN) };
    if r < 0 {
        logi!("encoder: option {key}={value} not taken ({})", err(r));
    }
}

impl Encoder {
    /// `rgb`: the frames are RGBx (else BGRx). Width and height are made even (the last column or
    /// row is left out).
    pub fn new(width: u32, height: u32, fps: u32, kbps: u32, rgb: bool) -> Result<Encoder, String> {
        let (w, h) = (width & !1, height & !1);
        if w < 64 || h < 64 || w > 4096 || h > 4096 {
            return Err(format!("{width}x{height} is outside what H.264 encoders take (64…4096)"));
        }
        match Self::open("h264_nvenc", w, h, fps, kbps, rgb) {
            Ok(e) => Ok(e),
            Err(nv) => {
                let e = Self::open("libx264", w, h, fps, kbps, rgb).map_err(|x| format!("no H.264 encoder: NVENC: {nv}; x264: {x}"))?;
                logi!("encoder: NVENC unavailable ({nv}); using x264 on the CPU");
                Ok(e)
            }
        }
    }

    fn open(name: &'static str, width: u32, height: u32, fps: u32, kbps: u32, rgb: bool) -> Result<Encoder, String> {
        unsafe {
            let cname = CString::new(name).unwrap();
            let codec = avcodec_find_encoder_by_name(cname.as_ptr());
            if codec.is_null() {
                return Err(format!("FFmpeg has no {name}"));
            }
            let ctx = avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return Err("out of memory".into());
            }
            let nvenc = name == "h264_nvenc";
            let input = if rgb { AVPixelFormat::AV_PIX_FMT_RGB0 } else { AVPixelFormat::AV_PIX_FMT_BGR0 };
            let mut e = Encoder {
                ctx,
                frame: av_frame_alloc(),
                packet: av_packet_alloc(),
                sws: null_mut(),
                yuv: null_mut(),
                input,
                width,
                height,
                fps,
                kbps,
                pts: 0,
                name: if nvenc { "NVENC" } else { "x264" },
            };
            let c = &mut *ctx;
            c.width = width as i32;
            c.height = height as i32;
            c.time_base = AVRational { num: 1, den: fps as i32 };
            c.framerate = AVRational { num: fps as i32, den: 1 };
            c.pix_fmt = if nvenc { input } else { AVPixelFormat::AV_PIX_FMT_YUV420P };
            c.gop_size = i32::MAX; // keyframes only when asked
            c.max_b_frames = 0;
            c.colorspace = AVColorSpace::AVCOL_SPC_BT709;
            c.color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
            c.color_trc = AVColorTransferCharacteristic::AVCOL_TRC_BT709;
            c.color_range = AVColorRange::AVCOL_RANGE_MPEG;
            c.flags |= AV_CODEC_FLAG_LOW_DELAY as i32;
            e.apply_rate(kbps);
            let p = c.priv_data;
            if nvenc {
                set(p, "preset", "p1");
                set(p, "tune", "ull");
                set(p, "rc", "cbr");
                set(p, "zerolatency", "1");
                set(p, "delay", "0");
                set(p, "forced-idr", "1");
                set(p, "no-scenecut", "1");
                set(p, "profile", "high");
            } else {
                set(p, "preset", "superfast");
                set(p, "tune", "zerolatency");
                set(p, "forced-idr", "1");
                set(p, "x264-params", "scenecut=0:nal-hrd=none");
                c.thread_count = 0;
            }
            let r = avcodec_open2(ctx, codec, null_mut());
            if r < 0 {
                return Err(format!("{name}: {}", err(r)));
            }
            if e.frame.is_null() || e.packet.is_null() {
                return Err("out of memory".into());
            }
            if !nvenc {
                e.sws = sws_getContext(
                    width as i32,
                    height as i32,
                    input,
                    width as i32,
                    height as i32,
                    AVPixelFormat::AV_PIX_FMT_YUV420P,
                    SwsFlags::SWS_BILINEAR as i32,
                    null_mut(),
                    null_mut(),
                    null(),
                );
                if e.sws.is_null() {
                    return Err("swscale: no BGRx → YUV converter".into());
                }
                // BT.709, limited range, like the stream says.
                let coef = sws_getCoefficients(SWS_CS_ITU709);
                sws_setColorspaceDetails(e.sws, coef, 1, coef, 0, 0, 1 << 16, 1 << 16);
                e.yuv = av_frame_alloc();
                let y = &mut *e.yuv;
                y.format = AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
                y.width = width as i32;
                y.height = height as i32;
                if av_frame_get_buffer(e.yuv, 32) < 0 {
                    return Err("out of memory".into());
                }
            }
            logi!("encoder: {} {}x{} {} fps {} kbps", e.name, width, height, fps, kbps);
            Ok(e)
        }
    }

    fn apply_rate(&mut self, kbps: u32) {
        let c = unsafe { &mut *self.ctx };
        let bps = kbps as i64 * 1000;
        c.bit_rate = bps;
        c.rc_max_rate = bps;
        c.rc_min_rate = 0;
        c.rc_buffer_size = (bps / self.fps.max(1) as i64).max(1) as i32;
    }

    /// From the next frame on, without a keyframe.
    pub fn set_bitrate(&mut self, kbps: u32) {
        if kbps > 0 && kbps != self.kbps {
            self.kbps = kbps;
            self.apply_rate(kbps);
        }
    }

    /// Encodes one picture (`data`: rows of `stride` bytes, 4 bytes a pixel, at least our size).
    pub fn encode(&mut self, data: &[u8], stride: usize, force_keyframe: bool) -> Result<EncodedFrame, String> {
        if data.len() < stride * self.height as usize || stride < self.width as usize * 4 {
            return Err("picture smaller than the encoder".into());
        }
        unsafe {
            let src = self.frame;
            av_frame_unref(src);
            (*src).format = self.input as i32;
            (*src).width = self.width as i32;
            (*src).height = self.height as i32;
            (*src).data[0] = data.as_ptr() as *mut u8;
            (*src).linesize[0] = stride as i32;
            // A reference that frees nothing, so libavcodec takes the frame without copying it. The
            // picture isn't touched again until this call has its packet.
            (*src).buf[0] = av_buffer_create(data.as_ptr() as *mut u8, data.len(), Some(keep), null_mut(), 0);
            let frame = if self.sws.is_null() {
                src
            } else {
                av_frame_make_writable(self.yuv);
                let y = &mut *self.yuv;
                sws_scale(self.sws, (*src).data.as_ptr() as *const *const u8, (*src).linesize.as_ptr(), 0, self.height as i32, y.data.as_ptr(), y.linesize.as_ptr());
                self.yuv
            };
            (*frame).pts = self.pts;
            self.pts += 1;
            (*frame).pict_type = if force_keyframe { AVPictureType::AV_PICTURE_TYPE_I } else { AVPictureType::AV_PICTURE_TYPE_NONE };
            if force_keyframe {
                (*frame).flags |= AV_FRAME_FLAG_KEY as i32;
            } else {
                (*frame).flags &= !(AV_FRAME_FLAG_KEY as i32);
            }
            let r = avcodec_send_frame(self.ctx, frame);
            // The source frame points into `data`: forget it before returning.
            av_frame_unref(src);
            if r < 0 {
                return Err(format!("{}: {}", self.name, err(r)));
            }
            let mut out = Vec::new();
            loop {
                let r = avcodec_receive_packet(self.ctx, self.packet);
                if r == EAGAIN || r == AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    return Err(format!("{}: {}", self.name, err(r)));
                }
                let p = &*self.packet;
                out.extend_from_slice(std::slice::from_raw_parts(p.data, p.size as usize));
                av_packet_unref(self.packet);
            }
            if out.is_empty() {
                return Err(format!("{} held the frame back", self.name));
            }
            Ok(parse_access_unit(Codec::H264, &out))
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if !self.frame.is_null() {
                (*self.frame).data[0] = null_mut();
            }
            av_frame_free(&mut self.frame);
            av_frame_free(&mut self.yuv);
            av_packet_free(&mut self.packet);
            if !self.sws.is_null() {
                sws_freeContext(self.sws);
            }
            avcodec_free_context(&mut self.ctx);
        }
    }
}
