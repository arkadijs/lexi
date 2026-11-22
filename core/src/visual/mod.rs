//! Platform-independent logic behind the GPU visuals (SPEC §2.3): audio smoothing, fractal view maths and
//! the adaptive-quality controller. The Android-only `render` module draws; this decides, and is tested here.

use std::collections::VecDeque;

/// Fast-attack / slow-release smoothing of an audio level (0..1), so motion follows the voice without
/// flicker. Time constants are in seconds.
#[derive(Debug, Clone)]
pub struct Envelope {
    value: f32,
    attack: f32,
    release: f32,
}

impl Envelope {
    pub fn new(attack: f32, release: f32) -> Self {
        Self {
            value: 0.0,
            attack,
            release,
        }
    }

    pub fn update(&mut self, target: f32, dt: f32) -> f32 {
        let tau = if target > self.value {
            self.attack
        } else {
            self.release
        };
        let k = 1.0 - (-dt / tau.max(1e-4)).exp();
        self.value += (target - self.value) * k;
        self.value
    }

    pub fn value(&self) -> f32 {
        self.value
    }
}

/// Pan/zoom state of the fractal view. Centre and scale are `f64` so repeated gestures do not drift;
/// the GPU gets `f32` (so zoom is clamped to ~1e5, SPEC §2.3).
#[derive(Debug, Clone, PartialEq)]
pub struct FractalView {
    pub cx: f64,
    pub cy: f64,
    /// Height of the view in complex-plane units.
    pub scale: f64,
}

impl Default for FractalView {
    fn default() -> Self {
        Self {
            cx: 0.0,
            cy: 0.0,
            scale: Self::BASE_SCALE,
        }
    }
}

impl FractalView {
    pub const BASE_SCALE: f64 = 3.0;
    pub const MIN_ZOOM: f64 = 0.5;
    pub const MAX_ZOOM: f64 = 1e5;

    pub fn zoom(&self) -> f64 {
        Self::BASE_SCALE / self.scale
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Drag by `(dx, dy)` screen pixels (y grows downwards) on a screen `h` pixels tall.
    pub fn pan_pixels(&mut self, dx: f32, dy: f32, h: f32) {
        let per_px = self.scale / f64::from(h.max(1.0));
        self.cx -= f64::from(dx) * per_px;
        self.cy += f64::from(dy) * per_px;
    }

    /// Multiply the zoom by `factor`, keeping the point under the focus `(fx, fy)` fixed on screen.
    pub fn zoom_at(&mut self, factor: f32, fx: f32, fy: f32, w: f32, h: f32) {
        let h = f64::from(h.max(1.0));
        let (ox, oy) = (
            (f64::from(fx) - f64::from(w) / 2.0) / h,
            (h / 2.0 - f64::from(fy)) / h,
        );
        let (px, py) = (self.cx + ox * self.scale, self.cy + oy * self.scale);
        let zoom =
            (self.zoom() * f64::from(factor.max(1e-3))).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.scale = Self::BASE_SCALE / zoom;
        self.cx = px - ox * self.scale;
        self.cy = py - oy * self.scale;
    }

    /// Escape-time iteration budget: more detail needs more iterations as you zoom in.
    pub fn iterations(&self) -> i32 {
        (96.0 + 28.0 * self.zoom().max(1.0).log2()).clamp(96.0, 700.0) as i32
    }
}

/// What the quality controller wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Lower,
    Raise,
}

/// Adaptive quality (SPEC §2.3): lower the resolution when frames are late for a second, raise it again
/// after a long calm stretch. "Late" is judged against the display's own refresh interval, estimated as
/// the 10th percentile of recent frame intervals, so 60, 90 and 120 Hz screens all work.
#[derive(Debug)]
pub struct QualityController {
    levels: usize,
    level: usize,
    recent: VecDeque<f32>,
    window_sum: f32,
    window_n: u32,
    window_len: f32,
    good_windows: u32,
    since_change: f32,
}

impl QualityController {
    const WINDOW_SECS: f32 = 1.0;
    const RECENT: usize = 240;
    /// Average interval above `period * LATE` means the renderer cannot keep up.
    const LATE: f32 = 1.3;
    const HEALTHY: f32 = 1.12;
    const GOOD_WINDOWS_TO_RAISE: u32 = 5;
    const MIN_SECS_BETWEEN_CHANGES: f32 = 3.0;

