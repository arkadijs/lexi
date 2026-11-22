//! Lexi core (SPEC §4).
//!
//! `openai`, `rtc`, `codec`, `jitter` and `state` are platform independent and also used by
//! `devtools/desk`. `ffi`, `audio` and `render` are Android-only.

pub mod call;
pub mod codec;
pub mod jitter;
pub mod openai;
pub mod pricing;
pub mod rtc;
pub mod shaderlog;
pub mod state;
pub mod visual;

#[cfg(target_os = "android")]
pub mod audio;
#[cfg(target_os = "android")]
pub mod ffi;
#[cfg(target_os = "android")]
pub mod render;
