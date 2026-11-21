//! libopus encode/decode: 48 kHz mono, 20 ms frames (SPEC §4.3).
//!
//! Platform independent; used by `rtc`, `jitter` and `devtools/desk`.

use opus::{Application, Bitrate, Channels, Decoder, Encoder};
use std::fmt;

pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per 20 ms mono frame at 48 kHz.
pub const FRAME_SAMPLES: usize = 960;
/// RTP timestamp increment per frame (Opus RTP clock is always 48 kHz).
pub const RTP_TICKS_PER_FRAME: u64 = FRAME_SAMPLES as u64;
/// Upper bound for one encoded packet (RFC 6716 recommends 1275 for a single frame).
const MAX_PACKET: usize = 1275;
/// Packets of at most this size are DTX / comfort-noise updates; senders skip them (RFC 7587 §3.1.3).
const DTX_MAX_LEN: usize = 2;

#[derive(Debug)]
pub struct CodecError(pub String);

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "opus: {}", self.0)
    }
}
impl std::error::Error for CodecError {}

impl From<opus::Error> for CodecError {
    fn from(e: opus::Error) -> Self {
        Self(e.to_string())
    }
}

/// Monotonic RTP media-time counter: +960 ticks per 20 ms frame, regardless of whether the
/// frame was actually transmitted (DTX frames still advance the clock).
#[derive(Debug, Default, Clone)]
pub struct RtpClock {
    ticks: u64,
}

impl RtpClock {
    pub fn new() -> Self {
        Self::default()
    }
    /// Timestamp (in 48 kHz ticks) of the *next* frame, then advance by one frame.
    pub fn next_frame(&mut self) -> u64 {
        let t = self.ticks;
        self.ticks += RTP_TICKS_PER_FRAME;
        t
    }
}

/// Result of encoding one frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Encoded {
    /// Packet to send, with the RTP timestamp of this frame.
    Packet { ts: u64, data: Vec<u8> },
    /// DTX: nothing to send for this frame (the clock still advanced).
    Silent { ts: u64 },
}

pub struct OpusEncoder {
    enc: Encoder,
    clock: RtpClock,
    buf: Vec<u8>,
}

impl fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("clock", &self.clock)
            .finish()
    }
}

impl OpusEncoder {
    /// VOIP application, 32 kbps, in-band FEC and DTX on (SPEC §4.3).
    pub fn new() -> Result<Self, CodecError> {
        let mut enc = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        enc.set_bitrate(Bitrate::Bits(32_000))?;
        enc.set_inband_fec(true)?;
        // FEC is only produced if the encoder expects loss.
        enc.set_packet_loss_perc(10)?;
        enc.set_dtx(true)?;
        Ok(Self {
            enc,
            clock: RtpClock::new(),
            buf: vec![0; MAX_PACKET],
        })
    }

    /// Encode exactly one 20 ms frame ([`FRAME_SAMPLES`] samples).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Encoded, CodecError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(CodecError(format!(
                "frame must be {FRAME_SAMPLES} samples, got {}",
                pcm.len()
            )));
        }
        let ts = self.clock.next_frame();
        let n = self.enc.encode(pcm, &mut self.buf)?;
        if n <= DTX_MAX_LEN {
            Ok(Encoded::Silent { ts })
        } else {
            Ok(Encoded::Packet {
                ts,
                data: self.buf[..n].to_vec(),
            })
        }
    }
}

pub struct OpusDecoder {
    dec: Decoder,
}

impl fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpusDecoder")
    }
}

