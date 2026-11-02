//! WebGL2 inspection (targets, pipeline state, bindings) and readback.
use super::c::*;
use super::formats::{ReadClass, read_class};
use super::uniforms::{is_sampler, sampler_binding};
use super::{Attach, GlCtx, enum_name, n};
use crate::{BindingRow, Inspection, ProgramInfo, ReplayState, StateNode, TargetView, js};
use wasm_bindgen::JsValue;
use webgd_core::display::obj_label;
use webgd_core::pixels::Image;
use webgd_core::track::gl::texture_dims;
use webgd_core::{Capture, ObjId};

fn leaf(name: &str, value: String) -> StateNode {
    StateNode::Leaf { name: name.to_string(), value }
}

fn group(name: &str, children: Vec<StateNode>) -> StateNode {
    StateNode::Group { name: name.to_string(), children }
}

/// Formats a getParameter-style value; objects are labelled via the reverse map.
fn fmt(st: &ReplayState, cap: &Capture, v: &JsValue) -> String {
    if v.is_null() || v.is_undefined() {
        return "null".into();
    }
    if let Some(b) = v.as_bool() {
        return b.to_string();
    }
    if let Some(f) = v.as_f64() {
        return format!("{f}");
    }
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(id) = st.objs.id_of(v) {
        return obj_label(cap, id);
    }
    if js::is_obj(&js::get(v, "length")) || js::get(v, "length").as_f64().is_some() {
        let items: Vec<String> = js_sys::Array::from(v).iter().map(|x| fmt(st, cap, &x)).collect();
        return format!("[{}]", items.join(", "));
    }
    js::get(&js::get(v, "constructor"), "name").as_string().unwrap_or_else(|| "object".into())
}

fn fmt_enum(v: &JsValue) -> String {
    match v.as_f64() {
        Some(x) => enum_name(x as u32),
        None => "null".into(),
    }
}

/// Blend factors / stencil ops: 0 and 1 mean ZERO / ONE there (not POINTS / LINES).
fn fmt_factor(v: &JsValue) -> String {
    match v.as_f64().map(|x| x as u32) {
        Some(0) => "ZERO".into(),
        Some(1) => "ONE".into(),
        _ => fmt_enum(v),
    }
}

pub fn inspect(st: &ReplayState, cap: &Capture, index: usize, ctx: &GlCtx) -> Inspection {
    let mut ins = Inspection { index, targets: Vec::new(), pipeline: Vec::new(), bindings: Vec::new(), notes: Vec::new() };
    targets(st, cap, index, ctx, &mut ins);
    pipeline(st, cap, ctx, &mut ins);
    bindings(st, cap, ctx, &mut ins);
    ins
}

fn attachment_param(ctx: &GlCtx, target: u32, att: u32, pname: u32) -> JsValue {
    ctx.call("getFramebufferAttachmentParameter", &[n(target), n(att), n(pname)]).unwrap_or(JsValue::NULL)
}

fn class_of_attachment(ctx: &GlCtx, target: u32, att: u32) -> ReadClass {
    match js::u(&attachment_param(ctx, target, att, FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE)) {
        FLOAT => ReadClass::Float,
        INT => ReadClass::Int,
        UNSIGNED_INT => ReadClass::Uint,
        _ if js::u(&attachment_param(ctx, target, att, FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING)) == SRGB => ReadClass::Srgb,
        _ => ReadClass::Unorm,
    }
}

fn rb_info(ctx: &GlCtx, rb: &JsValue) -> (u32, u32, u32, u32) {
    let prev = ctx.param(RENDERBUFFER_BINDING);
    let _ = ctx.call("bindRenderbuffer", &[n(RENDERBUFFER), rb.clone()]);
    let p = |pn: u32| js::u(&ctx.call("getRenderbufferParameter", &[n(RENDERBUFFER), n(pn)]).unwrap_or_default());
    let r = (p(RENDERBUFFER_WIDTH), p(RENDERBUFFER_HEIGHT), p(RENDERBUFFER_INTERNAL_FORMAT), p(RENDERBUFFER_SAMPLES));
    let _ = ctx.call("bindRenderbuffer", &[n(RENDERBUFFER), prev]);
    r
}

