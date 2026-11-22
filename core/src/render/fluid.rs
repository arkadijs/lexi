//! Stable Fluids (Jos Stam) on the GPU (SPEC §2.3): advection, vorticity confinement, divergence, Jacobi
//! pressure solve, gradient subtraction and dye advection, rendered as drifting colour bands.
//!
//! Motion: **calm by default**: a few slow emitters wander across the screen, and the user's voice only
//! nudges their strength. The bot's voice (`bot`, 0..1) adds turbulence, raises the vorticity and distorts and
//! shimmers the picture in proportion to its loudness; when the bot is silent it settles back to calm.

use super::gl::{self, Prog, Target, R16F, RG16F, RGBA16F};
use super::{FrameIn, Scene, TouchEv, ACTION_DOUBLE_TAP};
use crate::visual::{grid_size, hsv, FLUID_LEVELS};

const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_MOVE: i32 = 2;
const ACTION_CANCEL: i32 = 3;
const ACTION_POINTER_DOWN: i32 = 5;
const ACTION_POINTER_UP: i32 = 6;

/// Velocity is in grid cells per second; a touch drag of `d` (fraction of the screen) pushes at `d * TOUCH_FORCE`.
const TOUCH_FORCE: f32 = 6000.0;
const VELOCITY_DISSIPATION: f32 = 0.2;
const DYE_DISSIPATION: f32 = 0.9;
const PRESSURE_DECAY: f32 = 0.8;
/// The simulation runs at this fraction of real time: 10x slower and calmer than the first version (first 5x,
/// then 2x more, both requested after trying it on the phone). Slowing only `dt` would let ink and forces pile up (their damping is per
/// simulated second too), so the continuous sources are scaled by the same factor to keep the picture's
/// character while the motion slows. Touch strokes keep full strength: painting stays immediate, the ink
/// then drifts slowly.
pub const SIM_SPEED: f32 = 0.1;
/// At a tenth of real time the ink would take about ten times longer to fill an empty screen, so a new simulation
/// first runs this many steps of normal-speed dynamics (3 s of simulated time) and only then settles into the
/// slow pace: the picture is already developed when it first appears.
const PREROLL_STEPS: u32 = 180;

struct Pingpong {
    a: Target,
    b: Target,
    flip: bool,
}

impl Pingpong {
    fn new(gl: &glow::Context, w: i32, h: i32, f: gl::Format) -> Result<Self, String> {
        Ok(Self {
            a: Target::new(gl, w, h, f)?,
            b: Target::new(gl, w, h, f)?,
            flip: false,
        })
    }
    fn read(&self) -> &Target {
        if self.flip {
            &self.b
        } else {
            &self.a
        }
    }
    fn write(&self) -> &Target {
        if self.flip {
            &self.a
        } else {
            &self.b
        }
    }
    fn swap(&mut self) {
        self.flip = !self.flip;
    }
    fn destroy(&self, gl: &glow::Context) {
        self.a.destroy(gl);
        self.b.destroy(gl);
    }
}

struct Progs {
    advect: Prog,
    splat: Prog,
    curl: Prog,
    vorticity: Prog,
    divergence: Prog,
    pressure: Prog,
    gradsub: Prog,
    scale: Prog,
    display: Prog,
}

struct Grids {
    vel: Pingpong,
    pressure: Pingpong,
    div: Target,
    curl: Target,
    dye: Pingpong,
    sim: (i32, i32),
}

impl Grids {
    fn new(gl: &glow::Context, screen: (i32, i32), level: usize) -> Result<Self, String> {
        let (long, dye_scale, _) = FLUID_LEVELS[level.min(FLUID_LEVELS.len() - 1)];
        let sim = grid_size(screen.0, screen.1, long);
        let dye = (
            ((screen.0 as f32 * dye_scale) as i32).max(16),
            ((screen.1 as f32 * dye_scale) as i32).max(16),
        );
        Ok(Self {
            vel: Pingpong::new(gl, sim.0, sim.1, RG16F)?,
            pressure: Pingpong::new(gl, sim.0, sim.1, R16F)?,
            div: Target::new(gl, sim.0, sim.1, R16F)?,
            curl: Target::new(gl, sim.0, sim.1, R16F)?,
            dye: Pingpong::new(gl, dye.0, dye.1, RGBA16F)?,
            sim,
        })
    }

    fn destroy(&self, gl: &glow::Context) {
        self.vel.destroy(gl);
        self.pressure.destroy(gl);
        self.div.destroy(gl);
        self.curl.destroy(gl);
        self.dye.destroy(gl);
    }
}