    pub fn new(levels: usize) -> Self {
        Self {
            levels: levels.max(1),
            level: 0,
            recent: VecDeque::new(),
            window_sum: 0.0,
            window_n: 0,
            window_len: 0.0,
            good_windows: 0,
            since_change: 0.0,
        }
    }

    pub fn level(&self) -> usize {
        self.level
    }

    /// Estimated refresh interval in seconds (10th percentile of recent intervals).
    pub fn period(&self) -> f32 {
        if self.recent.is_empty() {
            return 1.0 / 60.0;
        }
        let mut v: Vec<f32> = self.recent.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 10]
    }

    /// Feed the time since the previous frame; returns a change when the level should move (and has).
    pub fn frame(&mut self, dt: f32) -> Option<Change> {
        if !(0.002..=0.25).contains(&dt) {
            return None; // a pause or a bogus first interval says nothing about load
        }
        if self.recent.len() == Self::RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(dt);
        self.window_sum += dt;
        self.window_n += 1;
        self.window_len += dt;
        self.since_change += dt;
        if self.window_len < Self::WINDOW_SECS {
            return None;
        }
        let avg = self.window_sum / self.window_n as f32;
        let period = self.period();
        self.window_sum = 0.0;
        self.window_n = 0;
        self.window_len = 0.0;

        if avg > period * Self::LATE {
            self.good_windows = 0;
            if self.level + 1 < self.levels && self.since_change >= Self::MIN_SECS_BETWEEN_CHANGES {
                self.level += 1;
                self.since_change = 0.0;
                return Some(Change::Lower);
            }
        } else if avg <= period * Self::HEALTHY {
            self.good_windows += 1;
            if self.good_windows >= Self::GOOD_WINDOWS_TO_RAISE && self.level > 0 {
                self.good_windows = 0;
                self.level -= 1;
                self.since_change = 0.0;
                return Some(Change::Raise);
            }
        } else {
            self.good_windows = 0;
        }
        None
    }
}

/// Map a smoothed audio RMS (speech peaks around 0.1-0.25) to the 0..1 range the visuals are scaled by. Linear
/// with a fixed gain, so the effect stays proportional to loudness and near-silence (the server's comfort noise
/// is around 0.001) produces nothing.
pub fn visual_level(rms: f32) -> f32 {
    (rms * 4.0).clamp(0.0, 1.0)
}

/// The Julia constant for the fractal scene at `time` seconds with the bot at loudness `bot` (0..1).
///
/// It travels slowly round a slightly shrunk copy of the main cardioid of the Mandelbrot set,
/// `c = k (e^{it}/2 - e^{2it}/4)`. Every `c` inside the Mandelbrot set gives a *connected* Julia set (a rabbit,
/// a spiral, a smooth blob with a fractal edge); outside it the set shatters into near-invisible dust. The bot's
/// voice wobbles the angle and pushes `k` towards (but never past) the edge, so the shape breathes with it.
pub fn julia_c(time: f32, bot: f32) -> (f32, f32) {
    let bot = bot.clamp(0.0, 1.0);
    let t = time * 0.04 + bot * 0.25 * (time * 5.0).sin();
    let k = 0.95 + 0.04 * bot * (0.5 + 0.5 * (time * 7.0).sin());
    (
        k * (0.5 * t.cos() - 0.25 * (2.0 * t).cos()),
        k * (0.5 * t.sin() - 0.25 * (2.0 * t).sin()),
    )
}