fn targets(st: &ReplayState, cap: &Capture, index: usize, ctx: &GlCtx, ins: &mut Inspection) {
    let fb = ctx.param(DRAW_FRAMEBUFFER_BINDING);
    if fb.is_null() {
        let w = js::u(&js::get(&ctx.gl, "drawingBufferWidth"));
        let h = js::u(&js::get(&ctx.gl, "drawingBufferHeight"));
        let prev = ctx.param(READ_FRAMEBUFFER_BINDING);
        ctx.bind_fb(READ_FRAMEBUFFER, &JsValue::NULL);
        let r = ctx.read_pixels(w, h, ReadClass::Unorm);
        ctx.bind_fb(READ_FRAMEBUFFER, &prev);
        let (image, note) = match r {
            Ok(i) => (Some(i), None),
            Err(e) => (None, Some(e)),
        };
        ins.targets.push(TargetView { label: "Default framebuffer".into(), resource: None, image, note });
        return;
    }
    let max_db = js::u(&ctx.param(MAX_DRAW_BUFFERS));
    for i in 0..max_db {
        let db = js::u(&ctx.param(DRAW_BUFFER0 + i));
        if db == NONE {
            continue;
        }
        let ty = js::u(&attachment_param(ctx, DRAW_FRAMEBUFFER, db, FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE));
        if ty == NONE {
            continue;
        }
        let obj = attachment_param(ctx, DRAW_FRAMEBUFFER, db, FRAMEBUFFER_ATTACHMENT_OBJECT_NAME);
        let id = st.objs.id_of(&obj);
        let class = class_of_attachment(ctx, DRAW_FRAMEBUFFER, db);
        let label = format!("{} (draw buffer {i})", enum_name(db));
        let r: Result<Image, String> = if ty == RENDERBUFFER {
            let (w, h, _, samples) = rb_info(ctx, &obj);
            if samples > 0 {
                Err("multisampled renderbuffer not readable".into())
            } else {
                ctx.read_attachment(&Attach::Rb(obj.clone()), w, h, class)
            }
        } else {
            let level = js::u(&attachment_param(ctx, DRAW_FRAMEBUFFER, db, FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL));
            let face_e = js::u(&attachment_param(ctx, DRAW_FRAMEBUFFER, db, FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE));
            let layer = js::u(&attachment_param(ctx, DRAW_FRAMEBUFFER, db, FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER));
            let face = face_e.checked_sub(TEXTURE_CUBE_MAP_POSITIVE_X).filter(|f| *f < 6).unwrap_or(0);
            match id.and_then(|id| texture_dims(cap, index, id)) {
                Some(d) => match d.levels.get(&(level, face)) {
                    Some(&(w, h, _)) => {
                        ctx.read_attachment(&Attach::Tex { tex: obj.clone(), target: d.target, level, face, layer }, w, h, class)
                    }
                    None => Err(format!("level {level} dimensions unknown")),
                },
                None => Err("texture dimensions unknown".into()),
            }
        };
        let (image, note) = match r {
            Ok(i) => (Some(i), None),
            Err(e) => (None, Some(e)),
        };
        ins.targets.push(TargetView { label, resource: id, image, note });
    }
    for att in [DEPTH_ATTACHMENT, STENCIL_ATTACHMENT, DEPTH_STENCIL_ATTACHMENT] {
        if att == DEPTH_STENCIL_ATTACHMENT && ins.targets.iter().any(|t| t.label.starts_with("DEPTH") || t.label.starts_with("STENCIL")) {
            continue;
        }
        let ty = js::u(&attachment_param(ctx, DRAW_FRAMEBUFFER, att, FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE));
        if ty == NONE {
            continue;
        }
        let obj = attachment_param(ctx, DRAW_FRAMEBUFFER, att, FRAMEBUFFER_ATTACHMENT_OBJECT_NAME);
        ins.targets.push(TargetView {
            label: enum_name(att),
            resource: st.objs.id_of(&obj),
            image: None,
            note: Some("depth/stencil not readable in WebGL2".into()),
        });
    }
}