/// A force (`dx`, `dy` in cells/s) and an ink colour injected at a point (`x`, `y` in 0..1, y up).
struct Splat {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    color: [f32; 3],
    radius: f32,
}

pub struct Fluid {
    progs: Progs,
    grids: Option<Grids>,
    level: usize,
    screen: (i32, i32),
    hue: f32,
    /// Active pointers: (id, last x, last y) in pixels.
    pointers: Vec<(i32, f32, f32)>,
    splats: Vec<Splat>,
    rng: u32,
}

impl Fluid {
    /// Fails (so the caller can fall back to the fractal) if float render targets are unavailable.
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        for (name, f) in [("RGBA16F", RGBA16F), ("RG16F", RG16F), ("R16F", R16F)] {
            Target::new(gl, 4, 4, f)
                .map(|t| t.destroy(gl))
                .map_err(|e| format!("{name} render targets unsupported: {e}"))?;
        }
        let p = |name, src| Prog::new(gl, name, src);
        let progs = Progs {
            advect: p("advect", include_str!("shaders/advect.frag"))?,
            splat: p("splat", include_str!("shaders/splat.frag"))?,
            curl: p("curl", include_str!("shaders/curl.frag"))?,
            vorticity: p("vorticity", include_str!("shaders/vorticity.frag"))?,
            divergence: p("divergence", include_str!("shaders/divergence.frag"))?,
            pressure: p("pressure", include_str!("shaders/pressure.frag"))?,
            gradsub: p("gradsub", include_str!("shaders/gradsub.frag"))?,
            scale: p("scale", include_str!("shaders/scale.frag"))?,
            display: p("display", include_str!("shaders/display.frag"))?,
        };
        Ok(Self {
            progs,
            grids: None,
            level: 0,
            screen: (0, 0),
            hue: 0.58,
            pointers: Vec::new(),
            splats: Vec::new(),
            rng: 0x9E37_79B9,
        })
    }

    fn rand(&mut self) -> f32 {
        // xorshift32
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    fn rebuild(&mut self, gl: &glow::Context) -> Result<(), String> {
        if let Some(g) = self.grids.take() {
            g.destroy(gl);
        }
        let g = Grids::new(gl, self.screen, self.level)?;
        log::info!(
            "fluid: level {} sim {}x{} dye {}x{} jacobi {}",
            self.level,
            g.sim.0,
            g.sim.1,
            g.dye.read().w,
            g.dye.read().h,
            FLUID_LEVELS[self.level.min(FLUID_LEVELS.len() - 1)].2
        );
        self.grids = Some(g);
        self.preroll(gl);
        Ok(())
    }

    /// Run [`PREROLL_STEPS`] steps of normal-speed dynamics so the first picture is already developed.
    fn preroll(&mut self, gl: &glow::Context) {
        let started = std::time::Instant::now();
        for step in 0..PREROLL_STEPS {
            // emitter paths advance at the slow pace; sources and the clock run at normal speed
            self.ambient(step as f32 / 60.0 * SIM_SPEED, 0.0, 1.0);
            self.apply_splats(gl);
            self.step(gl, 1.0 / 60.0, 8.0);
        }
        // SAFETY: waits for the queued work, so the pre-roll cost lands here and not in the first frame.
        unsafe {
            use glow::HasContext;
            gl.finish();
        }
        log::info!(
            "fluid: pre-roll {PREROLL_STEPS} steps in {} ms",
            started.elapsed().as_millis()
        );
    }

    /// Slow wandering emitters: the calm baseline. The user's voice only nudges their strength.
    fn ambient(&mut self, t: f32, mic: f32, gain: f32) {
        for k in 0..3 {
            let ph = k as f32 * 2.1;
            let a = t * 0.13 + ph * 1.7 + 0.5 * (t * 0.07 + ph).sin();
            let b = t * 0.11 + ph * 1.3;
            let (x, y) = (0.5 + 0.34 * a.sin(), 0.5 + 0.38 * b.cos());
            let strength = 55.0 * (1.0 + 0.7 * mic) * gain;
            let color = hsv(
                self.hue + k as f32 * 0.18,
                0.75,
                0.06 * (1.0 + 0.8 * mic) * gain,
            );
            self.splats.push(Splat {
                x,
                y,
                dx: a.cos() * strength,
                dy: -b.sin() * strength,
                color,
                radius: 0.004,
            });
        }
    }

    /// The bot's voice stirs the flow: random punches whose number and strength follow `bot`.
    fn turbulence(&mut self, f: &FrameIn) {
        let n = (f.bot * 5.0 + self.rand()) as i32;
        for _ in 0..n {
            let (x, y) = (0.15 + 0.7 * self.rand(), 0.2 + 0.6 * self.rand());
            let ang = self.rand() * std::f32::consts::TAU;
            let mag = (180.0 + 420.0 * self.rand()) * f.bot;
            // the punch keeps its strength (the voice must visibly stir the flow); its ink is scaled like the rest
            let color = hsv(
                self.hue + 0.5 * self.rand(),
                0.7,
                (0.10 + 0.25 * f.bot) * SIM_SPEED,
            );
            self.splats.push(Splat {
                x,
                y,
                dx: ang.cos() * mag,
                dy: ang.sin() * mag,
                color,
                radius: 0.0022,
            });
        }
    }

    fn apply_splats(&mut self, gl: &glow::Context) {
        let Some(g) = self.grids.as_mut() else {
            self.splats.clear();
            return;
        };
        let aspect = self.screen.0 as f32 / self.screen.1.max(1) as f32;
        let p = &mut self.progs.splat;
        for s in self.splats.drain(..) {
            for (field, color) in [(&mut g.vel, [s.dx, s.dy, 0.0]), (&mut g.dye, s.color)] {
                field.write().bind(gl);
                p.use_it(gl);
                p.tex(gl, "u_base", 0, field.read().tex);
                p.f2(gl, "u_res", field.read().w as f32, field.read().h as f32);
                p.f2(gl, "u_point", s.x, s.y);
                p.f3(gl, "u_color", color[0], color[1], color[2]);
                p.f1(gl, "u_radius", s.radius);
                p.f1(gl, "u_aspect", aspect);
                gl::draw(gl);
                field.swap();
            }
        }
    }

    fn step(&mut self, gl: &glow::Context, dt: f32, curl: f32) {
        let iters = FLUID_LEVELS[self.level.min(FLUID_LEVELS.len() - 1)].2;
        let (Some(g), p) = (self.grids.as_mut(), &mut self.progs) else {
            return;
        };
        let (vw, vh) = (g.sim.0 as f32, g.sim.1 as f32);
        let texel = (1.0 / vw, 1.0 / vh);

        g.curl.bind(gl);
        p.curl.use_it(gl);
        p.curl.tex(gl, "u_vel", 0, g.vel.read().tex);
        p.curl.f2(gl, "u_texel", texel.0, texel.1);
        gl::draw(gl);

        g.vel.write().bind(gl);
        p.vorticity.use_it(gl);
        p.vorticity.tex(gl, "u_vel", 0, g.vel.read().tex);
        p.vorticity.tex(gl, "u_curl", 1, g.curl.tex);
        p.vorticity.f2(gl, "u_texel", texel.0, texel.1);
        p.vorticity.f1(gl, "u_strength", curl);
        p.vorticity.f1(gl, "u_dt", dt);
        gl::draw(gl);
        g.vel.swap();

        g.div.bind(gl);
        p.divergence.use_it(gl);
        p.divergence.tex(gl, "u_vel", 0, g.vel.read().tex);
        p.divergence.f2(gl, "u_texel", texel.0, texel.1);
        gl::draw(gl);

        g.pressure.write().bind(gl);
        p.scale.use_it(gl);
        p.scale.tex(gl, "u_tex", 0, g.pressure.read().tex);
        p.scale.f2(gl, "u_res", vw, vh);
        p.scale.f1(gl, "u_value", PRESSURE_DECAY);
        gl::draw(gl);
        g.pressure.swap();

        p.pressure.use_it(gl);
        p.pressure.f2(gl, "u_texel", texel.0, texel.1);
        for _ in 0..iters {
            g.pressure.write().bind(gl);
            p.pressure.tex(gl, "u_pressure", 0, g.pressure.read().tex);
            p.pressure.tex(gl, "u_div", 1, g.div.tex);
            gl::draw(gl);
            g.pressure.swap();
        }

        g.vel.write().bind(gl);
        p.gradsub.use_it(gl);
        p.gradsub.tex(gl, "u_pressure", 0, g.pressure.read().tex);
        p.gradsub.tex(gl, "u_vel", 1, g.vel.read().tex);
        p.gradsub.f2(gl, "u_texel", texel.0, texel.1);
        gl::draw(gl);
        g.vel.swap();

        // velocity advects itself, then carries the dye
        g.vel.write().bind(gl);
        p.advect.use_it(gl);
        p.advect.tex(gl, "u_vel", 0, g.vel.read().tex);
        p.advect.tex(gl, "u_src", 1, g.vel.read().tex);
        p.advect.f2(gl, "u_dst_res", vw, vh);
        p.advect.f2(gl, "u_vel_texel", texel.0, texel.1);
        p.advect.f1(gl, "u_dt", dt);
        p.advect.f1(gl, "u_dissipation", VELOCITY_DISSIPATION);
        gl::draw(gl);
        g.vel.swap();

        g.dye.write().bind(gl);
        p.advect.tex(gl, "u_vel", 0, g.vel.read().tex);
        p.advect.tex(gl, "u_src", 1, g.dye.read().tex);
        p.advect.f2(
            gl,
            "u_dst_res",
            g.dye.read().w as f32,
            g.dye.read().h as f32,
        );
        p.advect.f1(gl, "u_dissipation", DYE_DISSIPATION);
        gl::draw(gl);
        g.dye.swap();
    }
}

