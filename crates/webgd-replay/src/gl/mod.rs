//! WebGL2 replay helpers: per-context scratch objects, readback, blit uploads, enum names.
pub mod c;
pub mod formats;
pub mod inspect;
pub mod snapshot;
pub mod uniforms;

use crate::js;
use c::*;
use formats::ReadClass;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use webgd_core::ObjId;
use webgd_core::pixels::Image;

/// One replay WebGL2 context.
pub struct GlCtx {
    /// Capture id of the context.
    pub id: ObjId,
    pub gl: JsValue,
    pub scratch_fbo: JsValue,
    pub scratch_vao: JsValue,
    pub scratch_unit: u32,
    /// EXT_color_buffer_float enabled.
    pub has_cbf: bool,
    pub timer_ext: Option<JsValue>,
    /// Cached helper programs (texture sampling readback), keyed by (sampler kind, target).
    pub progs: RefCell<HashMap<(u32, u32), JsValue>>,
}

impl GlCtx {
    pub fn new(id: ObjId, gl: JsValue) -> GlCtx {
        let has_cbf = js::is_obj(&js::call(&gl, "getExtension", &[JsValue::from_str("EXT_color_buffer_float")]).unwrap_or_default());
        // Required for blending into the R16F fragment-count target.
        let _ = js::call(&gl, "getExtension", &[JsValue::from_str("EXT_float_blend")]);
        let timer = js::call(&gl, "getExtension", &[JsValue::from_str("EXT_disjoint_timer_query_webgl2")]).unwrap_or_default();
        let scratch_fbo = js::call(&gl, "createFramebuffer", &[]).unwrap_or_default();
        let scratch_vao = js::call(&gl, "createVertexArray", &[]).unwrap_or_default();
        let units = js::u(&param(&gl, MAX_COMBINED_TEXTURE_IMAGE_UNITS));
        GlCtx {
            id,
            gl,
            scratch_fbo,
            scratch_vao,
            scratch_unit: units.saturating_sub(1),
            has_cbf,
            timer_ext: js::is_obj(&timer).then_some(timer),
            progs: RefCell::new(HashMap::new()),
        }
    }

    pub fn call(&self, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
        js::call(&self.gl, name, args)
    }

    /// Calls and ignores the result (GL calls reporting via getError, not exceptions).
    pub fn c(&self, name: &str, args: &[f64]) {
        let a: Vec<JsValue> = args.iter().map(|v| JsValue::from_f64(*v)).collect();
        let _ = js::call(&self.gl, name, &a);
    }

    pub fn param(&self, pname: u32) -> JsValue {
        param(&self.gl, pname)
    }

    pub fn bind_fb(&self, target: u32, fb: &JsValue) {
        let _ = self.call("bindFramebuffer", &[n(target), fb.clone()]);
    }

    pub fn bind_tex(&self, target: u32, tex: &JsValue) {
        let _ = self.call("bindTexture", &[n(target), tex.clone()]);
    }

    pub fn lose(&self) {
        if let Ok(ext) = self.call("getExtension", &[JsValue::from_str("WEBGL_lose_context")])
            && js::is_obj(&ext)
        {
            let _ = js::call(&ext, "loseContext", &[]);
        }
    }

    /// Reads `att` (w×h) through the scratch FBO bound as READ_FRAMEBUFFER. Restores bindings.
    pub fn read_attachment(&self, att: &Attach, w: u32, h: u32, class: ReadClass) -> Result<Image, String> {
        if class == ReadClass::Float && !self.has_cbf {
            return Err("float render targets unsupported".into());
        }
        let prev = self.param(READ_FRAMEBUFFER_BINDING);
        self.bind_fb(READ_FRAMEBUFFER, &self.scratch_fbo);
        attach(self, READ_FRAMEBUFFER, COLOR_ATTACHMENT0, att);
        let _ = self.call("readBuffer", &[n(COLOR_ATTACHMENT0)]);
        let status = js::u(&self.call("checkFramebufferStatus", &[n(READ_FRAMEBUFFER)]).unwrap_or_default());
        let res = if status == FRAMEBUFFER_COMPLETE {
            self.read_pixels(w, h, class)
        } else {
            Err(format!("attachment not readable (framebuffer status 0x{status:X})"))
        };
        attach(self, READ_FRAMEBUFFER, COLOR_ATTACHMENT0, &Attach::None);
        self.bind_fb(READ_FRAMEBUFFER, &prev);
        res
    }

