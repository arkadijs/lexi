//! AAudio capture and playback (SPEC §4.1, §4.3). Android only.
//!
//! Mic: `INPUT_PRESET_VOICE_COMMUNICATION` (platform AEC/NS where available), 48 kHz mono PCM16,
//! low latency, shared. Speaker: `USAGE_VOICE_COMMUNICATION`, `CONTENT_TYPE_SPEECH`, same format.
//! The callbacks only touch lock-free rings (`rtrb`): no allocation, no locks, no JNI, no logging.
//!
//! The ring endpoints live on the heap, owned by [`AAudioBackend`], and the callbacks get raw
//! pointers to them. That lets the streams be re-opened (route change, device disconnect) while
//! keeping the same rings, which the net thread is still connected to. Soundness rule: a stream is
//! always stopped and closed (callbacks can no longer run) before its endpoint is touched or freed.
//!
//! Rate: 48 kHz is requested and AAudio converts if the hardware runs at another rate. If a device
//! still reports something else after opening, opening fails with an error rather than playing at
//! the wrong speed; a resampler (`rubato`) is only worth adding if a real device ever does that.

use crate::call::AudioBackend;
use crate::jitter::ring::PcmConsumer;
use crate::rtc::MicProducer;
use ndk::audio::{
    AudioCallbackResult, AudioContentType, AudioDirection, AudioFormat, AudioInputPreset,
    AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder, AudioUsage,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const RATE: i32 = 48_000;
/// One Opus frame (20 ms) per callback: 50 wake-ups/s instead of one per tiny HAL burst, which was
/// the second biggest CPU cost of an idle call (SPEC N4).
const FRAMES_PER_CALLBACK: i32 = 960;

/// AAudio streams are documented as thread-safe for control calls; the type just lacks the marker.
struct Stream(AudioStream);
// SAFETY: see above; we only start/stop/close from the supervisor thread anyway.
unsafe impl Send for Stream {}

/// Raw pointer to a ring endpoint, handed to exactly one audio callback.
struct Ptr<T>(*mut T);
// SAFETY: each pointer is used by a single callback thread, and only while its stream is open.
unsafe impl<T> Send for Ptr<T> {}

#[derive(Default)]
pub struct AAudioBackend {
    mic: Option<Box<MicProducer>>,
    pcm: Option<Box<PcmConsumer>>,
    input: Option<Stream>,
    output: Option<Stream>,
    broken: Arc<AtomicBool>,
}

impl std::fmt::Debug for AAudioBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AAudioBackend")
            .field("open", &(self.input.is_some() && self.output.is_some()))
            .finish()
    }
}

impl AAudioBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn close_streams(&mut self) {
        for s in [self.input.take(), self.output.take()]
            .into_iter()
            .flatten()
        {
            let _ = s.0.request_stop();
            drop(s); // AAudioStream_close: no callback runs after this returns
        }
    }

    fn open_streams(&mut self) -> Result<(), String> {
        let (Some(mic), Some(pcm)) = (self.mic.as_mut(), self.pcm.as_mut()) else {
            return Err("audio endpoints missing".into());
        };
        self.broken.store(false, Ordering::Release);
        let mic_ptr = Ptr(&mut **mic as *mut MicProducer);
        let pcm_ptr = Ptr(&mut **pcm as *mut PcmConsumer);

        let broken = Arc::clone(&self.broken);
        let on_err = move || {
            let broken = Arc::clone(&broken);
            Box::new(move |_: &AudioStream, _| broken.store(true, Ordering::Release))
                as Box<dyn FnMut(&AudioStream, ndk::audio::AudioError) + Send>
        };

        let input = AudioStreamBuilder::new()
            .map_err(|e| format!("builder: {e}"))?
            .direction(AudioDirection::Input)
            .sample_rate(RATE)
            .channel_count(1)
            .format(AudioFormat::PCM_I16)
            .performance_mode(AudioPerformanceMode::LowLatency)
            .sharing_mode(AudioSharingMode::Shared)
            .frames_per_data_callback(FRAMES_PER_CALLBACK)
            .input_preset(AudioInputPreset::VoiceCommunication)
            .data_callback(Box::new(move |_s, data, frames| {
                let p = &mic_ptr;
                // SAFETY: AAudio passes `frames` mono i16 samples; `p.0` is valid while the stream is open.
                unsafe {
                    let samples =
                        std::slice::from_raw_parts(data as *const i16, frames.max(0) as usize);
                    (*p.0).push(samples);
                }
                AudioCallbackResult::Continue
            }))
            .error_callback(on_err())
            .open_stream()
            .map_err(|e| format!("open input: {e}"))?;
        let input = Stream(input);

        let output = AudioStreamBuilder::new()
            .map_err(|e| format!("builder: {e}"))?
            .direction(AudioDirection::Output)
            .sample_rate(RATE)
            .channel_count(1)
            .format(AudioFormat::PCM_I16)
            .performance_mode(AudioPerformanceMode::LowLatency)
            .sharing_mode(AudioSharingMode::Shared)
            .frames_per_data_callback(FRAMES_PER_CALLBACK)
            .usage(AudioUsage::VoiceCommunication)
            .content_type(AudioContentType::Speech)
            .data_callback(Box::new(move |_s, data, frames| {
                let p = &pcm_ptr;
                // SAFETY: as above; `pull` zero-fills on underrun.
                unsafe {
                    let out =
                        std::slice::from_raw_parts_mut(data as *mut i16, frames.max(0) as usize);
                    (*p.0).pull(out);
                }
                AudioCallbackResult::Continue
            }))
            .error_callback(on_err())
            .open_stream()
            .map_err(|e| format!("open output: {e}"))?;
        let output = Stream(output);

        for (name, s) in [("input", &input), ("output", &output)] {
            if s.0.sample_rate() != RATE || s.0.channel_count() != 1 {
                return Err(format!(
                    "{name} stream is {} Hz x{} (need 48000 Hz mono)",
                    s.0.sample_rate(),
                    s.0.channel_count()
                ));
            }
        }
        input
            .0
            .request_start()
            .map_err(|e| format!("start input: {e}"))?;
        output
            .0
            .request_start()
            .map_err(|e| format!("start output: {e}"))?;
        self.input = Some(input);
        self.output = Some(output);
        Ok(())
    }
}

impl AudioBackend for AAudioBackend {
    fn attach(&mut self, mic: MicProducer, pcm: PcmConsumer) -> Result<(), String> {
        self.detach();
        self.mic = Some(Box::new(mic));
        self.pcm = Some(Box::new(pcm));
        let r = self.open_streams();
        if r.is_err() {
            self.detach();
        }
        r
    }

    fn restart(&mut self) -> Result<(), String> {
        self.close_streams();
        self.open_streams()
    }

    fn needs_restart(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    fn detach(&mut self) {
        self.close_streams();
        self.mic = None;
        self.pcm = None;
    }
}

impl Drop for AAudioBackend {
    fn drop(&mut self) {
        self.detach();
    }
}
