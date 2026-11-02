//! Applies the WebGL2 frame-start snapshot to a replay context.
//! Order: textures, buffers, renderbuffers, framebuffers, samplers, VAOs, programs,
//! default framebuffer contents, then global state last.
use super::c::*;
use super::formats::{ReadClass, canonical_format_type, is_depth_stencil};
use super::{Attach, GlCtx, n};
use crate::util::{get_num, get_str, num_list};
use crate::value::to_js;
use crate::{ObjMap, js};
use std::collections::{BTreeSet, HashMap};
use wasm_bindgen::JsValue;
use webgd_core::{Call, Capture, ObjId, TypedKind, Value};

struct Ctx<'a> {
    gl: &'a GlCtx,
    cap: &'a Capture,
    objs: &'a ObjMap,
    notes: &'a mut HashMap<ObjId, Vec<String>>,
    errs: &'a mut Vec<String>,
}

impl Ctx<'_> {
    fn js(&mut self, v: &Value) -> JsValue {
        match to_js(self.cap, v, self.objs) {
            Ok(j) => j,
            Err(e) => {
                self.errs.push(e.to_string());
                JsValue::NULL
            }
        }
    }
    fn call(&mut self, name: &str, args: &[JsValue]) {
        if let Err(e) = self.gl.call(name, args) {
            self.errs.push(format!("{name}: {}", js::err_msg(&e)));
        }
    }
    fn note(&mut self, id: ObjId, s: String) {
        let v = self.notes.entry(id).or_default();
        if !v.contains(&s) {
            v.push(s);
        }
    }
}

fn arg(c: &Call, i: usize) -> &Value {
    c.args.get(i).unwrap_or(&Value::Undefined)
}

pub fn apply(
    gl: &GlCtx,
    cap: &Capture,
    calls: &[&Call],
    objs: &ObjMap,
    notes: &mut HashMap<ObjId, Vec<String>>,
    errs: &mut Vec<String>,
) {
    let mut x = Ctx {
        gl,
        cap,
        objs,
        notes,
        errs,
    };
    let of = |m: &'static str| calls.iter().copied().filter(move |c| cap.method(c) == m);

    // Neutral transfer state for restores.
    for (p, d) in PIXEL_STORE.iter().zip(PIXEL_STORE_DEFAULTS) {
        gl.c("pixelStorei", &[*p as f64, d as f64]);
    }
    let _ = gl.call("bindBuffer", &[n(PIXEL_UNPACK_BUFFER), JsValue::NULL]);
    let _ = gl.call("bindBuffer", &[n(PIXEL_PACK_BUFFER), JsValue::NULL]);
    let _ = gl.call("bindVertexArray", &[JsValue::NULL]);
    gl.c("disable", &[SCISSOR_TEST as f64]);
    gl.c("disable", &[RASTERIZER_DISCARD as f64]);

    let mut tex_targets: HashMap<ObjId, u32> = HashMap::new();
    for c in of("webgd:glTexture") {
        if let Some(id) = arg(c, 0).as_obj() {
            tex_targets.insert(
                id,
                get_num(cap, arg(c, 1), "target").unwrap_or(TEXTURE_2D as f64) as u32,
            );
        }
        texture(&mut x, c);
    }
    for c in of("webgd:glBuffer") {
        buffer(&mut x, c);
    }
    for c in of("webgd:glRenderbuffer") {
        renderbuffer(&mut x, c);
    }
    for c in of("webgd:glFramebuffer") {
        framebuffer(&mut x, c, &tex_targets);
    }
    for c in of("webgd:glSampler") {
        let s = x.js(arg(c, 0));
        if let Value::Dict(entries) = arg(c, 1) {
            for (k, v) in entries {
                let Ok(p) = cap.str(*k).parse::<u32>() else {
                    continue;
                };
                let m = if matches!(p, TEXTURE_MIN_LOD | TEXTURE_MAX_LOD) {
                    "samplerParameterf"
                } else {
                    "samplerParameteri"
                };
                let v = x.js(v);
                x.call(m, &[s.clone(), n(p), v]);
            }
        }
    }
    for c in of("webgd:glVertexArray") {
        vertex_array(&mut x, c);
    }
    for c in of("webgd:glProgram") {
        program(&mut x, c);
    }
    for c in of("webgd:glDefaultFramebuffer") {
        let d = arg(c, 0);
        let (w, h) = (
            get_num(cap, d, "width").unwrap_or(0.0) as u32,
            get_num(cap, d, "height").unwrap_or(0.0) as u32,
        );
        match cap.dict_get(d, "data") {
            Some(v @ Value::Typed { .. }) if w > 0 && h > 0 => {
                let data = x.js(v);
                if let Err(e) = gl.blit_upload(ReadClass::Unorm, w, h, &data, &Attach::None) {
                    x.errs.push(format!("default framebuffer: {e}"));
                }
            }
            _ => x.note(gl.id, "pre-frame default framebuffer contents not captured (preserveDrawingBuffer is false)".into()),
        }
    }
    if let Some(c) = of("webgd:glState").next() {
        global_state(&mut x, arg(c, 0));
    }
}