    /// `readPixels` of the bound READ_FRAMEBUFFER; rows are flipped so row 0 is the top.
    pub fn read_pixels(&self, w: u32, h: u32, class: ReadClass) -> Result<Image, String> {
        let (format, ty, ctor, pf) = class.read_spec();
        let saved_pack: Vec<(u32, JsValue)> = [PACK_ALIGNMENT, PACK_ROW_LENGTH, PACK_SKIP_ROWS, PACK_SKIP_PIXELS]
            .iter()
            .map(|p| (*p, self.param(*p)))
            .collect();
        let pack_buf = self.param(PIXEL_PACK_BUFFER_BINDING);
        let _ = self.call("bindBuffer", &[n(PIXEL_PACK_BUFFER), JsValue::NULL]);
        self.c("pixelStorei", &[PACK_ALIGNMENT as f64, 4.0]);
        for p in [PACK_ROW_LENGTH, PACK_SKIP_ROWS, PACK_SKIP_PIXELS] {
            self.c("pixelStorei", &[p as f64, 0.0]);
        }
        let len = w as f64 * h as f64 * 4.0;
        let res = js::construct(ctor, &[JsValue::from_f64(len)]).and_then(|buf| {
            self.call("readPixels", &[n(0), n(0), n(w), n(h), n(format), n(ty), buf.clone()])?;
            Ok(buf)
        });
        for (p, v) in saved_pack {
            let _ = self.call("pixelStorei", &[n(p), v]);
        }
        let _ = self.call("bindBuffer", &[n(PIXEL_PACK_BUFFER), pack_buf]);
        let buf = res.map_err(|e| js::err_msg(&e))?;
        let texel_bytes = if ty == UNSIGNED_BYTE { 4 } else { 16 };
        let data = flip_rows(&js::bytes_of(&buf), (w * texel_bytes) as usize, h as usize);
        Ok(Image { width: w, height: h, format: pf, data })
    }

    /// Uploads `data` (RGBA of `class`, w×h, GL row order) to `dst` through a temp texture + blit.
    /// `dst` = `Attach::None` targets the default framebuffer.
    pub fn blit_upload(&self, class: ReadClass, w: u32, h: u32, data: &JsValue, dst: &Attach) -> Result<(), String> {
        if class == ReadClass::Float && !self.has_cbf {
            return Err("float render targets unsupported".into());
        }
        let (format, ty, _, _) = class.read_spec();
        let tmp = self.call("createTexture", &[]).map_err(|e| js::err_msg(&e))?;
        self.c("activeTexture", &[(TEXTURE0 + self.scratch_unit) as f64]);
        self.bind_tex(TEXTURE_2D, &tmp);
        let _ = self.call("bindBuffer", &[n(PIXEL_UNPACK_BUFFER), JsValue::NULL]);
        let r = self.call(
            "texImage2D",
            &[n(TEXTURE_2D), n(0), n(class.temp_internalformat()), n(w), n(h), n(0), n(format), n(ty), data.clone()],
        );
        if let Err(e) = r {
            let _ = self.call("deleteTexture", &[tmp]);
            return Err(js::err_msg(&e));
        }
        let read_fb = self.call("createFramebuffer", &[]).map_err(|e| js::err_msg(&e))?;
        self.bind_fb(READ_FRAMEBUFFER, &read_fb);
        let _ = self.call("framebufferTexture2D", &[n(READ_FRAMEBUFFER), n(COLOR_ATTACHMENT0), n(TEXTURE_2D), tmp.clone(), n(0)]);
        let _ = self.call("readBuffer", &[n(COLOR_ATTACHMENT0)]);
        match dst {
            Attach::None => self.bind_fb(DRAW_FRAMEBUFFER, &JsValue::NULL),
            _ => {
                self.bind_fb(DRAW_FRAMEBUFFER, &self.scratch_fbo);
                attach(self, DRAW_FRAMEBUFFER, COLOR_ATTACHMENT0, dst);
                let _ = self.call("drawBuffers", &[js::arr(&[n(COLOR_ATTACHMENT0)])]);
            }
        }
        self.c("disable", &[SCISSOR_TEST as f64]);
        self.c("disable", &[RASTERIZER_DISCARD as f64]);
        let status = js::u(&self.call("checkFramebufferStatus", &[n(DRAW_FRAMEBUFFER)]).unwrap_or_default());
        let res = if status == FRAMEBUFFER_COMPLETE {
            self.c("blitFramebuffer", &[0.0, 0.0, w as f64, h as f64, 0.0, 0.0, w as f64, h as f64, COLOR_BUFFER_BIT as f64, NEAREST as f64]);
            Ok(())
        } else {
            Err(format!("restore target incomplete (framebuffer status 0x{status:X})"))
        };
        if !matches!(dst, Attach::None) {
            attach(self, DRAW_FRAMEBUFFER, COLOR_ATTACHMENT0, &Attach::None);
        }
        self.bind_fb(READ_FRAMEBUFFER, &JsValue::NULL);
        self.bind_fb(DRAW_FRAMEBUFFER, &JsValue::NULL);
        let _ = self.call("deleteFramebuffer", &[read_fb]);
        let _ = self.call("deleteTexture", &[tmp]);
        res
    }
}