fn pipeline(st: &ReplayState, cap: &Capture, ctx: &GlCtx, ins: &mut Inspection) {
    let p = |e: u32| ctx.param(e);
    let en = |e: u32| js::truthy(&ctx.call("isEnabled", &[n(e)]).unwrap_or_default()).to_string();
    let v = |e: u32| fmt(st, cap, &p(e));
    let e = |x: u32| fmt_enum(&p(x));
    let fac = |x: u32| fmt_factor(&p(x));
    let prog = p(CURRENT_PROGRAM);
    let mut prog_children = vec![leaf("CURRENT_PROGRAM", fmt(st, cap, &prog))];
    if js::is_obj(&prog) {
        let link = ctx.call("getProgramParameter", &[prog.clone(), n(LINK_STATUS)]).unwrap_or_default();
        prog_children.push(leaf("LINK_STATUS", fmt(st, cap, &link)));
    }
    let stencil = |back: bool| {
        let (f, r, m, sf, zf, zp, w) = if back {
            (STENCIL_BACK_FUNC, STENCIL_BACK_REF, STENCIL_BACK_VALUE_MASK, STENCIL_BACK_FAIL, STENCIL_BACK_PASS_DEPTH_FAIL, STENCIL_BACK_PASS_DEPTH_PASS, STENCIL_BACK_WRITEMASK)
        } else {
            (STENCIL_FUNC, STENCIL_REF, STENCIL_VALUE_MASK, STENCIL_FAIL, STENCIL_PASS_DEPTH_FAIL, STENCIL_PASS_DEPTH_PASS, STENCIL_WRITEMASK)
        };
        group(
            if back { "Back" } else { "Front" },
            vec![
                leaf("func", e(f)),
                leaf("ref", v(r)),
                leaf("valueMask", v(m)),
                leaf("fail", fac(sf)),
                leaf("passDepthFail", fac(zf)),
                leaf("passDepthPass", fac(zp)),
                leaf("writeMask", v(w)),
            ],
        )
    };
    let max_db = js::u(&p(MAX_DRAW_BUFFERS));
    let mut dbs = vec![leaf("DRAW_FRAMEBUFFER_BINDING", v(DRAW_FRAMEBUFFER_BINDING))];
    for i in 0..max_db {
        let d = js::u(&p(DRAW_BUFFER0 + i));
        if d != NONE {
            dbs.push(leaf(&format!("DRAW_BUFFER{i}"), enum_name(d)));
        }
    }
    dbs.push(leaf("READ_BUFFER", e(READ_BUFFER)));
    ins.pipeline = vec![
        group("Program", prog_children),
        group(
            "Rasterizer",
            vec![
                leaf("CULL_FACE", en(CULL_FACE)),
                leaf("CULL_FACE_MODE", e(CULL_FACE_MODE)),
                leaf("FRONT_FACE", e(FRONT_FACE)),
                leaf("LINE_WIDTH", v(LINE_WIDTH)),
                leaf("POLYGON_OFFSET_FILL", en(POLYGON_OFFSET_FILL)),
                leaf("POLYGON_OFFSET_FACTOR", v(POLYGON_OFFSET_FACTOR)),
                leaf("POLYGON_OFFSET_UNITS", v(POLYGON_OFFSET_UNITS)),
                leaf("RASTERIZER_DISCARD", en(RASTERIZER_DISCARD)),
            ],
        ),
        group(
            "Depth",
            vec![
                leaf("DEPTH_TEST", en(DEPTH_TEST)),
                leaf("DEPTH_FUNC", e(DEPTH_FUNC)),
                leaf("DEPTH_WRITEMASK", v(DEPTH_WRITEMASK)),
                leaf("DEPTH_RANGE", v(DEPTH_RANGE)),
            ],
        ),
        group("Stencil", vec![leaf("STENCIL_TEST", en(STENCIL_TEST)), stencil(false), stencil(true)]),
        group(
            "Blend",
            vec![
                leaf("BLEND", en(BLEND)),
                leaf("BLEND_EQUATION_RGB", e(BLEND_EQUATION_RGB)),
                leaf("BLEND_EQUATION_ALPHA", e(BLEND_EQUATION_ALPHA)),
                leaf("BLEND_SRC_RGB", fac(BLEND_SRC_RGB)),
                leaf("BLEND_DST_RGB", fac(BLEND_DST_RGB)),
                leaf("BLEND_SRC_ALPHA", fac(BLEND_SRC_ALPHA)),
                leaf("BLEND_DST_ALPHA", fac(BLEND_DST_ALPHA)),
                leaf("BLEND_COLOR", v(BLEND_COLOR)),
            ],
        ),
        group("ColorMask", vec![leaf("COLOR_WRITEMASK", v(COLOR_WRITEMASK)), leaf("DITHER", en(DITHER))]),
        group(
            "Viewport/Scissor",
            vec![
                leaf("VIEWPORT", v(VIEWPORT)),
                leaf("SCISSOR_TEST", en(SCISSOR_TEST)),
                leaf("SCISSOR_BOX", v(SCISSOR_BOX)),
            ],
        ),
        group(
            "Multisample",
            vec![
                leaf("SAMPLES", v(SAMPLES)),
                leaf("SAMPLE_BUFFERS", v(SAMPLE_BUFFERS)),
                leaf("SAMPLE_ALPHA_TO_COVERAGE", en(SAMPLE_ALPHA_TO_COVERAGE)),
                leaf("SAMPLE_COVERAGE", en(SAMPLE_COVERAGE)),
                leaf("SAMPLE_COVERAGE_VALUE", v(SAMPLE_COVERAGE_VALUE)),
                leaf("SAMPLE_COVERAGE_INVERT", v(SAMPLE_COVERAGE_INVERT)),
            ],
        ),
        group("DrawBuffers", dbs),
    ];
}

