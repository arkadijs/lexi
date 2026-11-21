//! JNI entry points (SPEC §4.2). Android only.
//!
//! Rules (CLAUDE.md): every export is wrapped in `catch_unwind` via [`jni_guard!`]; nothing here
//! is called from audio callbacks; Kotlin callbacks go through one dispatcher thread that owns the
//! callback reference and receives events over a bounded channel (never blocking the sender).
//!
//! Phase 4: the native side is a stub that logs to logcat and drives plausible state transitions
//! so the UI can be exercised. Real call logic arrives in Phase 5, rendering in Phase 6.

use crate::state::{CallState, Shared};
use jni::objects::{Global, JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint};
use jni::{jni_sig, jni_str, EnvUnowned, JValue, JavaVM};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const EVENT_QUEUE: usize = 32;

struct Event {
    state: CallState,
    detail: Option<String>,
}

struct Core {
    tx: SyncSender<Event>,
    shared: Arc<Shared>,
}

static CORE: OnceLock<Core> = OnceLock::new();

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
    match core.tx.try_send(Event {
        state,
        detail: detail.map(str::to_owned),
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => log::warn!("callback queue full; dropped {state:?}"),
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
                        let detail = match &ev.detail {
                            Some(d) => JObject::from(env.new_string(d)?),
                            None => JObject::null(),
                        };
                        env.call_method(
                            &callback,
                            jni_str!("onState"),
                            jni_sig!("(ILjava/lang/String;)V"),
                            &[JValue::Int(ev.state as jint), JValue::Object(&detail)],
                        )?;
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
        log::info!("init ok (stub core, Phase 4)");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_surfaceCreated<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    _surface: JObject<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("surfaceCreated (render arrives in Phase 6)");
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
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_surfaceDestroyed<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("surfaceDestroyed");
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
        // Touch events are frequent; keep this at trace level (filtered out at Info).
        log::trace!("touch action={action} id={pointer_id} x={x} y={y}");
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
        log::trace!("pinch scale={scale} fx={fx} fy={fy}");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_startCall<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    minter_url: JString<'l>,
    install_id_hash: JString<'l>,
) {
    jni_guard!(env, |e| {
        let url = minter_url.try_to_string(e)?;
        // The install hash is pseudonymous but still not logged: only its presence/length.
        let hash_len = install_id_hash.try_to_string(e)?.len();
        log::info!("startCall url={url} install_hash_len={hash_len} (stub)");
        emit(CallState::Connecting, None);
        // Stub: pretend the call connects. Replaced by the real session in Phase 5.
        let _ = std::thread::Builder::new()
            .name("lexi-stub-call".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_millis(700));
                if CORE
                    .get()
                    .is_some_and(|c| c.shared.call_state() == CallState::Connecting)
                {
                    emit(CallState::Live, None);
                }
            });
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_endCall<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("endCall (stub)");
        emit(CallState::Ended, Some("hangup"));
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
        log::info!("onAudioRouteChanged (stub)");
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_lexi_NativeBridge_onNetworkChanged<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
) {
    jni_guard!(env, |_e| {
        log::info!("onNetworkChanged (stub)");
        Ok(())
    })
}