/// Framebuffer attachment source.
pub enum Attach {
    None,
    /// `target` is the texture's bind target (TEXTURE_2D/CUBE_MAP/3D/2D_ARRAY); face 0..5.
    Tex { tex: JsValue, target: u32, level: u32, face: u32, layer: u32 },
    Rb(JsValue),
}

pub fn attach(ctx: &GlCtx, fb_target: u32, attachment: u32, att: &Attach) {
    let r = match att {
        Attach::None => ctx.call("framebufferRenderbuffer", &[n(fb_target), n(attachment), n(RENDERBUFFER), JsValue::NULL]),
        Attach::Rb(rb) => ctx.call("framebufferRenderbuffer", &[n(fb_target), n(attachment), n(RENDERBUFFER), rb.clone()]),
        Attach::Tex { tex, target, level, face, layer } => match *target {
            TEXTURE_3D | TEXTURE_2D_ARRAY => {
                ctx.call("framebufferTextureLayer", &[n(fb_target), n(attachment), tex.clone(), n(*level), n(*layer)])
            }
            TEXTURE_CUBE_MAP => ctx.call(
                "framebufferTexture2D",
                &[n(fb_target), n(attachment), n(TEXTURE_CUBE_MAP_POSITIVE_X + face), tex.clone(), n(*level)],
            ),
            _ => ctx.call("framebufferTexture2D", &[n(fb_target), n(attachment), n(TEXTURE_2D), tex.clone(), n(*level)]),
        },
    };
    let _ = r;
}

pub fn n(v: u32) -> JsValue {
    JsValue::from_f64(v as f64)
}

pub fn param(gl: &JsValue, pname: u32) -> JsValue {
    js::call(gl, "getParameter", &[n(pname)]).unwrap_or(JsValue::NULL)
}

/// Reverses row order (GL bottom-up → top-down).
pub fn flip_rows(data: &[u8], row_bytes: usize, h: usize) -> Vec<u8> {
    if row_bytes == 0 || data.len() < row_bytes * h {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(row_bytes * h);
    for y in (0..h).rev() {
        out.extend_from_slice(&data[y * row_bytes..(y + 1) * row_bytes]);
    }
    out
}

thread_local! {
    static ENUM_NAMES: OnceCell<HashMap<u32, String>> = const { OnceCell::new() };
}

/// Name of a WebGL2 enum value (first `WebGL2RenderingContext.prototype` constant wins), else hex.
pub fn enum_name(v: u32) -> String {
    ENUM_NAMES.with(|cell| {
        let map = cell.get_or_init(|| {
            let mut m = HashMap::new();
            let proto = js::get(&js::get(&js::global(), "WebGL2RenderingContext"), "prototype");
            if js::is_obj(&proto) {
                let names = js_sys::Object::get_own_property_names(proto.unchecked_ref::<js_sys::Object>());
                for name in names.iter() {
                    let Some(s) = name.as_string() else { continue };
                    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                        continue;
                    }
                    if let Some(val) = js::get(&proto, &s).as_f64() {
                        m.entry(val as u32).or_insert(s);
                    }
                }
            }
            m
        });
        map.get(&v).cloned().unwrap_or_else(|| format!("0x{v:X}"))
    })
}


/// Saved GL state for temporary draws (readback, fragment counting).
pub struct Saved {
    vals: Vec<(u32, JsValue)>,
    caps: Vec<(u32, bool)>,
}

impl GlCtx {
    pub fn save(&self, pnames: &[u32], caps: &[u32]) -> Saved {
        Saved {
            vals: pnames.iter().map(|p| (*p, self.param(*p))).collect(),
            caps: caps
                .iter()
                .map(|c| (*c, js::truthy(&self.call("isEnabled", &[n(*c)]).unwrap_or_default())))
                .collect(),
        }
    }

