//! Adaptive jitter buffer for the host's sound (a port of AudioJitterBuffer in the Mac client).
//! Packets are stored by sequence number, so late and duplicate copies land in the right place.
//! Playback starts once `target` packets are queued. Running dry raises the target (up to 150 ms);
//! 15 s without trouble lowers it again (down to 30 ms). Too much queued: skip ahead.
//! A lost packet is filled with the one before it played backwards and fading out (it joins that one
//! without a jump), and the packet after fades in over 1 ms: a gap is a soft dip, not a click.
//! Opus frames (from a PC host) are kept as they came and decoded in order as they're played, so the
//! decoder sees one stream; a missing one is Opus's own concealment instead.

use crate::core::crypto::AUDIO_FRAMES_PER_PACKET as FRAMES;
use crate::core::now_us;
use std::sync::Mutex;

const SLOTS: usize = 64;
const MIN_TARGET: usize = 6;
const MAX_TARGET: usize = 30;
/// Frames the packet after a filled-in gap takes to fade in (1 ms).
const FADE_IN: usize = 48;

struct Inner {
    samples: Vec<i16>, // SLOTS * FRAMES * 2, interleaved stereo
    slot_seq: [u32; SLOTS],
    slot_valid: [bool; SLOTS],
    play_seq: Option<u32>,
    play_offset: usize,
    highest: Option<u32>,
    buffering: bool,
    target: usize,
    calm_since_us: u64,
    dry_pending: bool,
    concealed: u64,
    underruns: u64,
    /// The last packet played from real sound, and whether it was the one just before the play head.
    last_pcm: Vec<i16>,
    last_good: bool,
    /// The play head is on the first packet after a filled-in gap.
    fade_in: bool,
    /// Per slot: an Opus frame not decoded yet (its bytes in `opus_frames`).
    slot_opus: [bool; SLOTS],
    opus_frames: Vec<Vec<u8>>,
    /// Made with the first Opus frame. `opus_playing`: the last packet played was Opus.
    decoder: Option<opus::Decoder>,
    opus_playing: bool,
}

pub struct JitterBuffer {
    inner: Mutex<Inner>,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        JitterBuffer {
            inner: Mutex::new(Inner {
                samples: vec![0; SLOTS * FRAMES * 2],
                slot_seq: [0; SLOTS],
                slot_valid: [false; SLOTS],
                play_seq: None,
                play_offset: 0,
                highest: None,
                buffering: true,
                target: 10,
                calm_since_us: 0,
                dry_pending: false,
                concealed: 0,
                underruns: 0,
                last_pcm: vec![0; FRAMES * 2],
                last_good: false,
                fade_in: false,
                slot_opus: [false; SLOTS],
                opus_frames: (0..SLOTS).map(|_| Vec::with_capacity(512)).collect(),
                decoder: None,
                opus_playing: false,
            }),
        }
    }
}

impl Inner {
    fn depth(&self) -> usize {
        match (self.play_seq, self.highest) {
            (Some(p), Some(h)) if h.wrapping_sub(p) < 0x8000_0000 => (h.wrapping_sub(p) + 1) as usize,
            _ => 0,
        }
    }

    fn invalidate(&mut self, seq: u32) {
        let slot = seq as usize % SLOTS;
        if self.slot_seq[slot] == seq {
            self.slot_valid[slot] = false;
        }
    }

    /// Where packet `seq` goes, or None if it's too late or a copy of one we have.
    fn admit(&mut self, seq: u32) -> Option<usize> {
        if let Some(p) = self.play_seq {
            if seq.wrapping_sub(p) > 0x8000_0000 {
                return None; // already played
            }
            if self.dry_pending {
                self.dry_pending = false;
                if seq.wrapping_sub(p) <= 2 {
                    // The packet we ran out waiting for showed up late: the network needs more cushion.
                    self.underruns += 1;
                    self.target = (self.target + 2).min(MAX_TARGET);
                    self.calm_since_us = now_us();
                } else {
                    self.reset(); // the host paused (it doesn't send silence)
                }
            } else if seq.wrapping_sub(p) >= SLOTS as u32 {
                self.reset();
            }
        }
        let slot = seq as usize % SLOTS;
        if self.slot_valid[slot] && self.slot_seq[slot] == seq {
            return None; // duplicate copy
        }
        Some(slot)
    }

    /// Packet `seq` is in `slot` now.
    fn stored(&mut self, seq: u32, slot: usize) {
        self.slot_seq[slot] = seq;
        self.slot_valid[slot] = true;
        if self.play_seq.is_none() {
            self.play_seq = Some(seq);
            self.play_offset = 0;
        }
        if self.highest.is_none_or(|h| seq.wrapping_sub(h) < 0x8000_0000) {
            self.highest = Some(seq);
        }
    }

