//! GPU visuals (SPEC §2.3): EGL on the `SurfaceView`'s window, GLES 3.0 through `glow`, one render
//! thread whose frames are paced by `AChoreographer`. Android only.
//!
//! Lifetime: [`Renderer::start`] on `surfaceCreated`, [`Renderer::stop`] on `surfaceDestroyed`. `stop`
//! joins the thread, so the window is never used after the Surface is gone and nothing renders while
//! the app is not visible or the screen is off (the Surface does not exist then).

mod egl;
mod fluid;
mod fractal;
mod gl;
mod gputimer;

use crate::state::Shared;
use crate::visual::{visual_level, Change, Envelope, QualityController};
use ndk::looper::{ForeignLooper, ThreadLooper};
use ndk::native_window::NativeWindow;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const MODE_FLUID: i32 = 0;
pub const MODE_FRACTAL: i32 = 1;
/// Debug/measurement only (`am start --es visuals off`): no renderer at all, a black surface.
pub const MODE_OFF: i32 = 2;
/// `touch` action code sent by Kotlin for a double tap; other actions are Android `MotionEvent` codes.
pub const ACTION_DOUBLE_TAP: i32 = 1000;

/// What the UI thread tells the render thread.
#[derive(Debug)]
pub enum Cmd {
    /// The surface size may have changed.
    Resize,
    Mode(i32),
    Touch(TouchEv),
    Pinch {
        scale: f32,
        fx: f32,
        fy: f32,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct TouchEv {
    pub action: i32,
    pub id: i32,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug)]
/// Per-frame inputs for a scene. `mic` / `bot` are already smoothed (0..1).
pub struct FrameIn {
    pub dt: f32,
    pub time: f32,
    pub mic: f32,
    pub bot: f32,
    pub w: i32,
    pub h: i32,
}

pub trait Scene {
    fn name(&self) -> &'static str;
    /// Number of quality levels (0 = best).
    fn levels(&self) -> usize;
    fn resize(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String>;
    fn set_quality(&mut self, gl: &glow::Context, level: usize) -> Result<(), String>;
    fn frame(&mut self, gl: &glow::Context, f: &FrameIn);
    fn touch(&mut self, ev: &TouchEv, w: i32, h: i32);
    fn pinch(&mut self, scale: f32, fx: f32, fy: f32, w: i32, h: i32);
    fn destroy(&mut self, gl: &glow::Context);
}

#[derive(Debug)]
pub struct Renderer {
    tx: Sender<Cmd>,
    stop: Arc<AtomicBool>,
    looper: Arc<Mutex<Option<ForeignLooper>>>,
    join: Option<JoinHandle<()>>,
}

impl Renderer {
    pub fn start(window: NativeWindow, shared: Arc<Shared>, mode: i32) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let looper: Arc<Mutex<Option<ForeignLooper>>> = Arc::default();
        let (s2, l2) = (Arc::clone(&stop), Arc::clone(&looper));
        let join = std::thread::Builder::new()
            .name("lexi-render".into())
            .spawn(move || thread_main(window, shared, rx, s2, l2, mode))?;
        Ok(Self {
            tx,
            stop,
            looper,
            join: Some(join),
        })
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
        if matches!(self.looper.lock().map(|l| l.is_some()), Ok(true)) {
            // Commands are drained once per frame; no wake-up needed, frames keep coming.
        }
    }