/// HSV (all 0..1; hue wraps) to RGB, for the ink colours.
pub fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let (i, f) = (h.floor(), h - h.floor());
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Fluid simulation quality levels, best first: (long side of the velocity grid in cells, dye
/// resolution as a fraction of the screen, Jacobi pressure iterations). SPEC §2.3: the simulation grid is at
/// most half the screen, dye is the screen or three quarters of it; quality only ever goes down from there.
pub const FLUID_LEVELS: [(i32, f32, i32); 4] = [
    (256, 0.75, 20),
    (192, 0.6, 16),
    (128, 0.5, 12),
    (96, 0.4, 8),
];

/// Grid size for a `w` x `h` screen when the long side has `long` cells (aspect preserved, at least 8).
pub fn grid_size(w: i32, h: i32, long: i32) -> (i32, i32) {
    let (w, h) = (w.max(1), h.max(1));
    let k = long as f32 / w.max(h) as f32;
    (
        ((w as f32 * k).round() as i32).max(8),
        ((h as f32 * k).round() as i32).max(8),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_attacks_fast_and_releases_slowly() {
        let mut e = Envelope::new(0.04, 0.3);
        for _ in 0..6 {
            e.update(1.0, 1.0 / 60.0); // 100 ms of loud
        }
        let loud = e.value();
        assert!(loud > 0.85, "{loud}");
        for _ in 0..6 {
            e.update(0.0, 1.0 / 60.0); // 100 ms of silence
        }
        assert!(
            e.value() > loud * 0.6,
            "release must be slower than attack: {} -> {}",
            loud,
            e.value()
        );
        for _ in 0..300 {
            e.update(0.0, 1.0 / 60.0);
        }
        assert!(e.value() < 0.01);
    }

    #[test]
    fn zoom_keeps_the_focus_point_fixed() {
        let mut v = FractalView {
            cx: -0.4,
            cy: 0.2,
            scale: 1.7,
        };
        let (w, h, fx, fy) = (1080.0, 2237.0, 300.0, 1700.0);
        let at = |v: &FractalView| {
            let hh = f64::from(h);
            (
                v.cx + ((f64::from(fx) - f64::from(w) / 2.0) / hh) * v.scale,
                v.cy + ((hh / 2.0 - f64::from(fy)) / hh) * v.scale,
            )
        };
        let before = at(&v);
        v.zoom_at(2.5, fx, fy, w, h);
        let after = at(&v);
        assert!((before.0 - after.0).abs() < 1e-12 && (before.1 - after.1).abs() < 1e-12);
        assert!((v.zoom() - (3.0 / 1.7) * 2.5).abs() < 1e-9);
    }

    #[test]
    fn zoom_is_clamped_in_both_directions() {
        let mut v = FractalView::default();
        for _ in 0..200 {
            v.zoom_at(1.5, 540.0, 1000.0, 1080.0, 2237.0);
        }
        assert!((v.zoom() - FractalView::MAX_ZOOM).abs() < 1e-6);
        for _ in 0..200 {
            v.zoom_at(0.5, 540.0, 1000.0, 1080.0, 2237.0);
        }
        assert!((v.zoom() - FractalView::MIN_ZOOM).abs() < 1e-9);
    }

    #[test]
    fn pan_follows_the_finger_and_reset_restores_the_view() {
        let mut v = FractalView::default();
        v.pan_pixels(100.0, 0.0, 2000.0); // finger moves right: the scene follows, so the centre moves left
        assert!(v.cx < 0.0 && v.cy == 0.0);
        v.pan_pixels(0.0, 100.0, 2000.0); // finger moves down: the centre moves up
        assert!(v.cy > 0.0);
        v.reset();
        assert_eq!(v, FractalView::default());
    }

    #[test]
    fn iterations_grow_with_zoom_and_stay_bounded() {
        let mut v = FractalView::default();
        let base = v.iterations();
        v.zoom_at(1000.0, 540.0, 1000.0, 1080.0, 2237.0);
        assert!(v.iterations() > base && v.iterations() <= 700);
        // absurdly deep (scale 1e-9) hits the cap; zoomed far out stays at the floor
        let deep = FractalView {
            scale: 1e-9,
            ..FractalView::default()
        };
        let far = FractalView {
            scale: 1e9,
            ..FractalView::default()
        };
        assert_eq!((deep.iterations(), far.iterations()), (700, 96));
    }

    fn feed(q: &mut QualityController, dt: f32, secs: f32) -> Vec<Change> {
        (0..(secs / dt) as usize)
            .filter_map(|_| q.frame(dt))
            .collect()
    }

    #[test]
    fn steady_60_fps_never_changes_quality() {
        let mut q = QualityController::new(4);
        assert!(feed(&mut q, 1.0 / 60.0, 30.0).is_empty());
        assert_eq!(q.level(), 0);
        assert!((q.period() - 1.0 / 60.0).abs() < 1e-4);
    }

    #[test]
    fn sustained_late_frames_lower_quality_step_by_step() {
        let mut q = QualityController::new(4);
        feed(&mut q, 1.0 / 60.0, 5.0); // learn the display period
        let changes = feed(&mut q, 1.0 / 30.0, 12.0); // now every frame is late
        assert_eq!(changes.first(), Some(&Change::Lower));
        assert!(
            q.level() >= 2,
            "should keep lowering while overloaded, level {}",
            q.level()
        );
        assert!(q.level() <= 3);
    }

    #[test]
    fn quality_recovers_after_a_long_calm_period_but_not_instantly() {
        let mut q = QualityController::new(4);
        feed(&mut q, 1.0 / 60.0, 5.0);
        feed(&mut q, 1.0 / 30.0, 6.0);
        let low = q.level();
        assert!(low >= 1);
        assert!(
            feed(&mut q, 1.0 / 60.0, 3.0).is_empty(),
            "no raise after only 3 calm seconds"
        );
        let up = feed(&mut q, 1.0 / 60.0, 12.0);
        assert!(up.contains(&Change::Raise));
        assert!(q.level() < low);
    }

    #[test]
    fn works_on_a_90_hz_display_and_ignores_pauses() {
        let mut q = QualityController::new(4);
        assert!(feed(&mut q, 1.0 / 90.0, 20.0).is_empty());
        assert!(q.frame(2.0).is_none(), "an app pause is not load");
        assert!(q.frame(0.0).is_none());
    }

    /// Inside the main cardioid of the Mandelbrot set (so the Julia set is connected).
    fn in_main_cardioid(x: f32, y: f32) -> bool {
        let q = (x - 0.25).powi(2) + y * y;
        q * (q + (x - 0.25)) < 0.25 * y * y
    }

    #[test]
    fn visual_level_is_proportional_and_ignores_silence() {
        assert_eq!(visual_level(0.0), 0.0);
        assert!(
            visual_level(0.001) < 0.01,
            "comfort noise must not move the picture"
        );
        assert!(
            (visual_level(0.1) - 0.4).abs() < 1e-6,
            "proportional: twice as loud, twice the effect"
        );
        assert!((visual_level(0.05) * 2.0 - visual_level(0.1)).abs() < 1e-6);
        assert_eq!(visual_level(0.5), 1.0, "capped, never beyond full effect");
        assert!(
            visual_level(0.19) > 0.7,
            "a loud bot (the measured peak) clearly stirs the visuals"
        );
    }

    #[test]
    fn julia_constant_never_leaves_the_connected_region() {
        for step in 0..4000 {
            let time = step as f32 * 0.25;
            for bot in [0.0, 0.3, 0.7, 1.0] {
                let (x, y) = julia_c(time, bot);
                assert!(
                    in_main_cardioid(x, y),
                    "t={time} bot={bot}: ({x}, {y}) is outside the cardioid"
                );
            }
        }
        assert!(
            !in_main_cardioid(0.7885, 0.0),
            "sanity: the old circle started in the dust region"
        );
    }

    #[test]
    fn julia_constant_moves_slowly_when_calm_and_more_with_the_bot() {
        let travel = |bot: f32| {
            (0..400)
                .map(|i| {
                    let (a, b) = (
                        julia_c(i as f32 * 0.05, bot),
                        julia_c((i + 1) as f32 * 0.05, bot),
                    );
                    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
                })
                .sum::<f32>()
        };
        assert!(travel(0.0) < travel(1.0), "the bot's voice adds motion");
        assert!(
            travel(0.0) / 20.0 < 0.1,
            "calm: under 0.1 plane units of movement per second"
        );
    }

    #[test]
    fn hsv_primaries_grey_and_wrap() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]);
        let g = hsv(1.0 / 3.0, 1.0, 1.0);
        assert!(g[1] > 0.99 && g[0] < 0.01);
        assert_eq!(hsv(0.7, 0.0, 0.5), [0.5, 0.5, 0.5]);
        assert_eq!(hsv(1.0, 1.0, 1.0), hsv(0.0, 1.0, 1.0));
        assert_eq!(hsv(-0.25, 1.0, 1.0), hsv(0.75, 1.0, 1.0));
    }

    #[test]
    fn grid_keeps_aspect_ratio() {
        assert_eq!(grid_size(1080, 2237, 256), (124, 256));
        assert_eq!(grid_size(2237, 1080, 256), (256, 124));
        assert_eq!(
            grid_size(1, 1000, 96),
            (8, 96),
            "a sliver is clamped to the minimum width"
        );
        assert_eq!(FLUID_LEVELS.len(), 4);
        assert!(FLUID_LEVELS
            .windows(2)
            .all(|w| w[0].0 > w[1].0 && w[0].2 > w[1].2));
    }
}
