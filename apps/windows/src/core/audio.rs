//! Adaptive jitter buffer for the host's sound (a port of AudioJitterBuffer in the Mac client).
//! Packets are stored by sequence number, so late and duplicate copies land in the right place.
//! Playback starts once `target` packets are queued. Running dry raises the target (up to 150 ms);
//! 15 s without trouble lowers it again (down to 30 ms). Too much queued: skip ahead.

use crate::core::crypto::AUDIO_FRAMES_PER_PACKET as FRAMES;
use crate::core::now_us;
use std::sync::Mutex;

const SLOTS: usize = 64;
const MIN_TARGET: usize = 6;
const MAX_TARGET: usize = 30;

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

    fn reset(&mut self) {
        self.slot_valid = [false; SLOTS];
        self.play_seq = None;
        self.highest = None;
        self.play_offset = 0;
        self.buffering = true;
        self.dry_pending = false;
    }
}

impl JitterBuffer {
    /// `pcm`: 240 frames of interleaved little-endian i16 stereo.
    pub fn write(&self, seq: u32, pcm: &[u8]) {
        let mut s = self.inner.lock().unwrap();
        if let Some(p) = s.play_seq {
            if seq.wrapping_sub(p) > 0x8000_0000 {
                return; // already played
            }
            if s.dry_pending {
                s.dry_pending = false;
                if seq.wrapping_sub(p) <= 2 {
                    // The packet we ran out waiting for showed up late: the network needs more cushion.
                    s.underruns += 1;
                    s.target = (s.target + 2).min(MAX_TARGET);
                    s.calm_since_us = now_us();
                } else {
                    s.reset(); // the host paused (it doesn't send silence)
                }
            } else if seq.wrapping_sub(p) >= SLOTS as u32 {
                s.reset();
            }
        }
        let slot = seq as usize % SLOTS;
        if s.slot_valid[slot] && s.slot_seq[slot] == seq {
            return; // duplicate copy
        }
        let base = slot * FRAMES * 2;
        let n = (pcm.len() / 2).min(FRAMES * 2);
        for i in 0..n {
            s.samples[base + i] = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]);
        }
        s.slot_seq[slot] = seq;
        s.slot_valid[slot] = true;
        if s.play_seq.is_none() {
            s.play_seq = Some(seq);
            s.play_offset = 0;
        }
        if s.highest.is_none_or(|h| seq.wrapping_sub(h) < 0x8000_0000) {
            s.highest = Some(seq);
        }
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
            let have = s.slot_valid[slot] && s.slot_seq[slot] == p;
            if !have && s.depth() <= 1 {
                s.buffering = true;
                s.dry_pending = true;
                break;
            }
            let n = (frames - i).min(FRAMES - s.play_offset);
            if have {
                let base = (slot * FRAMES + s.play_offset) * 2;
                out[i * 2..(i + n) * 2].copy_from_slice(&s.samples[base..base + n * 2]);
            } else {
                out[i * 2..(i + n) * 2].fill(0); // lost packet: brief silence
                if s.play_offset == 0 {
                    s.concealed += 1;
                }
            }
            i += n;
            s.play_offset += n;
            if s.play_offset == FRAMES {
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
}