struct ActiveVar {
    name: String,
    size: u32,
    ty: u32,
}

fn active(ctx: &GlCtx, prog: &JsValue, method: &str, i: u32) -> Option<ActiveVar> {
    let info = ctx.call(method, &[prog.clone(), n(i)]).ok().filter(js::is_obj)?;
    Some(ActiveVar { name: js::get(&info, "name").as_string()?, size: js::u(&js::get(&info, "size")), ty: js::u(&js::get(&info, "type")) })
}

/// Expanded element names of an active uniform (`a[0]` with size 3 → `a[0]`, `a[1]`, `a[2]`).
fn element_names(v: &ActiveVar) -> Vec<String> {
    if v.size <= 1 {
        return vec![v.name.clone()];
    }
    let base = v.name.strip_suffix("[0]").unwrap_or(&v.name);
    (0..v.size).map(|i| format!("{base}[{i}]")).collect()
}

/// Active non-block uniforms of `prog` as (element name, type).
pub fn plain_uniforms(ctx: &GlCtx, prog: &JsValue) -> Vec<(String, u32)> {
    let count = js::u(&ctx.call("getProgramParameter", &[prog.clone(), n(ACTIVE_UNIFORMS)]).unwrap_or_default());
    let mut out = Vec::new();
    for i in 0..count {
        let Some(var) = active(ctx, prog, "getActiveUniform", i) else { continue };
        let block = ctx
            .call("getActiveUniforms", &[prog.clone(), js::arr(&[n(i)]), n(UNIFORM_BLOCK_INDEX)])
            .map(|a| js::get_idx(&a, 0))
            .unwrap_or_default();
        if js::f(&block) >= 0.0 {
            continue;
        }
        for name in element_names(&var) {
            out.push((name, var.ty));
        }
    }
    out
}

