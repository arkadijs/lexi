//! Call state and the atomics shared between threads (SPEC §4.1).
//!
//! Audio callbacks and the renderer read these without locks; the numeric values of
//! [`CallState`] are the contract with Kotlin's `NativeCallback.onState`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CallState {
    Idle = 0,
    Connecting = 1,
    Live = 2,
    Reconnecting = 3,
    Ended = 4,
    Error = 5,
}

impl CallState {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Idle,
            1 => Self::Connecting,
            2 => Self::Live,
            3 => Self::Reconnecting,
            4 => Self::Ended,
            5 => Self::Error,
            _ => return None,
        })
    }
}

/// Lock-free state shared by the net thread, audio callbacks, renderer and FFI.
#[derive(Debug, Default)]
pub struct Shared {
    mic_level: AtomicU32,
    bot_level: AtomicU32,
    muted: AtomicBool,
    call_state: AtomicU8,
}

impl Shared {
    pub fn set_mic_level(&self, v: f32) {
        self.mic_level
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.mic_level.load(Ordering::Relaxed))
    }
    pub fn set_bot_level(&self, v: f32) {
        self.bot_level
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn bot_level(&self) -> f32 {
        f32::from_bits(self.bot_level.load(Ordering::Relaxed))
    }
    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_call_state(&self, s: CallState) {
        self.call_state.store(s as u8, Ordering::Release);
    }
    pub fn call_state(&self) -> CallState {
        CallState::from_u8(self.call_state.load(Ordering::Acquire)).unwrap_or(CallState::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        for v in 0..=5u8 {
            assert_eq!(CallState::from_u8(v).map(|s| s as u8), Some(v));
        }
        assert_eq!(CallState::from_u8(6), None);
    }

    #[test]
    fn shared_levels_clamp_and_state_defaults_idle() {
        let s = Shared::default();
        assert_eq!(s.call_state(), CallState::Idle);
        s.set_mic_level(3.0);
        s.set_bot_level(-1.0);
        assert_eq!(s.mic_level(), 1.0);
        assert_eq!(s.bot_level(), 0.0);
        s.set_muted(true);
        assert!(s.muted());
        s.set_call_state(CallState::Live);
        assert_eq!(s.call_state(), CallState::Live);
    }
}