impl Scene for Fluid {
    fn name(&self) -> &'static str {
        "fluid"
    }

    fn levels(&self) -> usize {
        FLUID_LEVELS.len()
    }

    fn resize(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        self.screen = (w, h);
        self.rebuild(gl)
    }

    fn set_quality(&mut self, gl: &glow::Context, level: usize) -> Result<(), String> {
        self.level = level;
        if self.screen.0 > 0 {
            self.rebuild(gl)?;
        }
        Ok(())
    }

    fn frame(&mut self, gl: &glow::Context, f: &FrameIn) {
        if self.grids.is_none() {
            return;
        }
        let dt = f.dt.clamp(0.001, 1.0 / 30.0) * SIM_SPEED;
        self.hue = (self.hue + dt * 0.012).rem_euclid(1.0);
        self.ambient(f.time * SIM_SPEED, f.mic, SIM_SPEED);
        self.turbulence(f);
        self.apply_splats(gl);
        // calm vorticity, stronger with the bot's voice
        self.step(gl, dt, 8.0 + 42.0 * f.bot);

        let Some(g) = self.grids.as_ref() else { return };
        // SAFETY: plain GL state calls on the current context.
        unsafe {
            use glow::HasContext;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, f.w, f.h);
        }
        let d = &mut self.progs.display;
        d.use_it(gl);
        d.tex(gl, "u_dye", 0, g.dye.read().tex);
        d.f2(gl, "u_res", f.w as f32, f.h as f32);
        d.f1(gl, "u_time", f.time);
        d.f1(gl, "u_drift", f.time * SIM_SPEED);
        d.f1(gl, "u_bot", f.bot);
        gl::draw(gl);
    }

    fn touch(&mut self, ev: &TouchEv, w: i32, h: i32) {
        let (wf, hf) = (w.max(1) as f32, h.max(1) as f32);
        match ev.action {
            ACTION_DOWN | ACTION_POINTER_DOWN => {
                self.pointers.retain(|p| p.0 != ev.id);
                self.pointers.push((ev.id, ev.x, ev.y));
            }
            ACTION_MOVE => {
                let hue = self.hue;
                if let Some(p) = self.pointers.iter_mut().find(|p| p.0 == ev.id) {
                    let (dx, dy) = ((ev.x - p.1) / wf, -(ev.y - p.2) / hf);
                    p.1 = ev.x;
                    p.2 = ev.y;
                    if dx != 0.0 || dy != 0.0 {
                        let color = hsv(hue + (ev.x / wf) * 0.3, 0.8, 0.5);
                        self.splats.push(Splat {
                            x: ev.x / wf,
                            y: 1.0 - ev.y / hf,
                            dx: dx * TOUCH_FORCE,
                            dy: dy * TOUCH_FORCE,
                            color,
                            radius: 0.0035,
                        });
                    }
                }
            }
            ACTION_UP | ACTION_POINTER_UP | ACTION_CANCEL => self.pointers.retain(|p| p.0 != ev.id),
            ACTION_DOUBLE_TAP => {
                // a burst of colour where you tapped
                let color = hsv(self.hue, 0.8, 0.9);
                self.splats.push(Splat {
                    x: ev.x / wf,
                    y: 1.0 - ev.y / hf,
                    dx: 0.0,
                    dy: 0.0,
                    color,
                    radius: 0.012,
                });
            }
            _ => {}
        }
    }

    fn pinch(&mut self, _scale: f32, _fx: f32, _fy: f32, _w: i32, _h: i32) {}

    fn destroy(&mut self, gl: &glow::Context) {
        if let Some(g) = self.grids.take() {
            g.destroy(gl);
        }
        for p in [
            &self.progs.advect,
            &self.progs.splat,
            &self.progs.curl,
            &self.progs.vorticity,
            &self.progs.divergence,
            &self.progs.pressure,
            &self.progs.gradsub,
            &self.progs.scale,
            &self.progs.display,
        ] {
            p.destroy(gl);
        }
    }
}
