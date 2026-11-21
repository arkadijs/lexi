//! cpal mic/speaker glue for the desk harness (48 kHz only; Android uses AAudio in Phase 5).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use lexi_core::jitter::ring::PcmConsumer;
use lexi_core::rtc::MicProducer;

type R<T> = Result<T, String>;

fn pick_config(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<SupportedStreamConfig> {
    let mut best: Option<SupportedStreamConfig> = None;
    for c in configs {
        if !matches!(c.sample_format(), SampleFormat::F32 | SampleFormat::I16) {
            continue;
        }
        let Some(cfg) = c.try_with_sample_rate(48_000) else {
            continue;
        };
        // Prefer fewer channels, then i16 (no conversion).
        let better = match &best {
            None => true,
            Some(b) => {
                (cfg.channels(), cfg.sample_format() != SampleFormat::I16)
                    < (b.channels(), b.sample_format() != SampleFormat::I16)
            }
        };
        if better {
            best = Some(cfg);
        }
    }
    best
}

fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0) as i16
}

pub fn start_mic(mut mic: MicProducer) -> R<Stream> {
    let host = cpal::default_host();
    let dev = host
        .default_input_device()
        .ok_or("no input device (use --headless to run without audio hardware)")?;
    let cfg = dev
        .supported_input_configs()
        .map_err(|e| e.to_string())
        .and_then(|it| {
            pick_config(it)
                .ok_or_else(|| "input device does not support 48 kHz i16/f32".to_string())
        })?;
    let ch = usize::from(cfg.channels());
    let fmt = cfg.sample_format();
    let sc: StreamConfig = cfg.into();
    let err = |e: cpal::Error| eprintln!("mic stream error: {e}");
    let mut scratch: Vec<i16> = Vec::new();
    let stream = match fmt {
        SampleFormat::I16 => dev.build_input_stream(
            sc,
            move |data: &[i16], _: &_| {
                scratch.clear();
                scratch.extend(data.chunks(ch).map(|f| f[0]));
                mic.push(&scratch);
            },
            err,
            None,
        ),
        _ => dev.build_input_stream(
            sc,
            move |data: &[f32], _: &_| {
                scratch.clear();
                scratch.extend(data.chunks(ch).map(|f| to_i16(f[0])));
                mic.push(&scratch);
            },
            err,
            None,
        ),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

pub fn start_speaker(mut pcm: PcmConsumer) -> R<Stream> {
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or("no output device")?;
    let cfg = dev
        .supported_output_configs()
        .map_err(|e| e.to_string())
        .and_then(|it| {
            pick_config(it)
                .ok_or_else(|| "output device does not support 48 kHz i16/f32".to_string())
        })?;
    let ch = usize::from(cfg.channels());
    let fmt = cfg.sample_format();
    let sc: StreamConfig = cfg.into();
    let err = |e: cpal::Error| eprintln!("speaker stream error: {e}");
    let mut scratch: Vec<i16> = Vec::new();
    let stream = match fmt {
        SampleFormat::I16 => dev.build_output_stream(
            sc,
            move |out: &mut [i16], _: &_| {
                scratch.resize(out.len() / ch, 0);
                pcm.pull(&mut scratch);
                for (frame, s) in out.chunks_mut(ch).zip(&scratch) {
                    frame.fill(*s);
                }
            },
            err,
            None,
        ),
        _ => dev.build_output_stream(
            sc,
            move |out: &mut [f32], _: &_| {
                scratch.resize(out.len() / ch, 0);
                pcm.pull(&mut scratch);
                for (frame, s) in out.chunks_mut(ch).zip(&scratch) {
                    frame.fill(f32::from(*s) / 32768.0);
                }
            },
            err,
            None,
        ),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}