fn bindings(st: &ReplayState, cap: &Capture, ctx: &GlCtx, ins: &mut Inspection) {
    let label = |v: &JsValue| fmt(st, cap, v);
    let prog = ctx.param(CURRENT_PROGRAM);
    if !js::is_obj(&prog) {
        ins.notes.push("no program bound".into());
    }
    // Attributes.
    if js::is_obj(&prog) {
        let count = js::u(&ctx.call("getProgramParameter", &[prog.clone(), n(ACTIVE_ATTRIBUTES)]).unwrap_or_default());
        for i in 0..count {
            let Some(var) = active(ctx, &prog, "getActiveAttrib", i) else { continue };
            let loc = js::f(&ctx.call("getAttribLocation", &[prog.clone(), var.name.as_str().into()]).unwrap_or_default());
            if loc < 0.0 {
                continue;
            }
            let a = |pn: u32| ctx.call("getVertexAttrib", &[js::num(loc), n(pn)]).unwrap_or_default();
            let buf = a(VERTEX_ATTRIB_ARRAY_BUFFER_BINDING);
            let enabled = js::truthy(&a(VERTEX_ATTRIB_ARRAY_ENABLED));
            let detail = if enabled {
                let off = ctx.call("getVertexAttribOffset", &[js::num(loc), n(VERTEX_ATTRIB_ARRAY_POINTER)]).unwrap_or_default();
                format!(
                    "{} × {}{}{}, stride {}, offset {}, divisor {}",
                    js::u(&a(VERTEX_ATTRIB_ARRAY_SIZE)),
                    enum_name(js::u(&a(VERTEX_ATTRIB_ARRAY_TYPE))),
                    if js::truthy(&a(VERTEX_ATTRIB_ARRAY_NORMALIZED)) { " normalized" } else { "" },
                    if js::truthy(&a(VERTEX_ATTRIB_ARRAY_INTEGER)) { " integer" } else { "" },
                    js::u(&a(VERTEX_ATTRIB_ARRAY_STRIDE)),
                    js::f(&off),
                    js::u(&a(VERTEX_ATTRIB_ARRAY_DIVISOR)),
                )
            } else {
                format!("disabled, current value {}", label(&a(0x8626)))
            };
            ins.bindings.push(BindingRow {
                group: "Attributes".into(),
                slot: format!("{loc} {} ({})", var.name, enum_name(var.ty)),
                resource: if enabled { st.objs.id_of(&buf) } else { None },
                detail,
            });
        }
    }
    let ib = ctx.param(ELEMENT_ARRAY_BUFFER_BINDING);
    ins.bindings.push(BindingRow {
        group: "Index buffer".into(),
        slot: "ELEMENT_ARRAY_BUFFER".into(),
        resource: st.objs.id_of(&ib),
        detail: label(&ib),
    });
    if !js::is_obj(&prog) {
        return;
    }
    // Uniforms + textures.
    let prev_active = ctx.param(ACTIVE_TEXTURE);
    for (name, ty) in plain_uniforms(ctx, &prog) {
        let loc = ctx.call("getUniformLocation", &[prog.clone(), name.as_str().into()]).unwrap_or(JsValue::NULL);
        if loc.is_null() {
            continue;
        }
        let val = ctx.call("getUniform", &[prog.clone(), loc]).unwrap_or_default();
        ins.bindings.push(BindingRow {
            group: "Uniforms".into(),
            slot: format!("{name} ({})", enum_name(ty)),
            resource: None,
            detail: label(&val),
        });
        if is_sampler(ty) {
            let unit = js::u(&val);
            ctx.c("activeTexture", &[(TEXTURE0 + unit) as f64]);
            let tex = ctx.param(sampler_binding(ty));
            let smp = ctx.param(SAMPLER_BINDING);
            ins.bindings.push(BindingRow {
                group: "Textures".into(),
                slot: format!("unit {unit} {name}"),
                resource: st.objs.id_of(&tex),
                detail: format!("{} {}", enum_name(sampler_binding(ty)), label(&tex)),
            });
            if js::is_obj(&smp) {
                ins.bindings.push(BindingRow {
                    group: "Textures".into(),
                    slot: format!("unit {unit} sampler"),
                    resource: st.objs.id_of(&smp),
                    detail: label(&smp),
                });
            }
        }
    }
    let _ = ctx.call("activeTexture", &[prev_active]);
    // Uniform blocks.
    let blocks = js::u(&ctx.call("getProgramParameter", &[prog.clone(), n(ACTIVE_UNIFORM_BLOCKS)]).unwrap_or_default());
    for i in 0..blocks {
        let name = ctx.call("getActiveUniformBlockName", &[prog.clone(), n(i)]).ok().and_then(|v| v.as_string()).unwrap_or_default();
        let bp = |pn: u32| js::u(&ctx.call("getActiveUniformBlockParameter", &[prog.clone(), n(i), n(pn)]).unwrap_or_default());
        let binding = bp(UNIFORM_BLOCK_BINDING);
        let data_size = bp(UNIFORM_BLOCK_DATA_SIZE);
        let ip = |pn: u32| ctx.call("getIndexedParameter", &[n(pn), n(binding)]).unwrap_or_default();
        let buf = ip(UNIFORM_BUFFER_BINDING);
        ins.bindings.push(BindingRow {
            group: "Uniform blocks".into(),
            slot: format!("{name} → binding {binding}"),
            resource: st.objs.id_of(&buf),
            detail: format!(
                "{} offset {} size {} (block size {data_size})",
                label(&buf),
                js::f(&ip(UNIFORM_BUFFER_START)),
                js::f(&ip(UNIFORM_BUFFER_SIZE))
            ),
        });
    }
}