fn texture(x: &mut Ctx, c: &Call) {
    let cap = x.cap;
    let Some(id) = arg(c, 0).as_obj() else { return };
    let tex = x.js(arg(c, 0));
    let d = arg(c, 1);
    let target = get_num(cap, d, "target").unwrap_or(TEXTURE_2D as f64) as u32;
    let ifmt = get_num(cap, d, "internalformat").unwrap_or(0.0) as u32;
    let immutable = get_num(cap, d, "immutable").unwrap_or(0.0) != 0.0;
    let imm_levels = get_num(cap, d, "immutableLevels").unwrap_or(0.0) as u32;
    let levels: &[Value] = cap
        .dict_get(d, "levels")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    let gl = x.gl;
    gl.c("activeTexture", &[(TEXTURE0 + gl.scratch_unit) as f64]);
    gl.bind_tex(target, &tex);

    let is_upload_log = |l: &Value| get_str(cap, l, "note") == Some("uploadLog");
    let needs_storage: Vec<&Value> = levels.iter().filter(|l| !is_upload_log(l)).collect();
    if !needs_storage.is_empty() && ifmt != 0 {
        let fmt_ty = match (get_num(cap, d, "format"), get_num(cap, d, "type")) {
            (Some(f), Some(t)) => Some((f as u32, t as u32)),
            _ => canonical_format_type(ifmt),
        };
        let three_d = matches!(target, TEXTURE_3D | TEXTURE_2D_ARRAY);
        let dims = |l: &Value| {
            (
                get_num(cap, l, "level").unwrap_or(0.0) as u32,
                get_num(cap, l, "face").unwrap_or(0.0) as u32,
                get_num(cap, l, "width").unwrap_or(1.0) as u32,
                get_num(cap, l, "height").unwrap_or(1.0) as u32,
                get_num(cap, l, "depth").unwrap_or(1.0) as u32,
            )
        };
        if immutable {
            let (lv, _, w, h, dp) = levels
                .iter()
                .map(dims)
                .min_by_key(|t| t.0)
                .unwrap_or((0, 0, 1, 1, 1));
            let (w0, h0) = ((w << lv).max(1), (h << lv).max(1));
            let d0 = if target == TEXTURE_3D {
                (dp << lv).max(1)
            } else {
                dp
            };
            let nl = imm_levels.max(1);
            if three_d {
                x.call(
                    "texStorage3D",
                    &[n(target), n(nl), n(ifmt), n(w0), n(h0), n(d0)],
                );
            } else {
                x.call("texStorage2D", &[n(target), n(nl), n(ifmt), n(w0), n(h0)]);
            }
        } else if let Some((fmt, ty)) = fmt_ty {
            let mut done = BTreeSet::new();
            for l in &needs_storage {
                let (lv, face, w, h, dp) = dims(l);
                if !done.insert((lv, face)) {
                    continue;
                }
                if three_d {
                    x.call(
                        "texImage3D",
                        &[
                            n(target),
                            n(lv),
                            n(ifmt),
                            n(w),
                            n(h),
                            n(dp),
                            n(0),
                            n(fmt),
                            n(ty),
                            JsValue::NULL,
                        ],
                    );
                } else {
                    let t = if target == TEXTURE_CUBE_MAP {
                        TEXTURE_CUBE_MAP_POSITIVE_X + face
                    } else {
                        target
                    };
                    x.call(
                        "texImage2D",
                        &[
                            n(t),
                            n(lv),
                            n(ifmt),
                            n(w),
                            n(h),
                            n(0),
                            n(fmt),
                            n(ty),
                            JsValue::NULL,
                        ],
                    );
                }
            }
        } else {
            x.errs
                .push(format!("texture {}: unknown format 0x{ifmt:X}", id.0));
        }
    }
    if let Some(Value::Dict(params)) = cap.dict_get(d, "params") {
        for (k, v) in params {
            let Ok(p) = cap.str(*k).parse::<u32>() else {
                continue;
            };
            let m = if matches!(p, TEXTURE_MIN_LOD | TEXTURE_MAX_LOD) {
                "texParameterf"
            } else {
                "texParameteri"
            };
            let v = x.js(v);
            x.call(m, &[n(target), n(p), v]);
        }
    }
    for l in levels {
        let data = cap.dict_get(l, "data").unwrap_or(&Value::Null);
        if is_upload_log(l) {
            continue;
        }
        if data.is_nullish() {
            let why = if is_depth_stencil(ifmt) {
                "depth/stencil"
            } else {
                "not readable"
            };
            x.note(id, format!("pre-frame contents not captured ({why})"));
            continue;
        }
        let class = get_str(cap, l, "readClass")
            .and_then(ReadClass::parse)
            .unwrap_or(ReadClass::Unorm);
        let g = |k: &str| get_num(cap, l, k).unwrap_or(0.0) as u32;
        let att = Attach::Tex {
            tex: tex.clone(),
            target,
            level: g("level"),
            face: g("face"),
            layer: g("layer"),
        };
        let data = x.js(data);
        if let Err(e) =
            x.gl.blit_upload(class, g("width"), g("height"), &data, &att)
        {
            x.errs
                .push(format!("texture {} level {}: {e}", id.0, g("level")));
            x.note(id, format!("pre-frame contents not restored ({e})"));
        }
    }
}

