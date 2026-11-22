//! GPU time per frame via `EXT_disjoint_timer_query`: how long the GPU really spent on our commands,
//! which is what "GPU load" means for this app (no vendor counter is readable on the phone).
//! Results arrive a few frames late and are polled without blocking; a disjoint event (clock change,
//! context loss) discards the affected samples.

use glow::HasContext;
use std::collections::VecDeque;

const TIME_ELAPSED_EXT: u32 = 0x88BF;
const GPU_DISJOINT_EXT: u32 = 0x8FBB;
const MAX_IN_FLIGHT: usize = 8;

pub struct GpuTimer {
    supported: bool,
    open: Option<glow::Query>,
    pending: VecDeque<glow::Query>,
    free: Vec<glow::Query>,
    sum_ns: u64,
    max_ns: u32,
    n: u32,
}

/// Average and worst GPU time per frame since the last call, and how many frames that covers.
#[derive(Debug, Clone, Copy)]
pub struct GpuStats {
    pub avg_ms: f32,
    pub max_ms: f32,
    pub frames: u32,
}

impl GpuTimer {
    pub fn new(gl: &glow::Context) -> Self {
        let supported = gl
            .supported_extensions()
            .contains("GL_EXT_disjoint_timer_query");
        log::info!(
            "render: GPU timer queries {}",
            if supported {
                "available"
            } else {
                "NOT available"
            }
        );
        Self {
            supported,
            open: None,
            pending: VecDeque::new(),
            free: Vec::new(),
            sum_ns: 0,
            max_ns: 0,
            n: 0,
        }
    }

    /// Start timing the commands that follow (until [`end`](Self::end)).
    pub fn begin(&mut self, gl: &glow::Context) {
        if !self.supported || self.open.is_some() || self.pending.len() >= MAX_IN_FLIGHT {
            return;
        }
        // SAFETY: query objects belong to the current context.
        unsafe {
            let q = match self.free.pop() {
                Some(q) => q,
                None => match gl.create_query() {
                    Ok(q) => q,
                    Err(_) => return,
                },
            };
            gl.begin_query(TIME_ELAPSED_EXT, q);
            self.open = Some(q);
        }
    }

    pub fn end(&mut self, gl: &glow::Context) {
        if let Some(q) = self.open.take() {
            // SAFETY: matches the `begin_query` above.
            unsafe { gl.end_query(TIME_ELAPSED_EXT) };
            self.pending.push_back(q);
        }
    }

    /// Collect the results that are ready (never blocks).
    pub fn collect(&mut self, gl: &glow::Context) {
        while let Some(&q) = self.pending.front() {
            // SAFETY: plain queries on objects we created.
            unsafe {
                if gl.get_query_parameter_u32(q, glow::QUERY_RESULT_AVAILABLE) == 0 {
                    break;
                }
                let disjoint = gl.get_parameter_i32(GPU_DISJOINT_EXT) != 0;
                let ns = gl.get_query_parameter_u32(q, glow::QUERY_RESULT);
                if !disjoint {
                    self.sum_ns += u64::from(ns);
                    self.max_ns = self.max_ns.max(ns);
                    self.n += 1;
                }
            }
            self.pending.pop_front();
            self.free.push(q);
        }
    }

    pub fn take_stats(&mut self) -> Option<GpuStats> {
        if self.n == 0 {
            return None;
        }
        let s = GpuStats {
            avg_ms: (self.sum_ns as f64 / f64::from(self.n) / 1e6) as f32,
            max_ms: self.max_ns as f32 / 1e6,
            frames: self.n,
        };
        self.sum_ns = 0;
        self.max_ns = 0;
        self.n = 0;
        Some(s)
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        // SAFETY: deleting our own query objects.
        unsafe {
            for q in self
                .pending
                .drain(..)
                .chain(self.free.drain(..))
                .chain(self.open.take())
            {
                gl.delete_query(q);
            }
        }
    }
}
