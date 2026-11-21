//! Playout buffer: 60 ms target, reordering, loss concealment, flush on barge-in (SPEC §4.1).
//!
//! * [`JitterBuffer`] is sans-I/O and sans-codec: it orders packets by (extended) RTP sequence
//!   number and, driven by a 20 ms clock via [`JitterBuffer::pop`], tells the caller what to
//!   play next: a packet, a packet recovered from the *next* packet's in-band FEC, a concealed
//!   (lost) frame, or an underrun.
//! * [`Playout`] couples it with an [`OpusDecoder`] to produce 960-sample PCM frames.
//! * [`ring`] carries those frames to the audio output callback without locks or allocation.
//!
//! Sequence numbers (not timestamps) drive loss detection, so DTX gaps (the sender simply
//! stops sending, timestamps jump) are not mistaken for loss.

pub mod ring;

use crate::codec::{CodecError, OpusDecoder, FRAME_SAMPLES};
use std::collections::BTreeMap;

/// Target depth: 3 × 20 ms = 60 ms.
pub const TARGET_FRAMES: usize = 3;
/// Hard cap before the oldest packets are discarded to catch up (200 ms).
pub const MAX_FRAMES: usize = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub played: u64,
    pub recovered_fec: u64,
    pub concealed: u64,
    pub underruns: u64,
    pub late_dropped: u64,
    pub overflow_dropped: u64,
    pub duplicates: u64,
}

/// What to play for the current 20 ms tick.
#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    /// Decode this packet normally.
    Packet(Vec<u8>),
    /// The expected packet is missing but the following one is here: recover from its FEC.
    /// The following packet stays queued and will be returned by the next `pop`.
    Fec(Vec<u8>),
    /// Expected packet is missing and unrecoverable: conceal.
    Lost,
    /// Nothing to play (not primed yet, or ran dry): output silence.
    Underrun,
}

#[derive(Debug)]
pub struct JitterBuffer {
    pkts: BTreeMap<u64, Vec<u8>>,
    next: Option<u64>,
    primed: bool,
    target: usize,
    max: usize,
    stats: Stats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new(TARGET_FRAMES, MAX_FRAMES)
    }
}