fn buffer(x: &mut Ctx, c: &Call) {
    let cap = x.cap;
    let buf = x.js(arg(c, 0));
    let d = arg(c, 1);
    let usage = get_num(cap, d, "usage").unwrap_or(0x88E4 as f64);
    let is_index = get_num(cap, d, "isIndex").unwrap_or(0.0) != 0.0;
    let target = if is_index {
        ELEMENT_ARRAY_BUFFER
    } else {
        ARRAY_BUFFER
    };
    if is_index {
        let _ =
            x.gl.call("bindVertexArray", std::slice::from_ref(&x.gl.scratch_vao));
    }
    x.call("bindBuffer", &[n(target), buf]);
    let data = match cap.dict_get(d, "data") {
        Some(v @ Value::Typed { .. }) => x.js(v),
        _ => js::num(get_num(cap, d, "size").unwrap_or(0.0)),
    };
    x.call("bufferData", &[n(target), data, js::num(usage)]);
    x.call("bindBuffer", &[n(target), JsValue::NULL]);
    if is_index {
        let _ = x.gl.call("bindVertexArray", &[JsValue::NULL]);
    }
}

fn renderbuffer(x: &mut Ctx, c: &Call) {
    let cap = x.cap;
    let Some(id) = arg(c, 0).as_obj() else { return };
    let rb = x.js(arg(c, 0));
    let d = arg(c, 1);
    let g = |k: &str| get_num(cap, d, k).unwrap_or(0.0) as u32;
    let (w, h, ifmt, samples) = (g("width"), g("height"), g("internalformat"), g("samples"));
    if w == 0 || h == 0 || ifmt == 0 {
        return;
    }
    x.call("bindRenderbuffer", &[n(RENDERBUFFER), rb.clone()]);
    x.call(
        "renderbufferStorageMultisample",
        &[n(RENDERBUFFER), n(samples), n(ifmt), n(w), n(h)],
    );
    match cap.dict_get(d, "data") {
        Some(v @ Value::Typed { .. }) => {
            let class = get_str(cap, d, "readClass")
                .and_then(ReadClass::parse)
                .unwrap_or(ReadClass::Unorm);
            let data = x.js(v);
            if let Err(e) = x.gl.blit_upload(class, w, h, &data, &Attach::Rb(rb)) {
                x.errs.push(format!("renderbuffer {}: {e}", id.0));
            }
        }
        _ => {
            let why = if is_depth_stencil(ifmt) {
                "depth/stencil"
            } else if samples > 0 {
                "multisampled"
            } else {
                "not readable"
            };
            x.note(id, format!("pre-frame contents not captured ({why})"));
        }
    }
    x.call("bindRenderbuffer", &[n(RENDERBUFFER), JsValue::NULL]);
}