    /// The Opus frame in `slot` (or, with None, the decoder's guess at a missing one) as PCM there.
    fn decode_opus(&mut self, slot: usize, frame: Option<usize>) -> bool {
        let Some(decoder) = self.decoder.as_mut() else { return false };
        let base = slot * FRAMES * 2;
        let input: &[u8] = match frame {
            Some(f) => &self.opus_frames[f],
            None => &[],
        };
        let ok = matches!(decoder.decode(input, &mut self.samples[base..base + FRAMES * 2], false), Ok(n) if n == FRAMES);
        if !ok {
            self.samples[base..base + FRAMES * 2].fill(0);
        }
        self.slot_opus[slot] = false;
        ok
    }

    fn reset(&mut self) {
        self.slot_valid = [false; SLOTS];
        self.play_seq = None;
        self.highest = None;
        self.play_offset = 0;
        self.buffering = true;
        self.dry_pending = false;
        self.last_good = false;
        self.fade_in = false;
    }
}

impl JitterBuffer {
    /// `pcm`: 240 frames of interleaved little-endian i16 stereo.
    pub fn write(&self, seq: u32, pcm: &[u8]) {
        let mut s = self.inner.lock().unwrap();
        let Some(slot) = s.admit(seq) else { return };
        let base = slot * FRAMES * 2;
        let n = (pcm.len() / 2).min(FRAMES * 2);
        for i in 0..n {
            s.samples[base + i] = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]);
        }
        s.slot_opus[slot] = false;
        s.stored(seq, slot);
    }

    /// One 5 ms Opus frame (decoded when it's played).
    pub fn write_opus(&self, seq: u32, frame: &[u8]) {
        let mut s = self.inner.lock().unwrap();
        if s.decoder.is_none() {
            match opus::Decoder::new(48_000, opus::Channels::Stereo) {
                Ok(d) => s.decoder = Some(d),
                Err(e) => {
                    crate::logi!("sound: no Opus decoder ({e})");
                    return;
                }
            }
        }
        let Some(slot) = s.admit(seq) else { return };
        s.opus_frames[slot].clear();
        s.opus_frames[slot].extend_from_slice(frame);
        s.slot_opus[slot] = true;
        s.stored(seq, slot);
    }

    /// Fills `out` (interleaved stereo i16) completely; silence where there's nothing to play.
    pub fn read(&self, out: &mut [i16]) {
        let mut s = self.inner.lock().unwrap();
        let frames = out.len() / 2;
        let now = now_us();
        if s.calm_since_us == 0 {
            s.calm_since_us = now;
        }
        let mut i = 0;
        if s.buffering && s.depth() >= s.target {
            s.buffering = false;
        }
        if !s.buffering && s.depth() > s.target + 10 {
            if let Some(h) = s.highest {
                let new_play = h.wrapping_sub(s.target as u32).wrapping_add(1);
                while let Some(p) = s.play_seq {
                    if p == new_play {
                        break;
                    }
                    s.invalidate(p);
                    s.play_seq = Some(p.wrapping_add(1));
                }
                s.play_offset = 0;
            }
        }
        while i < frames && !s.buffering {
            let Some(p) = s.play_seq else {
                s.buffering = true;
                break;
            };
            let slot = p as usize % SLOTS;
            let mut have = s.slot_valid[slot] && s.slot_seq[slot] == p;
            if !have && s.depth() <= 1 {
                s.buffering = true;
                s.dry_pending = true;
                break;
            }
            let off = s.play_offset;
            if off == 0 {
                if have && s.slot_opus[slot] {
                    s.decode_opus(slot, Some(slot));
                    s.opus_playing = true;
                } else if have {
                    s.opus_playing = false;
                } else if s.opus_playing && s.decode_opus(slot, None) {
                    // Missing in an Opus stream: the decoder fills it in (and it's a packet we have now,
                    // so a late copy is ignored).
                    s.concealed += 1;
                    s.slot_seq[slot] = p;
                    s.slot_valid[slot] = true;
                    have = true;
                }
            }
            let n = (frames - i).min(FRAMES - s.play_offset);
            if have {
                let base = (slot * FRAMES + off) * 2;
                out[i * 2..(i + n) * 2].copy_from_slice(&s.samples[base..base + n * 2]);
                if s.fade_in {
                    for j in 0..n.min(FADE_IN.saturating_sub(off)) {
                        let g = (off + j) as f32 / FADE_IN as f32;
                        for c in 0..2 {
                            let k = (i + j) * 2 + c;
                            out[k] = (out[k] as f32 * g) as i16;
                        }
                    }
                }
            } else {
                // Lost: the packet before, backwards and fading to nothing (silence if that one was lost too).
                if s.last_good {
                    for j in 0..n {
                        let pos = off + j;
                        let g = 1.0 - (pos + 1) as f32 / FRAMES as f32;
                        for c in 0..2 {
                            out[(i + j) * 2 + c] = (s.last_pcm[(FRAMES - 1 - pos) * 2 + c] as f32 * g) as i16;
                        }
                    }
                } else {
                    out[i * 2..(i + n) * 2].fill(0);
                }
                if off == 0 {
                    s.concealed += 1;
                }
            }
            i += n;
            s.play_offset += n;
            if s.play_offset == FRAMES {
                if have {
                    let base = slot * FRAMES * 2;
                    let Inner { samples, last_pcm, .. } = &mut *s;
                    last_pcm.copy_from_slice(&samples[base..base + FRAMES * 2]);
                }
                s.last_good = have;
                s.fade_in = !have;
                s.invalidate(p);
                s.play_seq = Some(p.wrapping_add(1));
                s.play_offset = 0;
            }
        }
        out[i * 2..].fill(0);
        if now - s.calm_since_us > 15_000_000 && s.target > MIN_TARGET {
            s.target -= 1;
            s.calm_since_us = now;
        }
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().reset();
    }

    /// (concealed, underruns, buffer ms), reset each call.
    pub fn take_stats(&self) -> (u64, u64, usize) {
        let mut s = self.inner.lock().unwrap();
        let r = (s.concealed, s.underruns, s.target * 5);
        s.concealed = 0;
        s.underruns = 0;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_in_order_after_buffering() {
        let b = JitterBuffer::default();
        for seq in 0..12u32 {
            let pcm: Vec<u8> = (0..FRAMES * 2).flat_map(|_| (seq as i16).to_le_bytes()).collect();
            b.write(seq, &pcm);
        }
        let mut out = vec![0i16; FRAMES * 2];
        b.read(&mut out);
        assert!(out.iter().all(|&s| s == 0)); // seq 0
        b.read(&mut out);
        assert!(out.iter().all(|&s| s == 1));
    }

    /// A 440 Hz tone through Opus with every 7th frame lost: it plays all the way, with the decoder
    /// filling the gaps (no silent packets), and copies that arrive twice are ignored.
    #[test]
    fn opus_frames_play_in_order_and_gaps_are_concealed() {
        let mut enc = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
        let b = JitterBuffer::default();
        let mut t = 0usize;
        let mut frames = Vec::new();
        for _ in 0..40u32 {
            let pcm: Vec<i16> = (0..FRAMES)
                .flat_map(|_| {
                    t += 1;
                    let v = ((t as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 8000.0) as i16;
                    [v, v]
                })
                .collect();
            let mut out = [0u8; 1000];
            let n = enc.encode(&pcm, &mut out).unwrap();
            frames.push(out[..n].to_vec());
        }
        let send = |seq: usize| {
            if seq % 7 != 3 {
                b.write_opus(seq as u32, &frames[seq]);
                b.write_opus(seq as u32, &frames[seq]); // a copy
            }
        };
        (0..10).for_each(send);
        let mut out = vec![0i16; FRAMES * 2];
        let mut silent = 0;
        for i in 0..30 {
            send(10 + i); // one arrives for each one played
            b.read(&mut out);
            if out.iter().all(|&x| x.unsigned_abs() < 50) {
                silent += 1;
            }
        }
        // (The very first frame is the decoder starting up.)
        assert!(silent <= 1, "{silent} silent packets");
        assert_eq!(b.take_stats().0, 4, "seq 3, 10, 17, 24 concealed");
    }

    #[test]
    fn a_lost_packet_fades_out_instead_of_cutting_to_silence() {
        let b = JitterBuffer::default();
        for seq in (0..14u32).filter(|&s| s != 1) {
            let pcm: Vec<u8> = (0..FRAMES).flat_map(|f| [(f as i16 * 10).to_le_bytes(), (f as i16 * 10).to_le_bytes()]).flatten().collect();
            b.write(seq, &pcm);
        }
        let mut out = vec![0i16; FRAMES * 2];
        b.read(&mut out); // seq 0: 0, 10, … 2390
        assert_eq!(out[(FRAMES - 1) * 2], 2390);
        b.read(&mut out); // seq 1 is missing
        assert!(out[0] > 2300, "starts where seq 0 ended, got {}", out[0]);
        assert_eq!(out[(FRAMES - 1) * 2], 0, "and fades to nothing");
        b.read(&mut out); // seq 2 fades in
        assert_eq!(out[0], 0);
        assert_eq!(out[(FRAMES - 1) * 2], 2390);
        assert_eq!(b.take_stats().0, 1);
    }
}