    /// Stop rendering and wait for the thread (it frees every GL object and the EGL surface).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(g) = self.looper.lock() {
            if let Some(l) = g.as_ref() {
                l.wake();
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Ctx {
    egl: egl::Egl,
    gl: glow::Context,
    rx: Receiver<Cmd>,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    choreographer: *mut ndk_sys::AChoreographer,
    mode: i32,
    scene: Option<Box<dyn Scene>>,
    size: (i32, i32),
    mic: Envelope,
    bot: Envelope,
    quality: QualityController,
    /// The bot is currently speaking (hysteresis on its level), for the log.
    bot_speaking: bool,
    gpu: gputimer::GpuTimer,
    last_ns: Option<i64>,
    start_ns: i64,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    frames: u32,
    sum: f32,
    worst: f32,
    /// Loudest smoothed bot / mic level seen in the window (the instantaneous value is almost always 0).
    bot_peak: f32,
    mic_peak: f32,
    since: Option<Instant>,
}

fn thread_main(
    window: NativeWindow,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    stop: Arc<AtomicBool>,
    looper_slot: Arc<Mutex<Option<ForeignLooper>>>,
    mode: i32,
) {
    let looper = ThreadLooper::prepare();
    if let Ok(mut slot) = looper_slot.lock() {
        *slot = ForeignLooper::for_thread();
    }
    let egl = match egl::Egl::new(&window) {
        Ok(e) => e,
        Err(e) => {
            log::error!("render: EGL setup failed: {e}");
            return;
        }
    };
    // SAFETY: `proc_address` returns pointers EGL resolved for the current ES 3.0 context.
    let gl = unsafe { glow::Context::from_loader_function(|s| egl.proc_address(s)) };
    log_gl_info(&gl);
    // SAFETY: AChoreographer_getInstance needs a thread with a looper, which `prepare` just gave us.
    let choreographer = unsafe { ndk_sys::AChoreographer_getInstance() };
    if choreographer.is_null() {
        log::error!("render: no Choreographer");
        return;
    }
    let gpu = gputimer::GpuTimer::new(&gl);
    let mut ctx = Box::new(Ctx {
        egl,
        gl,
        rx,
        stop: Arc::clone(&stop),
        shared,
        choreographer,
        mode,
        scene: None,
        size: (0, 0),
        mic: Envelope::new(0.08, 0.35),
        bot: Envelope::new(0.04, 0.22),
        quality: QualityController::new(4),
        bot_speaking: false,
        gpu,
        last_ns: None,
        start_ns: 0,
        stats: Stats::default(),
    });
    ctx.on_resize();
    let data = (&mut *ctx as *mut Ctx).cast::<c_void>();
    // SAFETY: `ctx` outlives every callback: callbacks only run inside `poll_once` below, on this
    // thread, and `ctx` is dropped after the loop ends.
    unsafe { ndk_sys::AChoreographer_postFrameCallback64(choreographer, Some(on_frame), data) };
    log::info!("render: started");
    while !stop.load(Ordering::SeqCst) {
        let _ = looper.poll_once();
    }
    ctx.destroy();
    log::info!("render: stopped");
}

unsafe extern "C" fn on_frame(frame_time_ns: i64, data: *mut c_void) {
    // A panic must not unwind into the Choreographer.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `data` is the `Ctx` posted by `thread_main`, alive and used only on this thread.
        let ctx = unsafe { &mut *data.cast::<Ctx>() };
        if ctx.stop.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = ctx.frame(frame_time_ns) {
            log::error!("render: stopping after error: {e}");
            ctx.stop.store(true, Ordering::SeqCst);
            return;
        }
        // SAFETY: as above; re-arm for the next vsync.
        unsafe {
            ndk_sys::AChoreographer_postFrameCallback64(ctx.choreographer, Some(on_frame), data)
        };
    }));
}

fn log_gl_info(gl: &glow::Context) {
    use glow::HasContext;
    // SAFETY: simple string queries on the current context.
    unsafe {
        log::info!(
            "render: GL {} | {} | {}",
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VENDOR)
        );
    }
}

impl Ctx {
    fn make_scene(&mut self) -> Result<Box<dyn Scene>, String> {
        if self.mode != MODE_FRACTAL {
            match fluid::Fluid::new(&self.gl) {
                Ok(f) => return Ok(Box::new(f)),
                // e.g. no half-float render targets on this GPU: the fractal still works.
                Err(e) => {
                    log::warn!("render: fluid unavailable ({e}); showing the fractal instead")
                }
            }
        }
        Ok(Box::new(fractal::Fractal::new(&self.gl)?))
    }