fn framebuffer(x: &mut Ctx, c: &Call, tex_targets: &HashMap<ObjId, u32>) {
    let cap = x.cap;
    let fb = x.js(arg(c, 0));
    let d = arg(c, 1);
    x.gl.bind_fb(FRAMEBUFFER, &fb);
    if let Some(Value::Dict(atts)) = cap.dict_get(d, "attachments") {
        let keys: Vec<u32> = atts
            .iter()
            .filter_map(|(k, _)| cap.str(*k).parse().ok())
            .collect();
        let has_split_ds = keys.contains(&DEPTH_ATTACHMENT) || keys.contains(&STENCIL_ATTACHMENT);
        for (k, a) in atts {
            let Ok(att) = cap.str(*k).parse::<u32>() else {
                continue;
            };
            if att == DEPTH_STENCIL_ATTACHMENT && has_split_ds {
                continue;
            }
            let ty = get_num(cap, a, "objectType").unwrap_or(0.0) as u32;
            let obj_v = cap.dict_get(a, "object").unwrap_or(&Value::Null);
            let obj = x.js(obj_v);
            let g = |k: &str| get_num(cap, a, k).unwrap_or(0.0) as u32;
            let src = match ty {
                TEXTURE => {
                    let target = obj_v
                        .as_obj()
                        .and_then(|id| tex_targets.get(&id).copied())
                        .unwrap_or(TEXTURE_2D);
                    let face = g("face");
                    let face = if face >= TEXTURE_CUBE_MAP_POSITIVE_X {
                        face - TEXTURE_CUBE_MAP_POSITIVE_X
                    } else {
                        face
                    };
                    Attach::Tex {
                        tex: obj,
                        target,
                        level: g("level"),
                        face,
                        layer: g("layer"),
                    }
                }
                RENDERBUFFER => Attach::Rb(obj),
                _ => continue,
            };
            super::attach(x.gl, FRAMEBUFFER, att, &src);
        }
    }
    if let Some(db) = cap.dict_get(d, "drawBuffers") {
        let list: Vec<JsValue> = num_list(cap, db).into_iter().map(js::num).collect();
        if !list.is_empty() {
            x.call("drawBuffers", &[js::arr(&list)]);
        }
    }
    if let Some(rb) = get_num(cap, d, "readBuffer") {
        x.call("readBuffer", &[js::num(rb)]);
    }
    x.gl.bind_fb(FRAMEBUFFER, &JsValue::NULL);
}

