//! JNI entry points (SPEC §4.2). Android only.
//!
//! Rules (CLAUDE.md): every export is wrapped in `catch_unwind` via [`jni_guard!`]; nothing here
//! is called from audio callbacks; Kotlin callbacks go through one dispatcher thread that owns the
//! callback reference and receives events over a bounded channel (never blocking the sender).
//!
//! Calls run on the supervisor in [`crate::call`] with the AAudio backend; rendering is Phase 6.

use crate::audio::AAudioBackend;
use crate::call::{CallConfig, CallHandle, RealConnector};
use crate::openai::Secret;
use crate::render::{Cmd, Renderer, TouchEv};
use crate::rtc::RtcConfig;
use crate::state::{CallState, Shared};
use jni::objects::{Global, JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint};
use jni::{jni_sig, jni_str, EnvUnowned, JValue, JavaVM};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

const EVENT_QUEUE: usize = 32;

enum Event {
    State {
        state: CallState,
        detail: Option<String>,
    },
    /// Running cost of the call in micro-USD (SPEC §2.6).
    Cost(u64),
}

struct Core {
    tx: SyncSender<Event>,
    shared: Arc<Shared>,
}

static CORE: OnceLock<Core> = OnceLock::new();
/// The renderer, alive while the SurfaceView has a Surface.
static RENDER: Mutex<Option<Renderer>> = Mutex::new(None);
/// The selected visual mode, kept here so a renderer started later begins in the right one.
static VISUAL_MODE: AtomicI32 = AtomicI32::new(crate::render::MODE_FLUID);

fn with_render(f: impl FnOnce(&Renderer)) {
    if let Ok(g) = RENDER.lock() {
        if let Some(r) = g.as_ref() {
            f(r);
        }
    }
}

/// The active (or last) call. Locked only from JNI threads, never from audio callbacks.
static CALL: Mutex<Option<CallHandle>> = Mutex::new(None);

fn with_call(f: impl FnOnce(&CallHandle)) -> bool {
    match CALL.lock() {
        Ok(g) => g.as_ref().map(f).is_some(),
        Err(_) => false,
    }
}

/// Wrap a JNI export body: upgrades the env, catches panics, logs errors, returns `$default`.
macro_rules! jni_guard {
    ($env:ident, |$e:ident| $body:expr) => {{
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            $env.with_env(|$e| -> Result<_, jni::errors::Error> { $body })
                .resolve::<jni::errors::LogErrorAndDefault>()
        }));
        match caught {
            Ok(v) => v,
            Err(_) => {
                log::error!("panic caught at JNI boundary");
                Default::default()
            }
        }
    }};
}

