//! Julia-set explorer (SPEC §2.3): pan, pinch, double-tap reset; calm by default, shimmering and
//! warping in proportion to the bot's voice. Rendered at a reduced scale into an offscreen target and
//! stretched to the screen; adaptive quality lowers that scale.

use super::gl::{self, Prog, Target, RGBA8};
use super::{Scene, TouchEv, ACTION_DOUBLE_TAP};
use crate::visual::FractalView;

const FRAG: &str = include_str!("shaders/fractal.frag");
const BLIT: &str = include_str!("shaders/blit.frag");
/// Render scale per quality level (fraction of the screen resolution).
const SCALES: [f32; 4] = [0.6, 0.5, 0.4, 0.3];

const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_MOVE: i32 = 2;
const ACTION_CANCEL: i32 = 3;
const ACTION_POINTER_DOWN: i32 = 5;
const ACTION_POINTER_UP: i32 = 6;

pub struct Fractal {
    fractal: Prog,
    blit: Prog,
    target: Option<Target>,
    view: FractalView,
    level: usize,
    screen: (i32, i32),
    /// Active pointers: (id, last x, last y).
    pointers: Vec<(i32, f32, f32)>,
}

impl Fractal {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        Ok(Self {
            fractal: Prog::new(gl, "fractal", FRAG)?,
            blit: Prog::new(gl, "blit", BLIT)?,
            target: None,
            view: FractalView::default(),
            level: 0,
            screen: (0, 0),
            pointers: Vec::new(),
        })
    }

    fn rebuild_target(&mut self, gl: &glow::Context) -> Result<(), String> {
        if let Some(t) = self.target.take() {
            t.destroy(gl);
        }
        let s = SCALES[self.level.min(SCALES.len() - 1)];
        let (w, h) = (
            ((self.screen.0 as f32 * s) as i32).max(16),
            ((self.screen.1 as f32 * s) as i32).max(16),
        );
        self.target = Some(Target::new(gl, w, h, RGBA8)?);
        Ok(())
    }
}

impl Scene for Fractal {
    fn name(&self) -> &'static str {
        "fractal"
    }

    fn levels(&self) -> usize {
        SCALES.len()
    }

    fn resize(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        self.screen = (w, h);
        self.rebuild_target(gl)
    }

    fn set_quality(&mut self, gl: &glow::Context, level: usize) -> Result<(), String> {
        self.level = level;
        if self.screen.0 > 0 {
            self.rebuild_target(gl)?;
        }
        Ok(())
    }

    fn frame(&mut self, gl: &glow::Context, f: &super::FrameIn) {
        let Some(t) = &self.target else { return };
        let (cr, ci) = crate::visual::julia_c(f.time, f.bot);
        t.bind(gl);
        self.fractal.use_it(gl);
        self.fractal.f2(gl, "u_res", t.w as f32, t.h as f32);
        self.fractal
            .f2(gl, "u_center", self.view.cx as f32, self.view.cy as f32);
        self.fractal.f1(gl, "u_scale", self.view.scale as f32);
        self.fractal.f1(gl, "u_time", f.time);
        self.fractal.f1(gl, "u_bot", f.bot);
        self.fractal.f1(gl, "u_mic", f.mic);
        self.fractal.f2(gl, "u_c", cr, ci);
        self.fractal.f1(gl, "u_julia", 1.0);
        self.fractal.i1(gl, "u_iter", self.view.iterations());
        gl::draw(gl);

        // SAFETY: plain GL state calls on the current context.
        unsafe {
            use glow::HasContext;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, f.w, f.h);
        }
        self.blit.use_it(gl);
        self.blit.tex(gl, "u_tex", 0, t.tex);
        self.blit.f2(gl, "u_res", f.w as f32, f.h as f32);
        gl::draw(gl);
    }

    fn touch(&mut self, ev: &TouchEv, _w: i32, h: i32) {
        match ev.action {
            ACTION_DOWN | ACTION_POINTER_DOWN => {
                self.pointers.retain(|p| p.0 != ev.id);
                self.pointers.push((ev.id, ev.x, ev.y));
            }
            ACTION_MOVE => {
                let single = self.pointers.len() == 1;
                if let Some(p) = self.pointers.iter_mut().find(|p| p.0 == ev.id) {
                    // One finger pans; with two, the pinch gesture owns the view.
                    if single {
                        self.view.pan_pixels(ev.x - p.1, ev.y - p.2, h as f32);
                    }
                    p.1 = ev.x;
                    p.2 = ev.y;
                }
            }
            ACTION_UP | ACTION_POINTER_UP | ACTION_CANCEL => self.pointers.retain(|p| p.0 != ev.id),
            ACTION_DOUBLE_TAP => self.view.reset(),
            _ => {}
        }
    }

    fn pinch(&mut self, scale: f32, fx: f32, fy: f32, w: i32, h: i32) {
        self.view.zoom_at(scale, fx, fy, w as f32, h as f32);
    }

    fn destroy(&mut self, gl: &glow::Context) {
        if let Some(t) = self.target.take() {
            t.destroy(gl);
        }
        self.fractal.destroy(gl);
        self.blit.destroy(gl);
    }
}