fn vertex_array(x: &mut Ctx, c: &Call) {
    let cap = x.cap;
    let vao = x.js(arg(c, 0));
    let d = arg(c, 1);
    x.call("bindVertexArray", &[vao]);
    let attribs: &[Value] = cap
        .dict_get(d, "attribs")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for (i, a) in attribs.iter().enumerate() {
        let g = |k: &str| get_num(cap, a, k).unwrap_or(0.0);
        let buf_v = cap.dict_get(a, "buffer").unwrap_or(&Value::Null);
        if !buf_v.is_nullish() {
            let buf = x.js(buf_v);
            x.call("bindBuffer", &[n(ARRAY_BUFFER), buf]);
            let size = js::num(g("size").max(1.0));
            if g("integer") != 0.0 {
                x.call(
                    "vertexAttribIPointer",
                    &[
                        n(i as u32),
                        size,
                        js::num(g("type")),
                        js::num(g("stride")),
                        js::num(g("offset")),
                    ],
                );
            } else {
                x.call(
                    "vertexAttribPointer",
                    &[
                        n(i as u32),
                        size,
                        js::num(g("type")),
                        JsValue::from_bool(g("normalized") != 0.0),
                        js::num(g("stride")),
                        js::num(g("offset")),
                    ],
                );
            }
        }
        let m = if g("enabled") != 0.0 {
            "enableVertexAttribArray"
        } else {
            "disableVertexAttribArray"
        };
        x.call(m, &[n(i as u32)]);
        x.call("vertexAttribDivisor", &[n(i as u32), js::num(g("divisor"))]);
    }
    let eb = cap.dict_get(d, "elementBuffer").unwrap_or(&Value::Null);
    let eb = x.js(eb);
    x.call("bindBuffer", &[n(ELEMENT_ARRAY_BUFFER), eb]);
    x.call("bindBuffer", &[n(ARRAY_BUFFER), JsValue::NULL]);
    x.call("bindVertexArray", &[JsValue::NULL]);
}

fn program(x: &mut Ctx, c: &Call) {
    let cap = x.cap;
    let p = x.js(arg(c, 0));
    x.call("useProgram", std::slice::from_ref(&p));
    for u in arg(c, 1).as_array().unwrap_or(&[]) {
        let Some([Value::String(name), ty, value]) =
            u.as_array().and_then(|a| <&[Value; 3]>::try_from(a).ok())
        else {
            continue;
        };
        let loc =
            x.gl.call("getUniformLocation", &[p.clone(), cap.str(*name).into()])
                .unwrap_or(JsValue::NULL);
        if loc.is_null() {
            continue;
        }
        let v = x.js(value);
        if let Err(e) =
            super::uniforms::set_uniform(&x.gl.gl, ty.as_f64().unwrap_or(0.0) as u32, &loc, &v)
        {
            x.errs
                .push(format!("uniform {}: {}", cap.str(*name), js::err_msg(&e)));
        }
    }
    if let Value::Dict(blocks) = arg(c, 2) {
        for (name, b) in blocks {
            let idx =
                x.gl.call("getUniformBlockIndex", &[p.clone(), cap.str(*name).into()])
                    .unwrap_or_default();
            if js::f(&idx) as u32 == u32::MAX || idx.is_undefined() {
                continue;
            }
            x.call(
                "uniformBlockBinding",
                &[p.clone(), idx, js::num(b.as_f64().unwrap_or(0.0))],
            );
        }
    }
    x.call("useProgram", &[JsValue::NULL]);
}

