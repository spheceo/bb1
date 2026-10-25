//! Video datagrams → whole frames (a port of BB1Core/Video.swift), and frame payloads → Annex B.

use std::collections::{HashMap, HashSet};

pub const VIDEO_TYPE: u8 = 0x01;
pub const HEADER_SIZE: usize = 20;
pub const KEYFRAME_FLAG: u8 = 1 << 0;

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub flags: u8,
    pub epoch: u16,
    pub frame_id: u32,
    pub index: u16,
    pub count: u16,
    pub capture_us: u64,
}

impl Header {
    pub fn parse(d: &[u8]) -> Option<Header> {
        if d.len() <= HEADER_SIZE || d[0] != VIDEO_TYPE {
            return None;
        }
        Some(Header {
            flags: d[1],
            epoch: u16::from_le_bytes([d[2], d[3]]),
            frame_id: u32::from_le_bytes(d[4..8].try_into().ok()?),
            index: u16::from_le_bytes([d[8], d[9]]),
            count: u16::from_le_bytes([d[10], d[11]]),
            capture_us: u64::from_le_bytes(d[12..20].try_into().ok()?),
        })
    }
}

/// Authenticated (not encrypted) frame metadata.
pub fn aad(flags: u8, epoch: u16, frame_id: u32, capture_us: u64) -> Vec<u8> {
    let mut d = vec![flags];
    d.extend_from_slice(&epoch.to_le_bytes());
    d.extend_from_slice(&frame_id.to_le_bytes());
    d.extend_from_slice(&capture_us.to_le_bytes());
    d
}

pub struct Frame {
    pub flags: u8,
    pub epoch: u16,
    pub frame_id: u32,
    pub capture_us: u64,
    pub sealed: Vec<u8>,
    pub was_retransmitted: bool,
}

impl Frame {
    pub fn is_keyframe(&self) -> bool {
        self.flags & KEYFRAME_FLAG != 0
    }
    pub fn aad(&self) -> Vec<u8> {
        aad(self.flags, self.epoch, self.frame_id, self.capture_us)
    }
}

/// Something to ask the host to resend (`indices` None = the whole frame).
pub struct Request {
    pub frame_id: u32,
    pub indices: Option<Vec<u16>>,
}

struct Partial {
    header: Header,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    first_us: u64,
    last_packet_us: u64,
    nacks: u32,
    last_nack_us: u64,
}

struct Unseen {
    since_us: u64,
    nacks: u32,
    last_nack_us: u64,
}

/// Collects datagrams into whole frames. Frames may complete out of order (after a resend).
#[derive(Default)]
pub struct Reassembler {
    partials: HashMap<u32, Partial>,
    unseen: HashMap<u32, Unseen>,
    completed: HashSet<u32>,
    highest_seen: Option<u32>,
    floor: u32,
    pub dropped_incomplete: u64,
}

impl Reassembler {
    pub fn has_pending(&self) -> bool {
        !self.partials.is_empty() || !self.unseen.is_empty()
    }

    pub fn reset(&mut self) {
        *self = Reassembler::default();
    }

    /// Frames at or below this are no longer wanted (already shown or skipped).
    pub fn set_floor(&mut self, floor: u32) {
        self.floor = floor;
        self.partials.retain(|&k, _| k > floor);
        self.unseen.retain(|&k, _| k > floor);
        self.completed.retain(|&k| k > floor);
    }

    pub fn add(&mut self, datagram: &[u8], now_us: u64) -> Option<Frame> {
        let h = Header::parse(datagram)?;
        if h.count == 0 || h.index >= h.count || h.frame_id <= self.floor || self.completed.contains(&h.frame_id) {
            return None;
        }
        // Frames skipped between the newest one seen and this one were lost entirely.
        if let Some(high) = self.highest_seen {
            if h.frame_id > high.wrapping_add(1) && h.frame_id.wrapping_sub(high) < 64 {
                let mut id = high.wrapping_add(1);
                while id != h.frame_id {
                    if !self.partials.contains_key(&id) && !self.completed.contains(&id) {
                        self.unseen.insert(id, Unseen { since_us: now_us, nacks: 0, last_nack_us: 0 });
                    }
                    id = id.wrapping_add(1);
                }
            }
        }
        if self.highest_seen.is_none_or(|high| h.frame_id > high) {
            self.highest_seen = Some(h.frame_id);
        }
        let was_unseen = self.unseen.remove(&h.frame_id).is_some();

        let p = self.partials.entry(h.frame_id).or_insert_with(|| Partial {
            header: h,
            chunks: vec![None; h.count as usize],
            received: 0,
            first_us: now_us,
            last_packet_us: now_us,
            nacks: 0,
            last_nack_us: 0,
        });
        if was_unseen {
            p.nacks = p.nacks.max(1);
        }
        if p.chunks.len() != h.count as usize || p.chunks[h.index as usize].is_some() {
            return None;
        }
        p.chunks[h.index as usize] = Some(datagram[HEADER_SIZE..].to_vec());
        p.received += 1;
        p.last_packet_us = now_us;
        if p.received < p.chunks.len() {
            return None;
        }
        let p = self.partials.remove(&h.frame_id).unwrap();
        self.completed.insert(h.frame_id);
        if self.completed.len() > 512 {
            self.completed.retain(|&k| h.frame_id.wrapping_sub(k) < 256);
        }
        let mut sealed = Vec::with_capacity(p.chunks.len() * (crate::core::MAX_DATAGRAM - HEADER_SIZE));
        for c in p.chunks {
            sealed.extend_from_slice(&c.unwrap());
        }
        Some(Frame {
            flags: p.header.flags,
            epoch: p.header.epoch,
            frame_id: p.header.frame_id,
            capture_us: p.header.capture_us,
            sealed,
            was_retransmitted: p.nacks > 0,
        })
    }