/// `getBufferSubData` of a WebGL buffer.
pub fn read_buffer(ctx: &GlCtx, cap: &Capture, id: ObjId, buf: &JsValue) -> Result<Vec<u8>, String> {
    let is_index = cap
        .snapshot
        .iter()
        .find(|c| cap.method(c) == "webgd:glBuffer" && c.args.first().and_then(|a| a.as_obj()) == Some(id))
        .and_then(|c| c.args.get(1))
        .and_then(|d| cap.dict_get(d, "isIndex"))
        .and_then(|v| v.as_f64())
        .map(|v| v != 0.0)
        .unwrap_or_else(|| {
            cap.setup.iter().chain(cap.frame.iter()).find_map(|c| {
                (cap.method(c) == "bindBuffer" && c.args.get(1).and_then(|a| a.as_obj()) == Some(id))
                    .then(|| c.args.first().and_then(|a| a.as_f64()) == Some(ELEMENT_ARRAY_BUFFER as f64))
            }) == Some(true)
        });
    let (target, saved) = if is_index {
        let s = ctx.save(&[VERTEX_ARRAY_BINDING], &[]);
        let _ = ctx.call("bindVertexArray", std::slice::from_ref(&ctx.scratch_vao));
        (ELEMENT_ARRAY_BUFFER, s)
    } else {
        (COPY_READ_BUFFER, ctx.save(&[COPY_READ_BUFFER_BINDING], &[]))
    };
    let _ = ctx.call("bindBuffer", &[n(target), buf.clone()]);
    let size = js::u(&ctx.call("getBufferParameter", &[n(target), n(BUFFER_SIZE)]).unwrap_or_default());
    let out = js::construct("Uint8Array", &[n(size)])
        .and_then(|dst| ctx.call("getBufferSubData", &[n(target), n(0), dst.clone()]).map(|_| dst))
        .map(|dst| js::bytes_of(&dst))
        .map_err(|e| js::err_msg(&e));
    if is_index {
        let _ = ctx.call("bindBuffer", &[n(ELEMENT_ARRAY_BUFFER), JsValue::NULL]);
    }
    ctx.restore(saved);
    out
}

/// Reads a texture level/face/layer or a renderbuffer as of the replayed point.
pub fn read_image(ctx: &GlCtx, cap: &Capture, stop: usize, id: ObjId, obj: &JsValue, mip: u32, layer: u32) -> Result<Image, String> {
    if cap.class_of(id) == "WebGLRenderbuffer" {
        let (w, h, ifmt, samples) = rb_info(ctx, obj);
        if samples > 0 {
            return Err("multisampled renderbuffer not readable".into());
        }
        let class = read_class(ifmt).ok_or("depth/stencil not readable in WebGL2")?;
        return ctx.read_attachment(&Attach::Rb(obj.clone()), w, h, class);
    }
    let d = texture_dims(cap, stop, id).ok_or("texture dimensions unknown")?;
    let class = read_class(d.internalformat).ok_or("depth/stencil not readable in WebGL2")?;
    let (face, layer) = if d.target == TEXTURE_CUBE_MAP { (layer.min(5), 0) } else { (0, layer) };
    let &(w, h, _) = d.levels.get(&(mip, face)).ok_or_else(|| format!("mip {mip} not defined"))?;
    let att = Attach::Tex { tex: obj.clone(), target: d.target, level: mip, face, layer };
    match ctx.read_attachment(&att, w, h, class) {
        Ok(img) => Ok(img),
        Err(_) => sample_read(ctx, obj, d.target, mip, face, layer, w, h, class),
    }
}

const SAMPLE_VS: &str = "#version 300 es\nvoid main(){vec2 p=vec2(float((gl_VertexID<<1)&2),float(gl_VertexID&2));gl_Position=vec4(p*2.0-1.0,0.0,1.0);}";