fn global_state(x: &mut Ctx, st: &Value) {
    let cap = x.cap;
    let key = |e: u32| cap.dict_get(st, &e.to_string());
    let nums = |e: u32| key(e).map(|v| num_list(cap, v)).unwrap_or_default();
    let num = |e: u32| key(e).and_then(Value::as_f64);
    let gl = x.gl;
    let call_nums = |x: &mut Ctx, m: &str, vals: &[Option<f64>]| {
        if vals.iter().all(Option::is_some) {
            let a: Vec<JsValue> = vals.iter().map(|v| js::num(v.unwrap_or(0.0))).collect();
            x.call(m, &a);
        }
    };

    for cap_e in CAPS {
        if let Some(v) = key(cap_e) {
            let on = v.as_f64().unwrap_or(0.0) != 0.0;
            gl.c(if on { "enable" } else { "disable" }, &[cap_e as f64]);
        }
    }
    let bc = nums(BLEND_COLOR);
    if bc.len() == 4 {
        gl.c("blendColor", &bc);
    }
    call_nums(
        x,
        "blendEquationSeparate",
        &[num(BLEND_EQUATION_RGB), num(BLEND_EQUATION_ALPHA)],
    );
    call_nums(
        x,
        "blendFuncSeparate",
        &[
            num(BLEND_SRC_RGB),
            num(BLEND_DST_RGB),
            num(BLEND_SRC_ALPHA),
            num(BLEND_DST_ALPHA),
        ],
    );
    let cm = nums(COLOR_WRITEMASK);
    if cm.len() == 4 {
        let a: Vec<JsValue> = cm.iter().map(|v| JsValue::from_bool(*v != 0.0)).collect();
        x.call("colorMask", &a);
    }
    call_nums(x, "cullFace", &[num(CULL_FACE_MODE)]);
    call_nums(x, "depthFunc", &[num(DEPTH_FUNC)]);
    if let Some(v) = num(DEPTH_WRITEMASK) {
        x.call("depthMask", &[JsValue::from_bool(v != 0.0)]);
    }
    let dr = nums(DEPTH_RANGE);
    if dr.len() == 2 {
        gl.c("depthRange", &dr);
    }
    call_nums(x, "frontFace", &[num(FRONT_FACE)]);
    call_nums(x, "lineWidth", &[num(LINE_WIDTH)]);
    call_nums(
        x,
        "polygonOffset",
        &[num(POLYGON_OFFSET_FACTOR), num(POLYGON_OFFSET_UNITS)],
    );
    if let (Some(v), Some(inv)) = (num(SAMPLE_COVERAGE_VALUE), num(SAMPLE_COVERAGE_INVERT)) {
        x.call(
            "sampleCoverage",
            &[js::num(v), JsValue::from_bool(inv != 0.0)],
        );
    }
    let sb = nums(SCISSOR_BOX);
    if sb.len() == 4 {
        gl.c("scissor", &sb);
    }
    let vp = nums(VIEWPORT);
    if vp.len() == 4 {
        gl.c("viewport", &vp);
    }
    let front = Some(FRONT as f64);
    let back = Some(BACK as f64);
    call_nums(
        x,
        "stencilFuncSeparate",
        &[
            front,
            num(STENCIL_FUNC),
            num(STENCIL_REF),
            num(STENCIL_VALUE_MASK),
        ],
    );
    call_nums(
        x,
        "stencilFuncSeparate",
        &[
            back,
            num(STENCIL_BACK_FUNC),
            num(STENCIL_BACK_REF),
            num(STENCIL_BACK_VALUE_MASK),
        ],
    );
    call_nums(
        x,
        "stencilOpSeparate",
        &[
            front,
            num(STENCIL_FAIL),
            num(STENCIL_PASS_DEPTH_FAIL),
            num(STENCIL_PASS_DEPTH_PASS),
        ],
    );
    call_nums(
        x,
        "stencilOpSeparate",
        &[
            back,
            num(STENCIL_BACK_FAIL),
            num(STENCIL_BACK_PASS_DEPTH_FAIL),
            num(STENCIL_BACK_PASS_DEPTH_PASS),
        ],
    );
    call_nums(x, "stencilMaskSeparate", &[front, num(STENCIL_WRITEMASK)]);
    call_nums(
        x,
        "stencilMaskSeparate",
        &[back, num(STENCIL_BACK_WRITEMASK)],
    );
    let cc = nums(COLOR_CLEAR_VALUE);
    if cc.len() == 4 {
        gl.c("clearColor", &cc);
    }
    call_nums(x, "clearDepth", &[num(DEPTH_CLEAR_VALUE)]);
    call_nums(x, "clearStencil", &[num(STENCIL_CLEAR_VALUE)]);
    call_nums(
        x,
        "hint",
        &[Some(GENERATE_MIPMAP_HINT as f64), num(GENERATE_MIPMAP_HINT)],
    );
    call_nums(
        x,
        "hint",
        &[
            Some(FRAGMENT_SHADER_DERIVATIVE_HINT as f64),
            num(FRAGMENT_SHADER_DERIVATIVE_HINT),
        ],
    );
    for p in PIXEL_STORE {
        if let Some(v) = key(p) {
            let v = x.js(v);
            x.call("pixelStorei", &[n(p), v]);
        }
    }
    if let Some(Value::Array(attrs)) = cap.dict_get(st, "currentAttribs") {
        for (i, a) in attrs.iter().enumerate() {
            let m = match a {
                Value::Typed {
                    kind: TypedKind::Int32,
                    ..
                } => "vertexAttribI4iv",
                Value::Typed {
                    kind: TypedKind::Uint32,
                    ..
                } => "vertexAttribI4uiv",
                Value::Typed { .. } => "vertexAttrib4fv",
                _ => continue,
            };
            let v = x.js(a);
            x.call(m, &[n(i as u32), v]);
        }
    }
    if let Some(Value::Array(units)) = cap.dict_get(st, "units") {
        for (u, d) in units.iter().enumerate() {
            gl.c("activeTexture", &[(TEXTURE0 + u as u32) as f64]);
            for (k, t) in [
                ("2d", TEXTURE_2D),
                ("cube", TEXTURE_CUBE_MAP),
                ("3d", TEXTURE_3D),
                ("array", TEXTURE_2D_ARRAY),
            ] {
                let v = cap.dict_get(d, k).unwrap_or(&Value::Null);
                let v = x.js(v);
                x.call("bindTexture", &[n(t), v]);
            }
            let s = cap.dict_get(d, "sampler").unwrap_or(&Value::Null);
            let s = x.js(s);
            x.call("bindSampler", &[n(u as u32), s]);
        }
    }
    if let Some(Value::Array(ubos)) = cap.dict_get(st, "ubo") {
        for (i, d) in ubos.iter().enumerate() {
            let b = cap.dict_get(d, "buffer").unwrap_or(&Value::Null);
            if b.is_nullish() {
                continue;
            }
            let b = x.js(b);
            let start = get_num(cap, d, "start").unwrap_or(0.0);
            let size = get_num(cap, d, "size").unwrap_or(0.0);
            if size > 0.0 {
                x.call(
                    "bindBufferRange",
                    &[
                        n(UNIFORM_BUFFER),
                        n(i as u32),
                        b,
                        js::num(start),
                        js::num(size),
                    ],
                );
            } else {
                x.call("bindBufferBase", &[n(UNIFORM_BUFFER), n(i as u32), b]);
            }
        }
    }
    let bind = |x: &mut Ctx, m: &str, target: u32, pname: u32| {
        if let Some(v) = key(pname) {
            let v = x.js(v);
            x.call(m, &[n(target), v]);
        }
    };
    bind(x, "bindBuffer", ARRAY_BUFFER, ARRAY_BUFFER_BINDING);
    bind(x, "bindBuffer", COPY_READ_BUFFER, COPY_READ_BUFFER_BINDING);
    bind(
        x,
        "bindBuffer",
        COPY_WRITE_BUFFER,
        COPY_WRITE_BUFFER_BINDING,
    );
    bind(
        x,
        "bindBuffer",
        PIXEL_PACK_BUFFER,
        PIXEL_PACK_BUFFER_BINDING,
    );
    bind(
        x,
        "bindBuffer",
        PIXEL_UNPACK_BUFFER,
        PIXEL_UNPACK_BUFFER_BINDING,
    );
    bind(x, "bindBuffer", UNIFORM_BUFFER, UNIFORM_BUFFER_BINDING);
    if key(TRANSFORM_FEEDBACK_BUFFER_BINDING).is_some_and(|v| !v.is_nullish()) {
        bind(
            x,
            "bindBuffer",
            TRANSFORM_FEEDBACK_BUFFER,
            TRANSFORM_FEEDBACK_BUFFER_BINDING,
        );
    }
    bind(x, "bindRenderbuffer", RENDERBUFFER, RENDERBUFFER_BINDING);
    bind(
        x,
        "bindFramebuffer",
        DRAW_FRAMEBUFFER,
        DRAW_FRAMEBUFFER_BINDING,
    );
    bind(
        x,
        "bindFramebuffer",
        READ_FRAMEBUFFER,
        READ_FRAMEBUFFER_BINDING,
    );
    if let Some(v) = key(VERTEX_ARRAY_BINDING) {
        let v = x.js(v);
        x.call("bindVertexArray", &[v]);
    }
    if let Some(v) = key(CURRENT_PROGRAM) {
        let v = x.js(v);
        x.call("useProgram", &[v]);
    }
    call_nums(x, "activeTexture", &[num(ACTIVE_TEXTURE)]);
}