impl OpusDecoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            dec: Decoder::new(SAMPLE_RATE, Channels::Mono)?,
        })
    }

    /// Decode a packet into `out` (must hold at least one frame). Returns samples decoded.
    pub fn decode(&mut self, packet: &[u8], out: &mut [i16]) -> Result<usize, CodecError> {
        Ok(self.dec.decode(packet, out, false)?)
    }

    /// Recover the frame *before* `next_packet` from its in-band FEC data.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [i16]) -> Result<usize, CodecError> {
        let out = out
            .get_mut(..FRAME_SAMPLES)
            .ok_or_else(|| CodecError("output too small".into()))?;
        Ok(self.dec.decode(next_packet, out, true)?)
    }

    /// Packet-loss concealment for one lost frame.
    pub fn conceal(&mut self, out: &mut [i16]) -> Result<usize, CodecError> {
        let out = out
            .get_mut(..FRAME_SAMPLES)
            .ok_or_else(|| CodecError("output too small".into()))?;
        Ok(self.dec.decode(&[], out, false)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frame: usize, amp: f32) -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (frame * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                (amp * (2.0 * std::f32::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn rtp_clock_advances_960_per_frame() {
        let mut c = RtpClock::new();
        assert_eq!(
            [c.next_frame(), c.next_frame(), c.next_frame()],
            [0, 960, 1920]
        );
    }

    #[test]
    fn encoder_timestamps_advance_even_for_dtx_frames() {
        let mut e = OpusEncoder::new().unwrap();
        let mut stamps = vec![];
        for f in 0..60 {
            // 10 frames of tone, then silence long enough for DTX to engage.
            let pcm = if f < 10 {
                tone(f, 8000.0)
            } else {
                vec![0; FRAME_SAMPLES]
            };
            match e.encode(&pcm).unwrap() {
                Encoded::Packet { ts, .. } | Encoded::Silent { ts } => stamps.push(ts),
            }
        }
        for (i, w) in stamps.windows(2).enumerate() {
            assert_eq!(w[1] - w[0], 960, "frame {i}");
        }
    }

    #[test]
    fn dtx_suppresses_packets_during_silence() {
        let mut e = OpusEncoder::new().unwrap();
        let mut sent_in_silence = 0;
        for f in 0..100 {
            let pcm = if f < 10 {
                tone(f, 8000.0)
            } else {
                vec![0; FRAME_SAMPLES]
            };
            if let Encoded::Packet { .. } = e.encode(&pcm).unwrap() {
                if f >= 50 {
                    sent_in_silence += 1;
                }
            }
        }
        assert!(
            sent_in_silence < 10,
            "DTX should suppress most silent frames, sent {sent_in_silence}"
        );
    }

    #[test]
    fn roundtrip_preserves_tone_energy() {
        let mut e = OpusEncoder::new().unwrap();
        let mut d = OpusDecoder::new().unwrap();
        let mut out = vec![0i16; FRAME_SAMPLES];
        let mut energy = 0f64;
        for f in 0..20 {
            let Encoded::Packet { data, .. } = e.encode(&tone(f, 8000.0)).unwrap() else {
                panic!("tone must not be DTX");
            };
            assert_eq!(d.decode(&data, &mut out).unwrap(), FRAME_SAMPLES);
            if f >= 5 {
                energy += out.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>();
            }
        }
        let rms = (energy / (15.0 * FRAME_SAMPLES as f64)).sqrt();
        assert!(rms > 3000.0, "decoded rms {rms}");
    }

    #[test]
    fn plc_and_fec_produce_audio() {
        let mut e = OpusEncoder::new().unwrap();
        let mut d = OpusDecoder::new().unwrap();
        let mut out = vec![0i16; FRAME_SAMPLES];
        let mut pk = vec![];
        for f in 0..10 {
            if let Encoded::Packet { data, .. } = e.encode(&tone(f, 8000.0)).unwrap() {
                pk.push(data);
            }
        }
        for p in &pk[..5] {
            d.decode(p, &mut out).unwrap();
        }
        // packet 5 lost: recover from packet 6's FEC
        assert_eq!(d.decode_fec(&pk[6], &mut out).unwrap(), FRAME_SAMPLES);
        assert!(out.iter().any(|&s| s != 0));
        d.decode(&pk[6], &mut out).unwrap();
        assert_eq!(d.conceal(&mut out).unwrap(), FRAME_SAMPLES);
    }

    #[test]
    fn rejects_wrong_frame_size() {
        let mut e = OpusEncoder::new().unwrap();
        assert!(e.encode(&[0; 100]).is_err());
    }
}