fn emit(state: CallState, detail: Option<&str>) {
    let Some(core) = CORE.get() else {
        log::warn!("emit before init: {state:?}");
        return;
    };
    core.shared.set_call_state(state);
    match core.tx.try_send(Event::State {
        state,
        detail: detail.map(str::to_owned),
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => log::warn!("callback queue full; dropped {state:?}"),
        Err(TrySendError::Disconnected(_)) => log::error!("callback dispatcher is gone"),
    }
}

fn emit_cost(micro_usd: u64) {
    let Some(core) = CORE.get() else { return };
    match core.tx.try_send(Event::Cost(micro_usd)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => log::warn!("callback queue full; dropped a cost update"),
        Err(TrySendError::Disconnected(_)) => log::error!("callback dispatcher is gone"),
    }
}

fn spawn_dispatcher(callback: Global<JObject<'static>>, rx: std::sync::mpsc::Receiver<Event>) {
    let spawned = std::thread::Builder::new()
        .name("lexi-callbacks".into())
        .spawn(move || {
            let Ok(vm) = JavaVM::singleton() else {
                log::error!("no JavaVM; callbacks disabled");
                return;
            };
            while let Ok(ev) = rx.recv() {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
                        match &ev {
                            Event::State { state, detail } => {
                                let detail = match detail {
                                    Some(d) => JObject::from(env.new_string(d)?),
                                    None => JObject::null(),
                                };
                                env.call_method(
                                    &callback,
                                    jni_str!("onState"),
                                    jni_sig!("(ILjava/lang/String;)V"),
                                    &[JValue::Int(*state as jint), JValue::Object(&detail)],
                                )?;
                            }
                            Event::Cost(micro) => {
                                env.call_method(
                                    &callback,
                                    jni_str!("onCost"),
                                    jni_sig!("(J)V"),
                                    &[JValue::Long(i64::try_from(*micro).unwrap_or(i64::MAX))],
                                )?;
                            }
                        }
                        Ok(())
                    })
                }));
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => log::error!("onState callback failed: {e}"),
                    Err(_) => log::error!("panic in callback dispatcher"),
                }
            }
        });
    if let Err(e) = spawned {
        log::error!("cannot spawn callback dispatcher: {e}");
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_init<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    _context: JObject<'l>,
    callback: JObject<'l>,
) {
    jni_guard!(env, |e| {
        android_logger::init_once(
            android_logger::Config::default()
                .with_tag("lexi")
                .with_max_level(log::LevelFilter::Info),
        );
        if CORE.get().is_some() {
            log::info!("init: already initialised");
            return Ok(());
        }
        let cb = e.new_global_ref(&callback)?;
        let (tx, rx) = sync_channel(EVENT_QUEUE);
        spawn_dispatcher(cb, rx);
        let _ = CORE.set(Core {
            tx,
            shared: Arc::new(Shared::default()),
        });
        log::info!("init ok");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_surfaceCreated<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    surface: JObject<'l>,
) {
    jni_guard!(env, |e| {
        let Some(core) = CORE.get() else {
            log::error!("surfaceCreated before init");
            return Ok(());
        };
        // SAFETY: `e` is the live JNIEnv of this native call and `surface` a live android.view.Surface.
        let window = unsafe {
            ndk::native_window::NativeWindow::from_surface(
                e.get_raw().cast(),
                surface.as_raw().cast(),
            )
        };
        let Some(window) = window else {
            log::error!("surfaceCreated: no ANativeWindow");
            return Ok(());
        };
        let mode = VISUAL_MODE.load(Ordering::Relaxed);
        if mode == crate::render::MODE_OFF {
            log::info!("surfaceCreated: visuals are off (debug); no renderer");
            return Ok(());
        }
        match Renderer::start(window, Arc::clone(&core.shared), mode) {
            Ok(r) => {
                let old = RENDER.lock().ok().and_then(|mut g| g.replace(r));
                drop(old); // joins a previous renderer, if any
                log::info!("surfaceCreated: renderer started (mode {mode})");
            }
            Err(err) => log::error!("surfaceCreated: cannot start the render thread: {err}"),
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_surfaceChanged<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    w: jint,
    h: jint,
) {
    jni_guard!(env, |_e| {
        log::info!("surfaceChanged {w}x{h}");
        with_render(|r| r.send(Cmd::Resize));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_surfaceDestroyed<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        // The Surface is invalid once this returns: stop and join the render thread first.
        let renderer = RENDER.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = renderer {
            r.stop();
        }
        log::info!("surfaceDestroyed: renderer stopped");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_setVisualMode<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    mode: jint,
) {
    jni_guard!(env, |_e| {
        log::info!("setVisualMode {mode}");
        VISUAL_MODE.store(mode, Ordering::Relaxed);
        with_render(|r| r.send(Cmd::Mode(mode)));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_touch<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    action: jint,
    pointer_id: jint,
    x: jfloat,
    y: jfloat,
) {
    jni_guard!(env, |_e| {
        with_render(|r| {
            r.send(Cmd::Touch(TouchEv {
                action,
                id: pointer_id,
                x,
                y,
            }))
        });
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_pinch<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    scale: jfloat,
    fx: jfloat,
    fy: jfloat,
) {
    jni_guard!(env, |_e| {
        with_render(|r| r.send(Cmd::Pinch { scale, fx, fy }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_startCall<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    minter_url: JString<'l>,
    install_id_hash: JString<'l>,
    attest_key: JString<'l>,
    voice: JString<'l>,
    model: JString<'l>,
) {
    jni_guard!(env, |e| {
        let url = minter_url.try_to_string(e)?;
        let hash = install_id_hash.try_to_string(e)?;
        let key = attest_key.try_to_string(e)?;
        let voice = voice.try_to_string(e)?;
        let model = model.try_to_string(e)?;
        start_call(url, hash, key, voice, model);
        Ok(())
    })
}

fn start_call(url: String, hash: String, key: String, voice: String, model: String) {
    let Some(core) = CORE.get() else {
        log::error!("startCall before init");
        return;
    };
    if matches!(
        core.shared.call_state(),
        CallState::Connecting | CallState::Live | CallState::Reconnecting
    ) {
        log::warn!("startCall ignored: a call is already active");
        return;
    }
    // Only the URL host is logged; the install hash and attestation key never are.
    log::info!(
        "startCall url={url} attest={} voice={voice} model={model}",
        !key.is_empty()
    );
    let connector = Arc::new(RealConnector {
        minter_url: url,
        install_hash: hash,
        attest_key: (!key.is_empty()).then(|| Secret::new(key)),
        voice: (!voice.is_empty()).then_some(voice),
        model: (!model.is_empty()).then_some(model),
        rtc: RtcConfig::default(),
    });
    let started = CallHandle::start(
        CallConfig::default(),
        connector,
        Box::new(AAudioBackend::new()),
        Arc::clone(&core.shared),
        Box::new(emit),
        Box::new(emit_cost),
    );
    match started {
        Ok(handle) => {
            if let Ok(mut g) = CALL.lock() {
                *g = Some(handle);
            }
        }
        Err(e) => {
            log::error!("cannot start call supervisor: {e}");
            emit(CallState::Error, Some("error"));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_endCall<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("endCall");
        if !with_call(CallHandle::hangup) {
            emit(CallState::Ended, Some("hangup"));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_setMuted<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    muted: jboolean,
) {
    jni_guard!(env, |_e| {
        // Applied by the supervisor within one tick, including before a call exists.
        if let Some(core) = CORE.get() {
            core.shared.set_muted(muted);
        }
        log::info!("setMuted {muted}");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_onAudioRouteChanged<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("onAudioRouteChanged");
        with_call(CallHandle::route_changed);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_onNetworkChanged<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("onNetworkChanged");
        with_call(CallHandle::network_changed);
        Ok(())
    })
}
