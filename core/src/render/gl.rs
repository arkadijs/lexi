//! Small GLES3 helpers: shader programs (with line-numbered compile errors), render targets, uniforms.

use glow::HasContext;
use std::collections::HashMap;

pub const FULLSCREEN_VERT: &str = include_str!("shaders/fullscreen.vert");

/// A linked program plus a cache of uniform locations.
pub struct Prog {
    pub p: glow::Program,
    name: &'static str,
    locs: HashMap<&'static str, Option<glow::UniformLocation>>,
}

impl Prog {
    pub fn new(gl: &glow::Context, name: &'static str, frag: &str) -> Result<Self, String> {
        Ok(Self {
            p: compile(gl, name, FULLSCREEN_VERT, frag)?,
            name,
            locs: HashMap::new(),
        })
    }

    pub fn use_it(&self, gl: &glow::Context) {
        unsafe { gl.use_program(Some(self.p)) };
    }

    fn loc(&mut self, gl: &glow::Context, u: &'static str) -> Option<glow::UniformLocation> {
        let p = self.p;
        self.locs
            .entry(u)
            .or_insert_with(|| unsafe { gl.get_uniform_location(p, u) })
            .to_owned()
    }

    pub fn f1(&mut self, gl: &glow::Context, u: &'static str, v: f32) {
        let l = self.loc(gl, u);
        unsafe { gl.uniform_1_f32(l.as_ref(), v) };
    }
    pub fn f2(&mut self, gl: &glow::Context, u: &'static str, x: f32, y: f32) {
        let l = self.loc(gl, u);
        unsafe { gl.uniform_2_f32(l.as_ref(), x, y) };
    }
    pub fn f3(&mut self, gl: &glow::Context, u: &'static str, x: f32, y: f32, z: f32) {
        let l = self.loc(gl, u);
        unsafe { gl.uniform_3_f32(l.as_ref(), x, y, z) };
    }
    pub fn i1(&mut self, gl: &glow::Context, u: &'static str, v: i32) {
        let l = self.loc(gl, u);
        unsafe { gl.uniform_1_i32(l.as_ref(), v) };
    }
    /// Bind `tex` to texture unit `unit` and point the sampler uniform at it.
    pub fn tex(&mut self, gl: &glow::Context, u: &'static str, unit: u32, tex: glow::Texture) {
        let l = self.loc(gl, u);
        unsafe {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(l.as_ref(), unit as i32);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        let _ = self.name;
        unsafe { gl.delete_program(self.p) };
    }
}

fn compile(gl: &glow::Context, name: &str, vs: &str, fs: &str) -> Result<glow::Program, String> {
    unsafe {
        let prog = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src, what) in [
            (glow::VERTEX_SHADER, vs, "vertex"),
            (glow::FRAGMENT_SHADER, fs, "fragment"),
        ] {
            let sh = gl.create_shader(kind)?;
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                let log = gl.get_shader_info_log(sh);
                for line in crate::shaderlog::annotate(&format!("{name}/{what}"), src, &log) {
                    log::error!("{line}");
                }
                gl.delete_shader(sh);
                gl.delete_program(prog);
                return Err(format!("{name}: {what} shader failed to compile"));
            }
            gl.attach_shader(prog, sh);
            shaders.push(sh);
        }
        gl.link_program(prog);
        let ok = gl.get_program_link_status(prog);
        for sh in shaders {
            gl.detach_shader(prog, sh);
            gl.delete_shader(sh);
        }
        if !ok {
            let log = gl.get_program_info_log(prog);
            log::error!("shader program {name}: link failed: {log}");
            gl.delete_program(prog);
            return Err(format!("{name}: link failed"));
        }
        Ok(prog)
    }
}

/// A texture with a framebuffer to render into it.
pub struct Target {
    pub tex: glow::Texture,
    pub fbo: glow::Framebuffer,
    pub w: i32,
    pub h: i32,
}

/// Texel format of a [`Target`].
#[derive(Clone, Copy)]
pub struct Format {
    pub internal: u32,
    pub format: u32,
    pub ty: u32,
}

pub const RGBA8: Format = Format {
    internal: glow::RGBA8,
    format: glow::RGBA,
    ty: glow::UNSIGNED_BYTE,
};
pub const RGBA16F: Format = Format {
    internal: glow::RGBA16F,
    format: glow::RGBA,
    ty: glow::HALF_FLOAT,
};
pub const RG16F: Format = Format {
    internal: glow::RG16F,
    format: glow::RG,
    ty: glow::HALF_FLOAT,
};
pub const R16F: Format = Format {
    internal: glow::R16F,
    format: glow::RED,
    ty: glow::HALF_FLOAT,
};

impl Target {
    pub fn new(gl: &glow::Context, w: i32, h: i32, f: Format) -> Result<Self, String> {
        unsafe {
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                f.internal as i32,
                w,
                h,
                0,
                f.format,
                f.ty,
                glow::PixelUnpackData::Slice(None),
            );
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(tex);
                return Err(format!("framebuffer incomplete (0x{status:x}) for {w}x{h}"));
            }
            Ok(Self { tex, fbo, w, h })
        }
    }

    /// Bind as the render target and set the viewport.
    pub fn bind(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.viewport(0, 0, self.w, self.h);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }
}

/// Draw the fullscreen triangle with the bound program.
pub fn draw(gl: &glow::Context) {
    unsafe { gl.draw_arrays(glow::TRIANGLES, 0, 3) };
}