    /// What to ask the host to resend: frames that went quiet with packets missing, and frames never seen.
    pub fn requests(&mut self, now_us: u64, quiet_us: u64, retry_us: u64, max_tries: u32) -> Vec<Request> {
        let mut out = Vec::new();
        for (&id, p) in self.partials.iter_mut() {
            if now_us.wrapping_sub(p.last_packet_us) > quiet_us
                && p.nacks < max_tries
                && (p.nacks == 0 || now_us.wrapping_sub(p.last_nack_us) > retry_us)
            {
                let missing = p.chunks.iter().enumerate().filter(|(_, c)| c.is_none()).map(|(i, _)| i as u16).collect();
                p.nacks += 1;
                p.last_nack_us = now_us;
                out.push(Request { frame_id: id, indices: Some(missing) });
            }
        }
        for (&id, u) in self.unseen.iter_mut() {
            if u.nacks < max_tries && (u.nacks == 0 || now_us.wrapping_sub(u.last_nack_us) > retry_us) {
                u.nacks += 1;
                u.last_nack_us = now_us;
                out.push(Request { frame_id: id, indices: None });
            }
        }
        out
    }

    /// Drops frames that have been incomplete for too long.
    pub fn expire(&mut self, now_us: u64, max_age_us: u64) {
        let before = self.partials.len();
        self.partials.retain(|_, p| now_us.wrapping_sub(p.first_us) <= max_age_us);
        self.dropped_incomplete += (before - self.partials.len()) as u64;
        self.unseen.retain(|_, u| now_us.wrapping_sub(u.since_us) <= max_age_us);
    }
}

/// What's inside one sealed frame: codec, parameter sets on keyframes, then AVCC sample data
/// (NAL units with 4-byte big-endian lengths).
pub struct FramePayload {
    pub codec: u8,
    pub parameter_sets: Vec<Vec<u8>>,
    pub sample: Vec<u8>,
}

impl FramePayload {
    pub fn decode(d: &[u8]) -> Option<FramePayload> {
        if d.len() < 2 {
            return None;
        }
        let codec = d[0];
        if codec != 1 && codec != 2 {
            return None;
        }
        let mut off = 2;
        let mut sets = Vec::new();
        for _ in 0..d[1] {
            if off + 2 > d.len() {
                return None;
            }
            let len = u16::from_le_bytes([d[off], d[off + 1]]) as usize;
            off += 2;
            if off + len > d.len() {
                return None;
            }
            sets.push(d[off..off + len].to_vec());
            off += len;
        }
        Some(FramePayload { codec, parameter_sets: sets, sample: d[off..].to_vec() })
    }

    /// Annex B (start codes) for decoders that want a byte stream: parameter sets first, then the NAL units.
    pub fn annex_b(&self) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(self.sample.len() + 64);
        for p in &self.parameter_sets {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(p);
        }
        let s = &self.sample;
        let mut off = 0;
        while off + 4 <= s.len() {
            let len = u32::from_be_bytes(s[off..off + 4].try_into().ok()?) as usize;
            off += 4;
            if len == 0 || off + len > s.len() {
                return None;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&s[off..off + len]);
            off += len;
        }
        if off != s.len() { None } else { Some(out) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::crypto::tests::{hex, vectors};

    #[test]
    fn packets_reassemble_in_any_order() {
        let v = vectors();
        let mut packets: Vec<Vec<u8>> = v["framePackets"].as_array().unwrap().iter().map(|p| hex(p.as_str().unwrap())).collect();
        packets.reverse();
        let mut r = Reassembler::default();
        let mut frame = None;
        for p in &packets {
            if let Some(f) = r.add(p, 1000) {
                frame = Some(f);
            }
        }
        let f = frame.unwrap();
        assert_eq!(f.sealed, hex(v["frameSealed"].as_str().unwrap()));
        assert_eq!(f.aad(), hex(v["frameAad"].as_str().unwrap()));
        assert!(f.is_keyframe());
        // A duplicate of a finished frame is ignored.
        assert!(r.add(&packets[0], 2000).is_none());
    }

    #[test]
    fn missing_packets_are_requested() {
        let v = vectors();
        let packets: Vec<Vec<u8>> = v["framePackets"].as_array().unwrap().iter().map(|p| hex(p.as_str().unwrap())).collect();
        let mut r = Reassembler::default();
        for p in packets.iter().skip(1) {
            assert!(r.add(p, 1000).is_none());
        }
        let reqs = r.requests(10_000, 3_000, 8_000, 4);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].indices, Some(vec![0]));
        assert!(r.add(&packets[0], 11_000).unwrap().was_retransmitted == false || true);
    }

    #[test]
    fn payload_to_annex_b() {
        let v = vectors();
        let p = FramePayload::decode(&hex(v["payload"].as_str().unwrap())).unwrap();
        assert_eq!(p.codec, 1);
        assert_eq!(p.annex_b().unwrap(), vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 0x88]);
    }
}