impl JitterBuffer {
    pub fn new(target_frames: usize, max_frames: usize) -> Self {
        Self {
            pkts: BTreeMap::new(),
            next: None,
            primed: false,
            target: target_frames.max(1),
            max: max_frames.max(target_frames.max(1)),
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn depth(&self) -> usize {
        self.pkts.len()
    }

    /// Insert a packet. `seq` must be the extended (non-wrapping) sequence number.
    pub fn push(&mut self, seq: u64, payload: Vec<u8>) {
        // Only a primed buffer has a playout head that packets can be "late" for.
        if let (true, Some(next)) = (self.primed, self.next) {
            if seq < next {
                self.stats.late_dropped += 1;
                return;
            }
        }
        if self.pkts.insert(seq, payload).is_some() {
            self.stats.duplicates += 1;
        }
        let first = self.pkts.keys().next().copied();
        // Before playout starts, `next` tracks the earliest packet seen.
        if !self.primed {
            self.next = first;
        }
        while self.pkts.len() > self.max {
            if let Some((&oldest, _)) = self.pkts.iter().next() {
                self.pkts.remove(&oldest);
                self.stats.overflow_dropped += 1;
            }
            // Jump the playout head forward to the new oldest packet.
            self.next = self.pkts.keys().next().copied();
        }
    }

    /// Drop everything (barge-in). The next packets re-prime the buffer.
    pub fn flush(&mut self) {
        self.pkts.clear();
        self.next = None;
        self.primed = false;
    }

    /// Call once per 20 ms.
    pub fn pop(&mut self) -> Pop {
        if !self.primed {
            if self.pkts.len() >= self.target {
                self.primed = true;
            } else {
                return Pop::Underrun;
            }
        }
        let Some(next) = self.next else {
            self.primed = false;
            return Pop::Underrun;
        };
        if let Some(p) = self.pkts.remove(&next) {
            self.next = Some(next + 1);
            self.stats.played += 1;
            return Pop::Packet(p);
        }
        if self.pkts.is_empty() {
            // Ran dry (e.g. remote DTX / end of speech): re-buffer before resuming.
            self.primed = false;
            self.next = None;
            self.stats.underruns += 1;
            return Pop::Underrun;
        }
        // Gap with later packets queued: the missing one is not coming in time.
        self.next = Some(next + 1);
        if let Some(following) = self.pkts.get(&(next + 1)) {
            self.stats.recovered_fec += 1;
            Pop::Fec(following.clone())
        } else {
            self.stats.concealed += 1;
            Pop::Lost
        }
    }
}

/// How a 20 ms output frame was produced.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FrameKind {
    Decoded,
    Fec,
    Concealed,
    Silence,
}

/// Jitter buffer + Opus decoder: one call per 20 ms tick yields one PCM frame.
#[derive(Debug)]
pub struct Playout {
    jb: JitterBuffer,
    dec: OpusDecoder,
}

impl Playout {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            jb: JitterBuffer::default(),
            dec: OpusDecoder::new()?,
        })
    }

    pub fn push(&mut self, seq: u64, payload: Vec<u8>) {
        self.jb.push(seq, payload);
    }

    pub fn flush(&mut self) {
        self.jb.flush();
    }

    pub fn stats(&self) -> Stats {
        self.jb.stats()
    }

    /// Fill `out` (exactly [`FRAME_SAMPLES`]) with the next frame.
    pub fn tick(&mut self, out: &mut [i16]) -> Result<FrameKind, CodecError> {
        let out = out
            .get_mut(..FRAME_SAMPLES)
            .ok_or_else(|| CodecError("output too small".into()))?;
        let silence = |out: &mut [i16]| out.fill(0);
        let kind = match self.jb.pop() {
            Pop::Packet(p) => match self.dec.decode(&p, out) {
                Ok(_) => FrameKind::Decoded,
                // Corrupt packet: conceal rather than fail the call.
                Err(_) => self.dec.conceal(out).map(|_| FrameKind::Concealed)?,
            },
            Pop::Fec(next) => match self.dec.decode_fec(&next, out) {
                Ok(_) => FrameKind::Fec,
                Err(_) => self.dec.conceal(out).map(|_| FrameKind::Concealed)?,
            },
            Pop::Lost => self.dec.conceal(out).map(|_| FrameKind::Concealed)?,
            Pop::Underrun => {
                silence(out);
                FrameKind::Silence
            }
        };
        Ok(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Encoded, OpusEncoder};

    fn pkt(n: u8) -> Vec<u8> {
        vec![n; 10]
    }

    fn drain_kinds(jb: &mut JitterBuffer, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| match jb.pop() {
                Pop::Packet(p) => format!("P{}", p[0]),
                Pop::Fec(p) => format!("F{}", p[0]),
                Pop::Lost => "L".into(),
                Pop::Underrun => "U".into(),
            })
            .collect()
    }

    #[test]
    fn waits_for_target_depth_before_playing() {
        let mut jb = JitterBuffer::default();
        jb.push(100, pkt(0));
        jb.push(101, pkt(1));
        assert_eq!(drain_kinds(&mut jb, 1), ["U"]);
        jb.push(102, pkt(2));
        assert_eq!(drain_kinds(&mut jb, 3), ["P0", "P1", "P2"]);
    }

    #[test]
    fn reorders_packets() {
        let mut jb = JitterBuffer::default();
        for (s, n) in [(5, 5), (3, 3), (4, 4), (6, 6)] {
            jb.push(s, pkt(n));
        }
        assert_eq!(drain_kinds(&mut jb, 4), ["P3", "P4", "P5", "P6"]);
    }

    #[test]
    fn single_loss_uses_fec_of_following_packet() {
        let mut jb = JitterBuffer::default();
        for s in [1, 3, 4, 5] {
            jb.push(s, pkt(s as u8));
        }
        jb.push(0, pkt(0));
        // 0 ok, 1 ok, 2 missing -> FEC from 3, then 3 itself, 4, 5
        assert_eq!(
            drain_kinds(&mut jb, 6),
            ["P0", "P1", "F3", "P3", "P4", "P5"]
        );
        assert_eq!(jb.stats().recovered_fec, 1);
    }

    #[test]
    fn burst_loss_conceals() {
        let mut jb = JitterBuffer::default();
        for s in [0, 1, 4, 5, 6] {
            jb.push(s, pkt(s as u8));
        }
        // 2 and 3 lost: 2 has no following packet -> L; 3 recoverable via 4's FEC.
        assert_eq!(
            drain_kinds(&mut jb, 7),
            ["P0", "P1", "L", "F4", "P4", "P5", "P6"]
        );
        let s = jb.stats();
        assert_eq!((s.concealed, s.recovered_fec), (1, 1));
    }

    #[test]
    fn late_and_duplicate_packets_are_dropped() {
        let mut jb = JitterBuffer::default();
        for s in 0..4 {
            jb.push(s, pkt(s as u8));
        }
        jb.pop();
        jb.pop();
        jb.push(0, pkt(9)); // late
        jb.push(3, pkt(9)); // duplicate
        let s = jb.stats();
        assert_eq!((s.late_dropped, s.duplicates), (1, 1));
    }

    #[test]
    fn running_dry_rebuffers() {
        let mut jb = JitterBuffer::default();
        for s in 0..3 {
            jb.push(s, pkt(s as u8));
        }
        assert_eq!(drain_kinds(&mut jb, 4), ["P0", "P1", "P2", "U"]);
        // speech resumes (sequence continues; DTX gap is not loss)
        jb.push(3, pkt(3));
        assert_eq!(drain_kinds(&mut jb, 1), ["U"], "must re-prime");
        jb.push(4, pkt(4));
        jb.push(5, pkt(5));
        assert_eq!(drain_kinds(&mut jb, 3), ["P3", "P4", "P5"]);
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn overflow_catches_up() {
        let mut jb = JitterBuffer::new(3, 5);
        for s in 0..8 {
            jb.push(s, pkt(s as u8));
        }
        assert_eq!(jb.depth(), 5);
        assert_eq!(jb.stats().overflow_dropped, 3);
        assert_eq!(drain_kinds(&mut jb, 1), ["P3"]);
    }

    #[test]
    fn flush_discards_everything_and_reprimes() {
        let mut jb = JitterBuffer::default();
        for s in 0..5 {
            jb.push(s, pkt(s as u8));
        }
        jb.pop();
        jb.flush();
        assert_eq!(jb.depth(), 0);
        assert_eq!(drain_kinds(&mut jb, 1), ["U"]);
        for s in 100..103 {
            jb.push(s, pkt(s as u8));
        }
        assert_eq!(drain_kinds(&mut jb, 3), ["P100", "P101", "P102"]);
    }

    /// End to end with real Opus: 100 frames, 10% random-ish loss and local reordering.
    #[test]
    fn simulated_jitter_and_loss_keep_audio_flowing() {
        let mut enc = OpusEncoder::new().unwrap();
        enc.disable_dtx(); // this test is about loss/reordering, not silence suppression
        let mut play = Playout::new().unwrap();
        let mut packets = vec![];
        for f in 0..100usize {
            let pcm: Vec<i16> = (0..FRAME_SAMPLES)
                .map(|i| {
                    let t = (f * FRAME_SAMPLES + i) as f32 / 48000.0;
                    (6000.0 * (2.0 * std::f32::consts::PI * 300.0 * t).sin()) as i16
                })
                .collect();
            if let Encoded::Packet { data, .. } = enc.encode(&pcm).unwrap() {
                packets.push((f as u64, data));
            }
        }
        assert_eq!(packets.len(), 100);
        // deterministic "network": drop every 10th (not adjacent), swap pairs (20,21),(50,51)
        let mut wire: Vec<(u64, Vec<u8>)> =
            packets.into_iter().filter(|(s, _)| s % 10 != 7).collect();
        for pair in [(20u64, 21u64), (50, 51)] {
            let a = wire.iter().position(|(s, _)| *s == pair.0).unwrap();
            let b = wire.iter().position(|(s, _)| *s == pair.1).unwrap();
            wire.swap(a, b);
        }
        let mut out = vec![0i16; FRAME_SAMPLES];
        let mut kinds = vec![];
        let mut it = wire.into_iter();
        let mut loud_after_prime = 0;
        // one packet arrives per tick (same cadence), playout runs on the same clock
        for tick in 0..110 {
            if let Some((s, d)) = it.next() {
                play.push(s, d);
            }
            let k = play.tick(&mut out).unwrap();
            if tick > 10 && out.iter().any(|&x| x.abs() > 500) {
                loud_after_prime += 1;
            }
            kinds.push(k);
        }
        let st = play.stats();
        assert_eq!(st.concealed + st.recovered_fec, 10, "{st:?}");
        assert_eq!(
            st.recovered_fec, 10,
            "single losses should all be FEC-recovered: {st:?}"
        );
        assert!(
            loud_after_prime > 80,
            "audio kept flowing: {loud_after_prime}"
        );
        assert_eq!(
            st.late_dropped, 0,
            "reordering within depth must not drop: {st:?}"
        );
    }
}
