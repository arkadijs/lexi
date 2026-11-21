//! Lock-free, allocation-free PCM transport between the net thread and an audio callback.
//!
//! Safe to use from a real-time audio callback: `PcmConsumer::pull` and `PcmProducer::push`
//! never allocate, lock, or call into the OS. `flush` is cross-thread via an atomic generation
//! counter (the producer cannot clear a SPSC ring itself).

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::codec::FRAME_SAMPLES;

/// If the consumer is more than this many frames behind (clock drift), it skips ahead.
const MAX_BACKLOG_FRAMES: usize = 6;
const BACKLOG_TARGET_FRAMES: usize = 2;

pub fn pcm_ring(capacity_frames: usize) -> (PcmProducer, PcmConsumer) {
    let (p, c) = RingBuffer::new(capacity_frames.max(MAX_BACKLOG_FRAMES + 1) * FRAME_SAMPLES);
    let gen = Arc::new(AtomicU32::new(0));
    (
        PcmProducer {
            p,
            gen: Arc::clone(&gen),
        },
        PcmConsumer { c, gen, seen: 0 },
    )
}

#[derive(Debug)]
pub struct PcmProducer {
    p: Producer<i16>,
    gen: Arc<AtomicU32>,
}

impl PcmProducer {
    /// Push samples; returns how many were accepted (the rest are dropped when full).
    pub fn push(&mut self, samples: &[i16]) -> usize {
        let n = samples.len().min(self.p.slots());
        if n == 0 {
            return 0;
        }
        if let Ok(chunk) = self.p.write_chunk_uninit(n) {
            chunk.fill_from_iter(samples[..n].iter().copied());
            n
        } else {
            0
        }
    }

    /// Ask the consumer to discard everything buffered so far (barge-in).
    pub fn flush(&self) {
        self.gen.fetch_add(1, Ordering::Release);
    }
}

#[derive(Debug)]
pub struct PcmConsumer {
    c: Consumer<i16>,
    gen: Arc<AtomicU32>,
    seen: u32,
}

impl PcmConsumer {
    fn discard(&mut self, keep: usize) {
        let avail = self.c.slots();
        let drop_n = avail.saturating_sub(keep);
        if drop_n > 0 {
            if let Ok(chunk) = self.c.read_chunk(drop_n) {
                chunk.commit_all();
            }
        }
    }

    /// Fill `out` with buffered samples; the remainder is zeroed (underrun → silence).
    /// Returns the number of real samples written.
    pub fn pull(&mut self, out: &mut [i16]) -> usize {
        let g = self.gen.load(Ordering::Acquire);
        if g != self.seen {
            self.seen = g;
            self.discard(0);
        } else if self.c.slots() > MAX_BACKLOG_FRAMES * FRAME_SAMPLES {
            self.discard(BACKLOG_TARGET_FRAMES * FRAME_SAMPLES);
        }
        let n = out.len().min(self.c.slots());
        if n > 0 {
            if let Ok(chunk) = self.c.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                out[..a.len()].copy_from_slice(a);
                out[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
        }
        out[n..].fill(0);
        n
    }

    pub fn buffered(&self) -> usize {
        self.c.slots()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_samples_in_order_and_zero_fills() {
        let (mut p, mut c) = pcm_ring(8);
        assert_eq!(p.push(&[1, 2, 3, 4]), 4);
        let mut out = [9i16; 6];
        assert_eq!(c.pull(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4, 0, 0]);
    }

    #[test]
    fn flush_discards_buffered_audio() {
        let (mut p, mut c) = pcm_ring(8);
        p.push(&vec![7; FRAME_SAMPLES * 2]);
        p.flush();
        let mut out = [1i16; 4];
        assert_eq!(c.pull(&mut out), 0);
        assert_eq!(out, [0; 4]);
        p.push(&[5, 5]);
        assert_eq!(c.pull(&mut out), 2);
    }

    #[test]
    fn skips_ahead_when_backlog_too_large() {
        let (mut p, mut c) = pcm_ring(10);
        p.push(&vec![1; FRAME_SAMPLES * 8]);
        let mut out = vec![0i16; FRAME_SAMPLES];
        c.pull(&mut out);
        assert!(c.buffered() <= BACKLOG_TARGET_FRAMES * FRAME_SAMPLES);
    }

    #[test]
    fn drops_when_full_instead_of_blocking() {
        let (mut p, _c) = pcm_ring(7);
        let cap = 7 * FRAME_SAMPLES;
        assert_eq!(p.push(&vec![1; cap + 100]), cap);
        assert_eq!(p.push(&[1]), 0);
    }
}