fn sample_fs(prefix: &str, target: u32) -> String {
    let (ty, body) = match target {
        TEXTURE_3D => ("3D", "o=texelFetch(t,ivec3(c,u_layer),0);"),
        TEXTURE_2D_ARRAY => ("2DArray", "o=texelFetch(t,ivec3(c,u_layer),0);"),
        TEXTURE_CUBE_MAP => (
            "Cube",
            "vec2 uv=(vec2(c)+0.5)/u_size*2.0-1.0;vec3 d;\
             if(u_face==0)d=vec3(1.0,-uv.y,-uv.x);else if(u_face==1)d=vec3(-1.0,-uv.y,uv.x);\
             else if(u_face==2)d=vec3(uv.x,1.0,uv.y);else if(u_face==3)d=vec3(uv.x,-1.0,-uv.y);\
             else if(u_face==4)d=vec3(uv.x,-uv.y,1.0);else d=vec3(-uv.x,-uv.y,-1.0);\
             o=textureLod(t,d,0.0);",
        ),
        _ => ("2D", "o=texelFetch(t,c,0);"),
    };
    format!(
        "#version 300 es\nprecision highp float;precision highp int;\nuniform highp {prefix}sampler{ty} t;uniform int u_layer;uniform int u_face;uniform vec2 u_size;\nout highp {prefix}vec4 o;\nvoid main(){{ivec2 c=ivec2(gl_FragCoord.xy);{body}}}"
    )
}

/// Readback of non-renderable textures (LUMINANCE, compressed, RGB32F, …) by drawing `texelFetch`
/// into a float/int renderbuffer.
#[allow(clippy::too_many_arguments)]
fn sample_read(ctx: &GlCtx, tex: &JsValue, target: u32, level: u32, face: u32, layer: u32, w: u32, h: u32, class: ReadClass) -> Result<Image, String> {
    let (prefix, kind, rb_fmt, out_class) = match class {
        ReadClass::Int => ("i", 1, RGBA32I, ReadClass::Int),
        ReadClass::Uint => ("u", 2, RGBA32UI, ReadClass::Uint),
        _ if ctx.has_cbf => ("", 0, RGBA32F, ReadClass::Float),
        _ => ("", 0, RGBA8, ReadClass::Unorm),
    };
    let prog = {
        let mut progs = ctx.progs.borrow_mut();
        match progs.get(&(kind, target)) {
            Some(p) => p.clone(),
            None => {
                let vs = ctx.compile(VERTEX_SHADER, SAMPLE_VS)?;
                let p = ctx.build_program(&vs, &sample_fs(prefix, target), &[])?;
                progs.insert((kind, target), p.clone());
                p
            }
        }
    };
    let binding = match target {
        TEXTURE_3D => TEXTURE_BINDING_3D,
        TEXTURE_2D_ARRAY => TEXTURE_BINDING_2D_ARRAY,
        TEXTURE_CUBE_MAP => TEXTURE_BINDING_CUBE_MAP,
        _ => TEXTURE_BINDING_2D,
    };
    let saved = ctx.save(
        &[CURRENT_PROGRAM, VERTEX_ARRAY_BINDING, DRAW_FRAMEBUFFER_BINDING, READ_FRAMEBUFFER_BINDING, RENDERBUFFER_BINDING, VIEWPORT, COLOR_WRITEMASK, ACTIVE_TEXTURE],
        &[BLEND, CULL_FACE, DEPTH_TEST, STENCIL_TEST, SCISSOR_TEST, RASTERIZER_DISCARD, SAMPLE_ALPHA_TO_COVERAGE, SAMPLE_COVERAGE, POLYGON_OFFSET_FILL, DITHER],
    );
    let unit = ctx.scratch_unit;
    ctx.c("activeTexture", &[(TEXTURE0 + unit) as f64]);
    let prev_tex = ctx.param(binding);
    let prev_sampler = ctx.param(SAMPLER_BINDING);
    ctx.bind_tex(target, tex);
    let _ = ctx.call("bindSampler", &[n(unit), JsValue::NULL]);
    let tp = [TEXTURE_BASE_LEVEL, TEXTURE_MAX_LEVEL, TEXTURE_MIN_FILTER, TEXTURE_MAG_FILTER, TEXTURE_COMPARE_MODE];
    let prev_params: Vec<JsValue> =
        tp.iter().map(|p| ctx.call("getTexParameter", &[n(target), n(*p)]).unwrap_or_default()).collect();
    for (p, v) in tp.iter().zip([level, level, NEAREST, NEAREST, NONE]) {
        ctx.c("texParameteri", &[target as f64, *p as f64, v as f64]);
    }
    let rb = ctx.call("createRenderbuffer", &[]).map_err(|e| js::err_msg(&e))?;
    let _ = ctx.call("bindRenderbuffer", &[n(RENDERBUFFER), rb.clone()]);
    ctx.c("renderbufferStorage", &[RENDERBUFFER as f64, rb_fmt as f64, w as f64, h as f64]);
    let fbo = ctx.call("createFramebuffer", &[]).map_err(|e| js::err_msg(&e))?;
    ctx.bind_fb(FRAMEBUFFER, &fbo);
    let _ = ctx.call("framebufferRenderbuffer", &[n(FRAMEBUFFER), n(COLOR_ATTACHMENT0), n(RENDERBUFFER), rb.clone()]);
    let _ = ctx.call("drawBuffers", &[js::arr(&[n(COLOR_ATTACHMENT0)])]);
    for c in [BLEND, CULL_FACE, DEPTH_TEST, STENCIL_TEST, SCISSOR_TEST, RASTERIZER_DISCARD, SAMPLE_ALPHA_TO_COVERAGE, SAMPLE_COVERAGE] {
        ctx.c("disable", &[c as f64]);
    }
    let _ = ctx.call("colorMask", &[JsValue::TRUE, JsValue::TRUE, JsValue::TRUE, JsValue::TRUE]);
    ctx.c("viewport", &[0.0, 0.0, w as f64, h as f64]);
    let _ = ctx.call("useProgram", std::slice::from_ref(&prog));
    let loc = |name: &str| ctx.call("getUniformLocation", &[prog.clone(), name.into()]).unwrap_or(JsValue::NULL);
    let _ = ctx.call("uniform1i", &[loc("t"), n(unit)]);
    let _ = ctx.call("uniform1i", &[loc("u_layer"), n(layer)]);
    let _ = ctx.call("uniform1i", &[loc("u_face"), n(face)]);
    let _ = ctx.call("uniform2f", &[loc("u_size"), n(w), n(h)]);
    let _ = ctx.call("bindVertexArray", std::slice::from_ref(&ctx.scratch_vao));
    let status = js::u(&ctx.call("checkFramebufferStatus", &[n(FRAMEBUFFER)]).unwrap_or_default());
    let res = if status == FRAMEBUFFER_COMPLETE {
        ctx.c("drawArrays", &[TRIANGLES as f64, 0.0, 3.0]);
        ctx.read_pixels(w, h, out_class)
    } else {
        Err(format!("sampling target incomplete (0x{status:X})"))
    };
    for (p, v) in tp.iter().zip(prev_params) {
        let _ = ctx.call("texParameteri", &[n(target), n(*p), v]);
    }
    ctx.bind_tex(target, &prev_tex);
    let _ = ctx.call("bindSampler", &[n(unit), prev_sampler]);
    ctx.restore(saved);
    let _ = ctx.call("deleteFramebuffer", &[fbo]);
    let _ = ctx.call("deleteRenderbuffer", &[rb]);
    res
}

