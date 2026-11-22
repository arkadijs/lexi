//! EGL setup on an `ANativeWindow`: display, an ES 3.0 context and a window surface.

use khronos_egl as egl;
use ndk::native_window::NativeWindow;

// khronos-egl built with `no-pkg-config` does not tell the linker about libEGL, and a cdylib links fine
// with undefined symbols, so without this the app crashes at load time ("cannot locate symbol eglSwapInterval").
#[link(name = "EGL")]
unsafe extern "C" {}

/// `EGL_OPENGL_ES3_BIT` (EGL 1.5 / KHR_create_context); defined here because we only enable the 1.4 API.
const OPENGL_ES3_BIT: egl::Int = 0x0040;

pub struct Egl {
    inst: egl::Instance<egl::Static>,
    display: egl::Display,
    surface: egl::Surface,
    context: egl::Context,
}

fn e(what: &'static str) -> impl FnOnce(egl::Error) -> String {
    move |err| format!("{what}: {err}")
}

impl Egl {
    pub fn new(window: &NativeWindow) -> Result<Self, String> {
        let inst = egl::Instance::new(egl::Static);
        // SAFETY: EGL_DEFAULT_DISPLAY is always a valid argument.
        let display = unsafe { inst.get_display(egl::DEFAULT_DISPLAY) }.ok_or("no EGL display")?;
        inst.initialize(display).map_err(e("eglInitialize"))?;
        let attribs = [
            egl::RENDERABLE_TYPE,
            OPENGL_ES3_BIT,
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            0,
            egl::DEPTH_SIZE,
            0,
            egl::NONE,
        ];
        let config = inst
            .choose_first_config(display, &attribs)
            .map_err(e("eglChooseConfig"))?
            .ok_or("no EGL config for ES 3.0 / RGB888")?;
        // SAFETY: the window pointer is a live ANativeWindow owned by `window` for the whole call.
        let surface = unsafe {
            inst.create_window_surface(display, config, window.ptr().as_ptr().cast(), None)
        }
        .map_err(e("eglCreateWindowSurface"))?;
        let context = inst
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .map_err(e("eglCreateContext (ES 3.0)"))?;
        inst.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(e("eglMakeCurrent"))?;
        let _ = inst.swap_interval(display, 1);
        Ok(Self {
            inst,
            display,
            surface,
            context,
        })
    }

    /// Resolve a GL function for `glow`.
    pub fn proc_address(&self, name: &str) -> *const std::ffi::c_void {
        self.inst
            .get_proc_address(name)
            .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
    }

    /// Current surface size in pixels.
    pub fn size(&self) -> (i32, i32) {
        let q = |a| {
            self.inst
                .query_surface(self.display, self.surface, a)
                .unwrap_or(0)
        };
        (q(egl::WIDTH), q(egl::HEIGHT))
    }

    pub fn swap(&self) -> Result<(), String> {
        self.inst
            .swap_buffers(self.display, self.surface)
            .map_err(e("eglSwapBuffers"))
    }
}

impl Drop for Egl {
    fn drop(&mut self) {
        let _ = self.inst.make_current(self.display, None, None, None);
        let _ = self.inst.destroy_surface(self.display, self.surface);
        let _ = self.inst.destroy_context(self.display, self.context);
        let _ = self.inst.terminate(self.display);
    }
}