    pub fn restore(&self, s: Saved) {
        for (c, on) in s.caps {
            self.c(if on { "enable" } else { "disable" }, &[c as f64]);
        }
        let get = |p: u32| s.vals.iter().find(|(k, _)| *k == p).map(|(_, v)| v.clone());
        let spread = |v: &JsValue| -> Vec<JsValue> { js_sys::Array::from(v).iter().collect() };
        let mut active = None;
        for (p, v) in &s.vals {
            let v = v.clone();
            let _ = match *p {
                CURRENT_PROGRAM => self.call("useProgram", &[v]),
                VERTEX_ARRAY_BINDING => self.call("bindVertexArray", &[v]),
                DRAW_FRAMEBUFFER_BINDING => self.call("bindFramebuffer", &[n(DRAW_FRAMEBUFFER), v]),
                READ_FRAMEBUFFER_BINDING => self.call("bindFramebuffer", &[n(READ_FRAMEBUFFER), v]),
                RENDERBUFFER_BINDING => self.call("bindRenderbuffer", &[n(RENDERBUFFER), v]),
                ARRAY_BUFFER_BINDING => self.call("bindBuffer", &[n(ARRAY_BUFFER), v]),
                COPY_READ_BUFFER_BINDING => self.call("bindBuffer", &[n(COPY_READ_BUFFER), v]),
                VIEWPORT => self.call("viewport", &spread(&v)),
                COLOR_WRITEMASK => self.call("colorMask", &spread(&v)),
                COLOR_CLEAR_VALUE => self.call("clearColor", &spread(&v)),
                BLEND_SRC_RGB => self.call(
                    "blendFuncSeparate",
                    &[v, get(BLEND_DST_RGB).unwrap_or_default(), get(BLEND_SRC_ALPHA).unwrap_or_default(), get(BLEND_DST_ALPHA).unwrap_or_default()],
                ),
                BLEND_EQUATION_RGB => self.call("blendEquationSeparate", &[v, get(BLEND_EQUATION_ALPHA).unwrap_or_default()]),
                ACTIVE_TEXTURE => {
                    active = Some(v);
                    Ok(JsValue::UNDEFINED)
                }
                _ => Ok(JsValue::UNDEFINED),
            };
        }
        if let Some(v) = active {
            let _ = self.call("activeTexture", &[v]);
        }
    }

    /// Compiles and links a program; returns it or the info log.
    pub fn build_program(&self, vs: &JsValue, fs_src: &str, attribs: &[(String, JsValue)]) -> Result<JsValue, String> {
        let fs = self.call("createShader", &[n(FRAGMENT_SHADER)]).map_err(|e| js::err_msg(&e))?;
        let _ = self.call("shaderSource", &[fs.clone(), fs_src.into()]);
        let _ = self.call("compileShader", std::slice::from_ref(&fs));
        let p = self.call("createProgram", &[]).map_err(|e| js::err_msg(&e))?;
        let _ = self.call("attachShader", &[p.clone(), vs.clone()]);
        let _ = self.call("attachShader", &[p.clone(), fs.clone()]);
        for (name, loc) in attribs {
            let _ = self.call("bindAttribLocation", &[p.clone(), loc.clone(), name.as_str().into()]);
        }
        let _ = self.call("linkProgram", std::slice::from_ref(&p));
        if js::truthy(&self.call("getProgramParameter", &[p.clone(), n(LINK_STATUS)]).unwrap_or_default()) {
            Ok(p)
        } else {
            let log = self.call("getProgramInfoLog", std::slice::from_ref(&p)).ok().and_then(|v| v.as_string()).unwrap_or_default();
            let flog = self.call("getShaderInfoLog", std::slice::from_ref(&fs)).ok().and_then(|v| v.as_string()).unwrap_or_default();
            let _ = self.call("deleteProgram", &[p]);
            let _ = self.call("deleteShader", &[fs]);
            Err(format!("{log} {flog}"))
        }
    }

    pub fn compile(&self, ty: u32, src: &str) -> Result<JsValue, String> {
        let s = self.call("createShader", &[n(ty)]).map_err(|e| js::err_msg(&e))?;
        let _ = self.call("shaderSource", &[s.clone(), src.into()]);
        let _ = self.call("compileShader", std::slice::from_ref(&s));
        if js::truthy(&self.call("getShaderParameter", &[s.clone(), n(0x8B81)]).unwrap_or_default()) {
            Ok(s)
        } else {
            Err(self.call("getShaderInfoLog", &[s]).ok().and_then(|v| v.as_string()).unwrap_or_default())
        }
    }
}