    fn on_resize(&mut self) {
        let size = self.egl.size();
        if size.0 <= 0 || size.1 <= 0 {
            return;
        }
        if self.scene.is_none() {
            match self.make_scene() {
                Ok(s) => {
                    log::info!("render: scene {} ({} quality levels)", s.name(), s.levels());
                    self.scene = Some(s);
                }
                Err(e) => {
                    log::error!("render: cannot create scene: {e}");
                    self.stop.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
        if size != self.size {
            self.size = size;
            if let Some(s) = &mut self.scene {
                if let Err(e) = s.resize(&self.gl, size.0, size.1) {
                    log::error!("render: resize failed: {e}");
                    self.stop.store(true, Ordering::SeqCst);
                }
            }
            log::info!("render: surface {}x{}", size.0, size.1);
        }
    }

    fn frame(&mut self, now_ns: i64) -> Result<(), String> {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Resize => self.on_resize(),
                Cmd::Mode(m) => {
                    if m != self.mode {
                        self.mode = m;
                        if let Some(mut s) = self.scene.take() {
                            s.destroy(&self.gl);
                        }
                        self.size = (0, 0);
                        self.on_resize();
                    }
                }
                Cmd::Touch(ev) => {
                    if let Some(s) = &mut self.scene {
                        s.touch(&ev, self.size.0, self.size.1);
                    }
                }
                Cmd::Pinch { scale, fx, fy } => {
                    if let Some(s) = &mut self.scene {
                        s.pinch(scale, fx, fy, self.size.0, self.size.1);
                    }
                }
            }
        }
        if self.start_ns == 0 {
            self.start_ns = now_ns;
        }
        let dt = self
            .last_ns
            .map_or(1.0 / 60.0, |l| ((now_ns - l) as f32 / 1e9).clamp(0.0, 0.05));
        let interval = self.last_ns.map_or(0.0, |l| (now_ns - l) as f32 / 1e9);
        self.last_ns = Some(now_ns);
        let mic = self.mic.update(self.shared.mic_level(), dt);
        let bot = self.bot.update(self.shared.bot_level(), dt);
        if !self.bot_speaking && bot > 0.12 {
            self.bot_speaking = true;
            log::info!("render: bot started speaking (level {bot:.2})");
        } else if self.bot_speaking && bot < 0.04 {
            self.bot_speaking = false;
            log::info!("render: bot stopped speaking");
        }
        self.stats.bot_peak = self.stats.bot_peak.max(bot);
        self.stats.mic_peak = self.stats.mic_peak.max(mic);
        let f = FrameIn {
            dt,
            time: (now_ns - self.start_ns) as f32 / 1e9,
            mic: visual_level(mic),
            bot: visual_level(bot),
            w: self.size.0,
            h: self.size.1,
        };
        self.gpu.begin(&self.gl);
        if let Some(s) = &mut self.scene {
            s.frame(&self.gl, &f);
        }
        self.gpu.end(&self.gl);
        self.egl.swap()?;
        self.gpu.collect(&self.gl);

        if let Some(change) = self.quality.frame(interval) {
            let level = self.quality.level();
            log::info!(
                "render: quality {} -> level {level}",
                if change == Change::Lower {
                    "lowered"
                } else {
                    "raised"
                }
            );
            if let Some(s) = &mut self.scene {
                s.set_quality(&self.gl, level.min(s.levels() - 1))?;
            }
        }
        self.log_stats(interval);
        Ok(())
    }

    /// Frame-interval statistics, one line every 5 s (cheap enough to leave on).
    fn log_stats(&mut self, interval: f32) {
        let st = &mut self.stats;
        let since = *st.since.get_or_insert_with(Instant::now);
        if interval > 0.0 {
            st.frames += 1;
            st.sum += interval;
            st.worst = st.worst.max(interval);
        }
        if since.elapsed() >= Duration::from_secs(5) && st.frames > 0 {
            let avg = st.sum / st.frames as f32;
            log::info!(
                "render: {} frames, avg {:.1} ms ({:.0} fps), worst {:.1} ms, level {}, peak bot {:.2} mic {:.2}",
                st.frames,
                avg * 1000.0,
                1.0 / avg,
                st.worst * 1000.0,
                self.quality.level(),
                st.bot_peak,
                st.mic_peak
            );
            *st = Stats {
                since: Some(Instant::now()),
                ..Stats::default()
            };
            if let Some(g) = self.gpu.take_stats() {
                log::info!(
                    "render: gpu {:.2} ms/frame avg = {:.0}% of the {:.1} ms frame period, worst {:.2} ms ({} timed frames)",
                    g.avg_ms,
                    g.avg_ms / (avg * 1000.0) * 100.0,
                    avg * 1000.0,
                    g.max_ms,
                    g.frames
                );
            }
        }
    }

    fn destroy(&mut self) {
        self.gpu.destroy(&self.gl);
        if let Some(mut s) = self.scene.take() {
            s.destroy(&self.gl);
        }
    }
}