pub fn program_info(ctx: &GlCtx, prog: &JsValue) -> ProgramInfo {
    let link_status = js::truthy(&ctx.call("getProgramParameter", &[prog.clone(), n(LINK_STATUS)]).unwrap_or_default());
    let info_log = ctx.call("getProgramInfoLog", std::slice::from_ref(prog)).ok().and_then(|v| v.as_string()).unwrap_or_default();
    let count = |pn: u32| js::u(&ctx.call("getProgramParameter", &[prog.clone(), n(pn)]).unwrap_or_default());
    let attributes = (0..count(ACTIVE_ATTRIBUTES))
        .filter_map(|i| active(ctx, prog, "getActiveAttrib", i))
        .map(|v| {
            let loc = js::f(&ctx.call("getAttribLocation", &[prog.clone(), v.name.as_str().into()]).unwrap_or_default());
            format!("{} : {}[{}] @ {loc}", v.name, enum_name(v.ty), v.size)
        })
        .collect();
    let uniforms = (0..count(ACTIVE_UNIFORMS))
        .filter_map(|i| active(ctx, prog, "getActiveUniform", i))
        .map(|v| format!("{} : {}[{}]", v.name, enum_name(v.ty), v.size))
        .collect();
    ProgramInfo { link_status, info_log, attributes, uniforms }
}
