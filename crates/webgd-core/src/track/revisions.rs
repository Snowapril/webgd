//! Per-resource revision history: every call that creates, writes, reads or destroys a resource.
//!
//! The walk runs over the setup section, then loads frame-start bindings from the snapshot
//! section (which never produces events itself), then runs over the frame section. GL calls are
//! attributed through tracked per-context bindings (texture units, buffer targets, VAOs,
//! framebuffers); WebGPU calls through tracked pass/bundle encoder state and bind groups.
use crate::capture::{Call, Capture, ObjId, Section, Value};
use crate::track::gl::{
    TEXTURE_2D, TEXTURE_2D_ARRAY, TEXTURE_3D, TEXTURE_CUBE_MAP, TEXTURE_CUBE_MAP_POSITIVE_X,
};
use crate::track::resources::{RESOURCE_CLASSES, gpu_buffer_usage_names};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Access {
    Create,
    /// Contents modified (upload, copy destination, clear, render/storage write).
    Write,
    /// Bound or consumed without modification (sampled, vertex/index/uniform read, copy source).
    Read,
    Destroy,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Revision {
    /// `Setup` (pre-frame, not selectable) or `Frame`. Never `Snapshot`.
    pub section: Section,
    /// Index into `cap.setup` or `cap.frame`.
    pub index: usize,
    pub access: Access,
    /// Content revision current after this event: 0 at Create, +1 per Write; Read/Destroy carry the current value.
    pub revision: u32,
    /// Human-readable detail, e.g. "bufferSubData 64 B @ 16", "color attachment 0 (clear)", "vertex buffer slot 0".
    pub detail: String,
}

/// History for every resource that appears in the capture, keyed by resource id.
/// GPUTextureView events are attributed to their parent GPUTexture (and also listed under the view).
/// Each list is in capture order: all Setup events, then Frame events.
///
/// Only objects whose class is in [`RESOURCE_CLASSES`] get histories. Each id has its own
/// revision counter; a resource first seen without a Create starts at revision 0.
pub fn all(cap: &Capture) -> HashMap<ObjId, Vec<Revision>> {
    let mut w = Walker {
        log: Log::new(cap),
        gl: Gl::default(),
        gpu: Gpu::default(),
        tokens: HashMap::new(),
    };
    for (i, c) in cap.setup.iter().enumerate() {
        w.call(Section::Setup, i, c);
    }
    for c in &cap.snapshot {
        w.gl.snapshot(cap, c);
    }
    for (i, c) in cap.frame.iter().enumerate() {
        w.call(Section::Frame, i, c);
    }
    w.log.out
}

/// Content revision current at `frame[index]` (inclusive), or `None` if the resource does not exist yet.
///
/// The last event in Setup, or in Frame at or before `index`, gives the revision. With no such
/// event, a history that starts with a (frame) Create means the resource is created later →
/// `None`; a history without a Create belongs to a pre-existing resource → `Some(0)`. An empty
/// history → `None`.
pub fn revision_at(history: &[Revision], index: usize) -> Option<u32> {
    let current = history
        .iter()
        .rev()
        .find(|r| r.section == Section::Setup || (r.section == Section::Frame && r.index <= index));
    match current {
        Some(r) => Some(r.revision),
        None => match history.first() {
            Some(first) if first.access != Access::Create => Some(0),
            _ => None,
        },
    }
}

/// Methods whose ObjRef result is a newly created resource.
fn is_creator(m: &str) -> bool {
    m.starts_with("create") || matches!(m, "getCurrentTexture" | "getBindGroupLayout" | "finish")
}

fn str_of<'a>(cap: &'a Capture, v: Option<&Value>) -> Option<&'a str> {
    match v {
        Some(Value::String(s)) => Some(cap.str(*s)),
        _ => None,
    }
}

fn is_true(v: Option<&Value>) -> bool {
    v.and_then(Value::as_f64).is_some_and(|x| x != 0.0)
}

/// Byte length of upload data: a size Number, a typed array (with element `off`/`len`
/// sub-range), or an Image (RGBA8).
fn data_bytes(cap: &Capture, v: Option<&Value>, off: Option<f64>, len: Option<f64>) -> Option<f64> {
    match v? {
        Value::Number(n) => Some(*n),
        Value::Typed { kind, blob } => {
            let es = kind.element_size() as f64;
            match len.filter(|&l| l > 0.0) {
                Some(l) => Some(l * es),
                None => Some((cap.blob(*blob).len() as f64 - off.unwrap_or(0.0) * es).max(0.0)),
            }
        }
        Value::Image { width, height, .. } => Some(f64::from(*width) * f64::from(*height) * 4.0),
        _ => None,
    }
}

fn bytes_suffix(b: Option<f64>) -> String {
    b.map(|b| format!(" {b} B")).unwrap_or_default()
}

/// `w×h` or `w×h×d` of a GPUExtent3D (Array or Dict).
fn extent(cap: &Capture, v: Option<&Value>) -> Option<String> {
    let v = v?;
    let dim = |i: usize, k: &str| match v {
        Value::Array(a) => a.get(i).and_then(Value::as_f64),
        _ => cap.dict_get(v, k).and_then(Value::as_f64),
    };
    let w = dim(0, "width")?;
    let h = dim(1, "height").unwrap_or(1.0);
    Some(match dim(2, "depthOrArrayLayers").filter(|&d| d > 1.0) {
        Some(d) => format!("{w}×{h}×{d}"),
        None => format!("{w}×{h}"),
    })
}

/// Detail for the Create event of `id` made by `c`.
fn create_detail(cap: &Capture, c: &Call, m: &str, id: ObjId) -> String {
    let desc = c.args.first();
    let dn = |k: &str| {
        desc.and_then(|d| cap.dict_get(d, k))
            .and_then(Value::as_f64)
    };
    let ds = |k: &str| str_of(cap, desc.and_then(|d| cap.dict_get(d, k)));
    match cap.class_of(id) {
        "GPUBuffer" => match dn("size") {
            Some(size) => {
                let usage = gpu_buffer_usage_names(dn("usage").unwrap_or(0.0) as u32);
                let mapped = if is_true(desc.and_then(|d| cap.dict_get(d, "mappedAtCreation"))) {
                    " mappedAtCreation"
                } else {
                    ""
                };
                if usage.is_empty() {
                    format!("{m} {size} B{mapped}")
                } else {
                    format!("{m} {size} B {usage}{mapped}")
                }
            }
            None => m.to_owned(),
        },
        "GPUTexture" if m == "createTexture" => {
            let mut s = format!("{m} {}", ds("format").unwrap_or("?"));
            if let Some(e) = extent(cap, desc.and_then(|d| cap.dict_get(d, "size"))) {
                s.push(' ');
                s.push_str(&e);
            }
            if let Some(mips) = dn("mipLevelCount").filter(|&n| n > 1.0) {
                s.push_str(&format!(" {mips} mips"));
            }
            s
        }
        "GPUTextureView" => {
            let mut s = m.to_owned();
            for key in ["format", "dimension"] {
                if let Some(v) = ds(key) {
                    s.push(' ');
                    s.push_str(v);
                }
            }
            s
        }
        "GPUQuerySet" => format!(
            "{m} {} ×{}",
            ds("type").unwrap_or("?"),
            dn("count").unwrap_or(0.0)
        ),
        "WebGLShader" => match desc.and_then(Value::as_f64) {
            Some(35633.0) => format!("{m} vertex"),
            Some(35632.0) => format!("{m} fragment"),
            _ => m.to_owned(),
        },
        _ => m.to_owned(),
    }
}

/// Event sink with per-id revision counters.
struct Log<'a> {
    cap: &'a Capture,
    out: HashMap<ObjId, Vec<Revision>>,
    rev: HashMap<ObjId, u32>,
    /// GPUTextureView → GPUTexture, from `createView` calls.
    view_parent: HashMap<ObjId, ObjId>,
    section: Section,
    index: usize,
    /// Per-call dedupe: one event per (resource, access) per call.
    seen: HashSet<(ObjId, Access)>,
}

impl<'a> Log<'a> {
    fn new(cap: &'a Capture) -> Self {
        Log {
            cap,
            out: HashMap::new(),
            rev: HashMap::new(),
            view_parent: HashMap::new(),
            section: Section::Setup,
            index: 0,
            seen: HashSet::new(),
        }
    }

    fn begin(&mut self, section: Section, index: usize) {
        self.section = section;
        self.index = index;
        self.seen.clear();
    }

    fn tracked(&self, id: ObjId) -> bool {
        id.0 != 0 && RESOURCE_CLASSES.contains(&self.cap.class_of(id))
    }

    fn parent_texture(&self, id: ObjId) -> Option<ObjId> {
        if self.cap.class_of(id) != "GPUTextureView" {
            return None;
        }
        self.view_parent
            .get(&id)
            .copied()
            .or_else(|| self.cap.objects.get(&id).and_then(|o| o.parent))
            .filter(|&p| self.cap.class_of(p) == "GPUTexture")
    }

    /// Create event for `id` only (never attributed to a view's texture). A resource that
    /// already has events (e.g. a repeated `getCurrentTexture`) gets a Read instead.
    fn create(&mut self, id: ObjId, detail: String) {
        let access = if self.out.contains_key(&id) {
            Access::Read
        } else {
            Access::Create
        };
        self.push(id, access, detail);
    }

    /// Event on `id`, also attributed to the parent texture when `id` is a GPUTextureView.
    fn emit(&mut self, id: ObjId, access: Access, detail: impl Into<String>) {
        let detail = detail.into();
        if let Some(tex) = self.parent_texture(id) {
            self.push(tex, access, detail.clone());
        }
        self.push(id, access, detail);
    }

    fn push(&mut self, id: ObjId, access: Access, detail: String) {
        if !self.tracked(id) || !self.seen.insert((id, access)) {
            return;
        }
        let r = self.rev.entry(id).or_insert(0);
        match access {
            Access::Create => *r = 0,
            Access::Write => *r += 1,
            Access::Read | Access::Destroy => {}
        }
        let revision = *r;
        self.out.entry(id).or_default().push(Revision {
            section: self.section,
            index: self.index,
            access,
            revision,
            detail,
        });
    }
}

struct Walker<'a> {
    log: Log<'a>,
    gl: Gl,
    gpu: Gpu,
    /// Pending token → issuing method, for `webgd:resolved` creations (`create*Async`).
    tokens: HashMap<u32, &'a str>,
}

impl Walker<'_> {
    fn call(&mut self, section: Section, index: usize, c: &Call) {
        let cap = self.log.cap;
        let m = cap.method(c);
        self.log.begin(section, index);
        match &c.result {
            Value::Pending(t) => {
                self.tokens.insert(*t, m);
            }
            Value::ObjRef(id) if is_creator(m) => {
                if m == "createView" {
                    self.log.view_parent.insert(*id, c.this);
                }
                self.log.create(*id, create_detail(cap, c, m, *id));
            }
            _ => {}
        }
        if m == "webgd:resolved" {
            let token = c.args.first().and_then(Value::as_f64);
            if let (Some(t), Some(id)) = (token, c.args.get(1).and_then(Value::as_obj)) {
                let from = self.tokens.get(&(t as u32)).copied().unwrap_or("");
                if is_creator(from) {
                    self.log.create(id, from.to_owned());
                }
            }
        }
        if cap.class_of(c.this) == "WebGL2RenderingContext" {
            self.gl.call(&mut self.log, c, m);
        } else {
            self.gpu.call(&mut self.log, c, m);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// WebGL2

const TEXTURE0: u32 = 0x84C0;
/// Pseudo texture unit used by setup-log `webgd:bindForUpload` (never read by draws).
const UPLOAD_UNIT: u32 = u32::MAX;
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const PIXEL_PACK_BUFFER: u32 = 35051;
const PIXEL_UNPACK_BUFFER: u32 = 35052;
const UNIFORM_BUFFER: u32 = 35345;
const READ_FRAMEBUFFER: u32 = 36008;
const DRAW_FRAMEBUFFER: u32 = 36009;
const COLOR_ATTACHMENT0: u32 = 36064;
const DEPTH_ATTACHMENT: u32 = 36096;
const STENCIL_ATTACHMENT: u32 = 36128;
const DEPTH_STENCIL_ATTACHMENT: u32 = 33306;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const DEPTH_BUFFER_BIT: u32 = 0x100;
const STENCIL_BUFFER_BIT: u32 = 0x400;
/// `clearBuffer*` buffer enums.
const CB_COLOR: u32 = 6144;
const CB_DEPTH: u32 = 6145;
const CB_STENCIL: u32 = 6146;

/// (bind target, glState binding key) for non-VAO buffer targets.
const BUFFER_BINDINGS: &[(u32, &str)] = &[
    (ARRAY_BUFFER, "34964"),
    (36662, "36662"), // COPY_READ_BUFFER
    (36663, "36663"), // COPY_WRITE_BUFFER
    (PIXEL_PACK_BUFFER, "35053"),
    (PIXEL_UNPACK_BUFFER, "35055"),
    (UNIFORM_BUFFER, "35368"),
    (35982, "35983"), // TRANSFORM_FEEDBACK_BUFFER
];

/// glState `units` keys → texture bind target.
const UNIT_KEYS: [(&str, u32); 4] = [
    ("2d", TEXTURE_2D),
    ("cube", TEXTURE_CUBE_MAP),
    ("3d", TEXTURE_3D),
    ("array", TEXTURE_2D_ARRAY),
];

const TEX_WRITES: &[&str] = &[
    "texImage2D",
    "texImage3D",
    "texSubImage2D",
    "texSubImage3D",
    "texStorage2D",
    "texStorage3D",
    "copyTexImage2D",
    "copyTexSubImage2D",
    "copyTexSubImage3D",
    "compressedTexImage2D",
    "compressedTexImage3D",
    "compressedTexSubImage2D",
    "compressedTexSubImage3D",
    "generateMipmap",
];

const GL_DRAWS: &[&str] = &[
    "drawArrays",
    "drawElements",
    "drawArraysInstanced",
    "drawElementsInstanced",
    "drawRangeElements",
];

/// Cube face upload targets bind as TEXTURE_CUBE_MAP.
fn texture_bind_target(target: u32) -> u32 {
    if (TEXTURE_CUBE_MAP_POSITIVE_X..TEXTURE_CUBE_MAP_POSITIVE_X + 6).contains(&target) {
        TEXTURE_CUBE_MAP
    } else {
        target
    }
}

fn is_color(att: u32) -> bool {
    (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + 16).contains(&att)
}

fn attachment_name(att: u32) -> String {
    match att {
        DEPTH_ATTACHMENT => "depth attachment".into(),
        STENCIL_ATTACHMENT => "stencil attachment".into(),
        DEPTH_STENCIL_ATTACHMENT => "depth-stencil attachment".into(),
        a if is_color(a) => format!("color attachment {}", a - COLOR_ATTACHMENT0),
        a => format!("attachment {a}"),
    }
}

/// Whether a `clear` / `blitFramebuffer` buffer mask covers attachment `att`.
fn in_mask(att: u32, mask: u32) -> bool {
    match att {
        DEPTH_ATTACHMENT => mask & DEPTH_BUFFER_BIT != 0,
        STENCIL_ATTACHMENT => mask & STENCIL_BUFFER_BIT != 0,
        DEPTH_STENCIL_ATTACHMENT => mask & (DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0,
        _ => mask & COLOR_BUFFER_BIT != 0,
    }
}

/// `"{method} level L w×h"` style detail for texture uploads.
fn tex_detail(m: &str, a: &[Value]) -> String {
    let n = |i: usize| a.get(i).and_then(Value::as_f64);
    let size = |w: Option<f64>, h: Option<f64>| match (w, h) {
        (Some(w), Some(h)) => format!(" {w}×{h}"),
        _ => String::new(),
    };
    let image = |i: usize| match a.get(i) {
        Some(Value::Image { width, height, .. }) => format!(" {width}×{height}"),
        _ => String::new(),
    };
    let level = n(1).unwrap_or(0.0);
    match m {
        "generateMipmap" => m.to_owned(),
        "texStorage2D" | "texStorage3D" => {
            format!("{m} {} levels{}", n(1).unwrap_or(1.0), size(n(3), n(4)))
        }
        // (target, level, internalformat, format, type, source)
        "texImage2D" if a.len() == 6 => format!("{m} level {level}{}", image(5)),
        // (target, level, xoffset, yoffset, format, type, source)
        "texSubImage2D" if a.len() == 7 => format!("{m} level {level}{}", image(6)),
        "copyTexImage2D" => format!("{m} level {level}{}", size(n(5), n(6))),
        "copyTexSubImage2D" => format!("{m} level {level}{}", size(n(6), n(7))),
        "copyTexSubImage3D" => format!("{m} level {level}{}", size(n(7), n(8))),
        "texSubImage2D" | "compressedTexSubImage2D" => {
            format!("{m} level {level}{}", size(n(4), n(5)))
        }
        "texSubImage3D" | "compressedTexSubImage3D" => {
            format!("{m} level {level}{}", size(n(5), n(6)))
        }
        _ => format!("{m} level {level}{}", size(n(3), n(4))),
    }
}

/// Per-context GL bindings. `ObjId(0)` = nothing bound / default object.
#[derive(Default)]
struct GlCtx {
    /// Active texture unit index (`UPLOAD_UNIT` after `webgd:bindForUpload`).
    active: u32,
    /// (unit, bind target) → texture
    textures: BTreeMap<(u32, u32), ObjId>,
    /// unit → sampler
    samplers: BTreeMap<u32, ObjId>,
    /// non-element buffer target → buffer
    buffers: BTreeMap<u32, ObjId>,
    /// indexed UNIFORM_BUFFER binding → buffer
    ubos: BTreeMap<u32, ObjId>,
    vao: ObjId,
    draw_fb: ObjId,
    read_fb: ObjId,
    renderbuffer: ObjId,
    program: ObjId,
}

#[derive(Default)]
struct Vao {
    /// attrib index → (buffer, enabled)
    attribs: BTreeMap<u32, (Option<ObjId>, bool)>,
    elements: Option<ObjId>,
}

#[derive(Default)]
struct Gl {
    ctxs: HashMap<ObjId, GlCtx>,
    /// (context, vao; ObjId(0) = default VAO) → state
    vaos: HashMap<(ObjId, ObjId), Vao>,
    /// framebuffer → attachment enum → texture / renderbuffer
    fbs: HashMap<ObjId, BTreeMap<u32, ObjId>>,
}

fn assign<K: Ord>(map: &mut BTreeMap<K, ObjId>, key: K, v: Option<ObjId>) {
    match v {
        Some(v) => {
            map.insert(key, v);
        }
        None => {
            map.remove(&key);
        }
    }
}

/// One event per attachment of framebuffer `fb` that `keep` selects; the default framebuffer
/// (`ObjId(0)`) has no resource and is skipped.
fn fb_events(
    log: &mut Log,
    fbs: &HashMap<ObjId, BTreeMap<u32, ObjId>>,
    fb: ObjId,
    access: Access,
    keep: impl Fn(u32) -> bool,
    what: &str,
) {
    if fb.0 == 0 {
        return;
    }
    let Some(atts) = fbs.get(&fb) else { return };
    for (&att, &obj) in atts {
        if keep(att) {
            log.emit(obj, access, format!("{} ({what})", attachment_name(att)));
        }
    }
}

impl Gl {
    /// Frame-start bindings and framebuffer/VAO contents from snapshot records.
    fn snapshot(&mut self, cap: &Capture, c: &Call) {
        let a = &c.args;
        match cap.method(c) {
            "webgd:glFramebuffer" => {
                let Some(fb) = a.first().and_then(Value::as_obj) else {
                    return;
                };
                let atts = self.fbs.entry(fb).or_default();
                atts.clear();
                if let Some(Value::Dict(entries)) =
                    a.get(1).and_then(|d| cap.dict_get(d, "attachments"))
                {
                    for (k, v) in entries {
                        let obj = cap.dict_get(v, "object").and_then(Value::as_obj);
                        if let (Ok(att), Some(obj)) = (cap.str(*k).parse::<u32>(), obj) {
                            atts.insert(att, obj);
                        }
                    }
                }
            }
            "webgd:glVertexArray" => {
                let vao = a.first().and_then(Value::as_obj).unwrap_or(ObjId(0));
                let Some(d) = a.get(1) else { return };
                let mut v = Vao::default();
                for (i, at) in cap
                    .dict_get(d, "attribs")
                    .and_then(Value::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    let enabled = is_true(cap.dict_get(at, "enabled"));
                    let buf = cap.dict_get(at, "buffer").and_then(Value::as_obj);
                    if enabled || buf.is_some() {
                        v.attribs.insert(i as u32, (buf, enabled));
                    }
                }
                v.elements = cap.dict_get(d, "elementBuffer").and_then(Value::as_obj);
                self.vaos.insert((c.this, vao), v);
            }
            "webgd:glState" => {
                let Some(s) = a.first() else { return };
                let obj = |k: &str| {
                    cap.dict_get(s, k)
                        .and_then(Value::as_obj)
                        .unwrap_or(ObjId(0))
                };
                let mut st = GlCtx {
                    active: cap
                        .dict_get(s, "34016")
                        .and_then(Value::as_f64)
                        .map_or(0, |e| (e as u32).wrapping_sub(TEXTURE0)),
                    vao: obj("34229"),
                    draw_fb: obj("36006"),
                    read_fb: obj("36010"),
                    renderbuffer: obj("36007"),
                    program: obj("35725"),
                    ..GlCtx::default()
                };
                for &(target, key) in BUFFER_BINDINGS {
                    if let Some(b) = cap.dict_get(s, key).and_then(Value::as_obj) {
                        st.buffers.insert(target, b);
                    }
                }
                for (u, unit) in cap
                    .dict_get(s, "units")
                    .and_then(Value::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    for (key, target) in UNIT_KEYS {
                        if let Some(t) = cap.dict_get(unit, key).and_then(Value::as_obj) {
                            st.textures.insert((u as u32, target), t);
                        }
                    }
                    if let Some(smp) = cap.dict_get(unit, "sampler").and_then(Value::as_obj) {
                        st.samplers.insert(u as u32, smp);
                    }
                }
                for (i, e) in cap
                    .dict_get(s, "ubo")
                    .and_then(Value::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    if let Some(b) = cap.dict_get(e, "buffer").and_then(Value::as_obj) {
                        st.ubos.insert(i as u32, b);
                    }
                }
                self.ctxs.insert(c.this, st);
            }
            _ => {}
        }
    }

    fn call(&mut self, log: &mut Log, c: &Call, m: &str) {
        let cap = log.cap;
        let a = &c.args;
        let n = |i: usize| a.get(i).and_then(Value::as_f64);
        let u = |i: usize| n(i).map(|v| v as u32);
        let obj = |i: usize| a.get(i).and_then(Value::as_obj);
        let ctx = c.this;
        let Gl { ctxs, vaos, fbs } = self;
        let st = ctxs.entry(ctx).or_default();
        match m {
            "activeTexture" => {
                if let Some(e) = u(0) {
                    st.active = e.wrapping_sub(TEXTURE0);
                }
            }
            "bindTexture" | "webgd:bindForUpload" => {
                let Some(t) = u(0) else { return };
                if m != "bindTexture" {
                    st.active = UPLOAD_UNIT;
                }
                let key = (st.active, texture_bind_target(t));
                assign(&mut st.textures, key, obj(1));
            }
            "bindSampler" => {
                if let Some(unit) = u(0) {
                    assign(&mut st.samplers, unit, obj(1));
                }
            }
            "bindBuffer" => {
                let Some(t) = u(0) else { return };
                if t == ELEMENT_ARRAY_BUFFER {
                    vaos.entry((ctx, st.vao)).or_default().elements = obj(1);
                } else {
                    assign(&mut st.buffers, t, obj(1));
                }
            }
            "bindBufferBase" | "bindBufferRange" => {
                let (Some(t), Some(i)) = (u(0), u(1)) else {
                    return;
                };
                assign(&mut st.buffers, t, obj(2));
                if t == UNIFORM_BUFFER {
                    assign(&mut st.ubos, i, obj(2));
                }
            }
            "bindVertexArray" => st.vao = obj(0).unwrap_or(ObjId(0)),
            "bindFramebuffer" => {
                let fb = obj(1).unwrap_or(ObjId(0));
                match u(0) {
                    Some(DRAW_FRAMEBUFFER) => st.draw_fb = fb,
                    Some(READ_FRAMEBUFFER) => st.read_fb = fb,
                    _ => {
                        st.draw_fb = fb;
                        st.read_fb = fb;
                    }
                }
            }
            "bindRenderbuffer" => st.renderbuffer = obj(1).unwrap_or(ObjId(0)),
            "useProgram" => {
                st.program = obj(0).unwrap_or(ObjId(0));
                log.emit(st.program, Access::Read, m);
            }
            "vertexAttribPointer"
            | "vertexAttribIPointer"
            | "enableVertexAttribArray"
            | "disableVertexAttribArray" => {
                let Some(i) = u(0) else { return };
                let array_buffer = st.buffers.get(&ARRAY_BUFFER).copied();
                let attrib = vaos
                    .entry((ctx, st.vao))
                    .or_default()
                    .attribs
                    .entry(i)
                    .or_insert((None, false));
                match m {
                    "enableVertexAttribArray" => attrib.1 = true,
                    "disableVertexAttribArray" => attrib.1 = false,
                    _ => attrib.0 = array_buffer,
                }
                log.emit(st.vao, Access::Write, format!("{m} {i}"));
            }
            "framebufferTexture2D" | "framebufferTextureLayer" | "framebufferRenderbuffer" => {
                let (Some(target), Some(att)) = (u(0), u(1)) else {
                    return;
                };
                let attached = if m == "framebufferTextureLayer" {
                    obj(2)
                } else {
                    obj(3)
                };
                let fb = if target == READ_FRAMEBUFFER {
                    st.read_fb
                } else {
                    st.draw_fb
                };
                if fb.0 == 0 {
                    return;
                }
                assign(fbs.entry(fb).or_default(), att, attached);
                log.emit(fb, Access::Write, format!("{m} {}", attachment_name(att)));
            }
            "bufferData" | "bufferSubData" | "getBufferSubData" => {
                let Some(t) = u(0) else { return };
                let buf = if t == ELEMENT_ARRAY_BUFFER {
                    vaos.get(&(ctx, st.vao)).and_then(|v| v.elements)
                } else {
                    st.buffers.get(&t).copied()
                };
                let Some(buf) = buf else { return };
                match m {
                    "bufferData" => log.emit(
                        buf,
                        Access::Write,
                        format!("{m}{}", bytes_suffix(data_bytes(cap, a.get(1), n(3), n(4)))),
                    ),
                    "bufferSubData" => {
                        let bytes = bytes_suffix(data_bytes(cap, a.get(2), n(3), n(4)));
                        log.emit(
                            buf,
                            Access::Write,
                            format!("{m}{bytes} @ {}", n(1).unwrap_or(0.0)),
                        );
                    }
                    _ => log.emit(buf, Access::Read, m),
                }
            }
            "copyBufferSubData" => {
                let (Some(rt), Some(wt)) = (u(0), u(1)) else {
                    return;
                };
                let lookup = |t: u32| {
                    if t == ELEMENT_ARRAY_BUFFER {
                        vaos.get(&(ctx, st.vao)).and_then(|v| v.elements)
                    } else {
                        st.buffers.get(&t).copied()
                    }
                };
                let (src, dst) = (lookup(rt), lookup(wt));
                if let Some(dst) = dst {
                    log.emit(
                        dst,
                        Access::Write,
                        format!("{m}{} @ {}", bytes_suffix(n(4)), n(3).unwrap_or(0.0)),
                    );
                }
                if let Some(src) = src {
                    log.emit(src, Access::Read, format!("copy source ({m})"));
                }
            }
            m if TEX_WRITES.contains(&m) => {
                let Some(t) = u(0) else { return };
                if let Some(&tex) = st.textures.get(&(st.active, texture_bind_target(t))) {
                    log.emit(tex, Access::Write, tex_detail(m, a));
                }
                if m.starts_with("copyTex") {
                    fb_events(
                        log,
                        fbs,
                        st.read_fb,
                        Access::Read,
                        is_color,
                        &format!("{m} source"),
                    );
                } else if !matches!(m, "generateMipmap" | "texStorage2D" | "texStorage3D")
                    && let Some(&b) = st.buffers.get(&PIXEL_UNPACK_BUFFER)
                {
                    log.emit(b, Access::Read, format!("pixel unpack source ({m})"));
                }
            }
            "renderbufferStorage" | "renderbufferStorageMultisample" => {
                let size = match (
                    a.len().checked_sub(2).and_then(n),
                    a.last().and_then(Value::as_f64),
                ) {
                    (Some(w), Some(h)) => format!(" {w}×{h}"),
                    _ => String::new(),
                };
                log.emit(st.renderbuffer, Access::Write, format!("{m}{size}"));
            }
            m if GL_DRAWS.contains(&m) => {
                for (&(unit, _), &tex) in &st.textures {
                    if unit == UPLOAD_UNIT {
                        continue;
                    }
                    log.emit(tex, Access::Read, format!("texture unit {unit} ({m})"));
                    if let Some(&smp) = st.samplers.get(&unit) {
                        log.emit(smp, Access::Read, format!("sampler unit {unit} ({m})"));
                    }
                }
                if let Some(v) = vaos.get(&(ctx, st.vao)) {
                    for (&i, &(buf, enabled)) in &v.attribs {
                        if let Some(b) = buf.filter(|_| enabled) {
                            log.emit(b, Access::Read, format!("vertex attrib {i} ({m})"));
                        }
                    }
                    if m.contains("Elements")
                        && let Some(e) = v.elements
                    {
                        log.emit(e, Access::Read, format!("index buffer ({m})"));
                    }
                }
                for (&i, &b) in &st.ubos {
                    log.emit(b, Access::Read, format!("uniform block binding {i} ({m})"));
                }
                log.emit(st.program, Access::Read, m);
                fb_events(log, fbs, st.draw_fb, Access::Write, |_| true, m);
            }
            "clear" => {
                let mask = u(0).unwrap_or(0);
                fb_events(
                    log,
                    fbs,
                    st.draw_fb,
                    Access::Write,
                    |att| in_mask(att, mask),
                    m,
                );
            }
            "clearBufferfv" | "clearBufferiv" | "clearBufferuiv" | "clearBufferfi" => {
                let (buffer, draw_buffer) = (u(0).unwrap_or(0), u(1).unwrap_or(0));
                let keep = |att: u32| match buffer {
                    CB_COLOR => att == COLOR_ATTACHMENT0 + draw_buffer,
                    CB_DEPTH => matches!(att, DEPTH_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT),
                    CB_STENCIL => matches!(att, STENCIL_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT),
                    _ => !is_color(att),
                };
                fb_events(log, fbs, st.draw_fb, Access::Write, keep, m);
            }
            "blitFramebuffer" => {
                let mask = u(8).unwrap_or(0);
                fb_events(
                    log,
                    fbs,
                    st.read_fb,
                    Access::Read,
                    |att| in_mask(att, mask),
                    &format!("{m} source"),
                );
                fb_events(
                    log,
                    fbs,
                    st.draw_fb,
                    Access::Write,
                    |att| in_mask(att, mask),
                    m,
                );
            }
            "readPixels" => {
                fb_events(log, fbs, st.read_fb, Access::Read, is_color, m);
                if let Some(&b) = st.buffers.get(&PIXEL_PACK_BUFFER) {
                    log.emit(b, Access::Write, m);
                }
            }
            "shaderSource" | "linkProgram" | "samplerParameteri" | "samplerParameterf" => {
                if let Some(o) = obj(0) {
                    log.emit(o, Access::Write, m);
                }
            }
            "compileShader" => {
                if let Some(o) = obj(0) {
                    log.emit(o, Access::Read, m);
                }
            }
            "attachShader" | "detachShader" => {
                if let Some(p) = obj(0) {
                    log.emit(p, Access::Read, m);
                }
                if let Some(s) = obj(1) {
                    log.emit(s, Access::Read, m);
                }
            }
            "beginQuery" => {
                if let Some(q) = obj(1) {
                    log.emit(q, Access::Write, m);
                }
            }
            m if m.starts_with("delete") => {
                if let Some(o) = obj(0) {
                    log.emit(o, Access::Destroy, m);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// WebGPU

const GPU_BUFFER_STORAGE: u32 = 0x80;
const GPU_TEXTURE_STORAGE_BINDING: u32 = 0x08;
const GPU_MAP_READ: u32 = 0x1;
const GPU_MAP_WRITE: u32 = 0x2;

/// Render pass / compute pass / render bundle encoder state.
#[derive(Default)]
struct Enc {
    /// Writable render attachments: (view, name), e.g. "color attachment 0".
    attachments: Vec<(ObjId, String)>,
    groups: BTreeMap<u32, ObjId>,
    vertex: BTreeMap<u32, ObjId>,
    index: Option<ObjId>,
}

struct BindGroup {
    layout: Option<ObjId>,
    /// (binding, resource)
    entries: Vec<(u32, ObjId)>,
}

enum Mapped {
    /// Mapped for writing; `wrote` once a `webgd:mappedWrite` was recorded.
    Write {
        wrote: bool,
    },
    Read,
}

/// Resource tables shared by all encoders.
#[derive(Default)]
struct GpuRes {
    groups: HashMap<ObjId, BindGroup>,
    /// Explicit (`createBindGroupLayout`) layouts: binding → writable (storage buffer or
    /// non-read-only storage texture).
    layouts: HashMap<ObjId, HashMap<u32, bool>>,
    buffer_usage: HashMap<ObjId, u32>,
    texture_usage: HashMap<ObjId, u32>,
    mapped: HashMap<ObjId, Mapped>,
}

#[derive(Default)]
struct Gpu {
    encs: HashMap<ObjId, Enc>,
    res: GpuRes,
}

impl GpuRes {
    /// Auto-layout fallback: a buffer with STORAGE usage / a view of a texture with
    /// STORAGE_BINDING usage is treated as written by dispatches.
    fn storage_usage(&self, log: &Log, res: ObjId) -> bool {
        match log.cap.class_of(res) {
            "GPUBuffer" => self
                .buffer_usage
                .get(&res)
                .is_some_and(|u| u & GPU_BUFFER_STORAGE != 0),
            "GPUTextureView" => log
                .parent_texture(res)
                .and_then(|t| self.texture_usage.get(&t))
                .is_some_and(|u| u & GPU_TEXTURE_STORAGE_BINDING != 0),
            _ => false,
        }
    }

    /// Events for every resource of the bound bind groups. Explicit layouts decide by binding
    /// type; auto layouts write storage-usage resources for dispatches and read otherwise.
    fn bind_group_events(
        &self,
        log: &mut Log,
        slots: &BTreeMap<u32, ObjId>,
        compute: bool,
        m: &str,
    ) {
        for (&g, bg) in slots {
            let Some(group) = self.groups.get(bg) else {
                continue;
            };
            let explicit = group.layout.and_then(|l| self.layouts.get(&l));
            for &(binding, res) in &group.entries {
                let writes = match explicit.and_then(|l| l.get(&binding)) {
                    Some(&w) => w,
                    None => compute && self.storage_usage(log, res),
                };
                let access = if writes { Access::Write } else { Access::Read };
                log.emit(
                    res,
                    access,
                    format!("bind group {g} binding {binding} ({m})"),
                );
            }
        }
    }
}

impl Gpu {
    fn call(&mut self, log: &mut Log, c: &Call, m: &str) {
        let cap = log.cap;
        let a = &c.args;
        let n = |i: usize| a.get(i).and_then(Value::as_f64);
        let obj = |i: usize| a.get(i).and_then(Value::as_obj);
        let field = |i: usize, k: &str| a.get(i).and_then(|d| cap.dict_get(d, k));
        let mip = |i: usize| field(i, "mipLevel").and_then(Value::as_f64).unwrap_or(0.0);
        let ext = |i: usize| {
            extent(cap, a.get(i))
                .map(|e| format!(" {e}"))
                .unwrap_or_default()
        };
        match (cap.class_of(c.this), m) {
            ("GPUDevice", "createBuffer") => {
                let Some(id) = c.result.as_obj() else { return };
                self.res.buffer_usage.insert(
                    id,
                    field(0, "usage").and_then(Value::as_f64).unwrap_or(0.0) as u32,
                );
                if is_true(field(0, "mappedAtCreation")) {
                    self.res.mapped.insert(id, Mapped::Write { wrote: false });
                }
            }
            ("GPUDevice", "createTexture") => {
                let Some(id) = c.result.as_obj() else { return };
                self.res.texture_usage.insert(
                    id,
                    field(0, "usage").and_then(Value::as_f64).unwrap_or(0.0) as u32,
                );
            }
            ("GPUDevice", "createBindGroupLayout") => {
                let Some(id) = c.result.as_obj() else { return };
                let mut kinds = HashMap::new();
                for e in field(0, "entries").and_then(Value::as_array).unwrap_or(&[]) {
                    let Some(binding) = cap.dict_get(e, "binding").and_then(Value::as_f64) else {
                        continue;
                    };
                    let writes = if let Some(b) = cap.dict_get(e, "buffer") {
                        str_of(cap, cap.dict_get(b, "type")) == Some("storage")
                    } else if let Some(t) = cap.dict_get(e, "storageTexture") {
                        str_of(cap, cap.dict_get(t, "access")) != Some("read-only")
                    } else {
                        false
                    };
                    kinds.insert(binding as u32, writes);
                }
                self.res.layouts.insert(id, kinds);
            }
            ("GPUDevice", "createBindGroup") => {
                let Some(id) = c.result.as_obj() else { return };
                let entries = field(0, "entries")
                    .and_then(Value::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|e| {
                        let binding = cap.dict_get(e, "binding").and_then(Value::as_f64)? as u32;
                        let r = cap.dict_get(e, "resource")?;
                        let res = r
                            .as_obj()
                            .or_else(|| cap.dict_get(r, "buffer").and_then(Value::as_obj))?;
                        Some((binding, res))
                    })
                    .collect();
                self.res.groups.insert(
                    id,
                    BindGroup {
                        layout: field(0, "layout").and_then(Value::as_obj),
                        entries,
                    },
                );
            }
            ("GPUCommandEncoder", "beginRenderPass") => {
                let Some(pass) = c.result.as_obj() else {
                    return;
                };
                let mut enc = Enc::default();
                let cols = field(0, "colorAttachments")
                    .and_then(Value::as_array)
                    .unwrap_or(&[]);
                for (i, col) in cols.iter().enumerate() {
                    let Some(view) = cap.dict_get(col, "view").and_then(Value::as_obj) else {
                        continue;
                    };
                    let op = if str_of(cap, cap.dict_get(col, "loadOp")) == Some("clear") {
                        "clear"
                    } else {
                        "load"
                    };
                    let name = format!("color attachment {i}");
                    log.emit(view, Access::Write, format!("{name} ({op})"));
                    if let Some(rt) = cap.dict_get(col, "resolveTarget").and_then(Value::as_obj) {
                        log.emit(rt, Access::Write, format!("resolve target {i}"));
                    }
                    enc.attachments.push((view, name));
                }
                if let Some(ds) = field(0, "depthStencilAttachment")
                    && let Some(view) = cap.dict_get(ds, "view").and_then(Value::as_obj)
                {
                    let has = |k: &str| cap.dict_get(ds, k).is_some_and(|v| !v.is_nullish());
                    let ro = |k: &str| is_true(cap.dict_get(ds, k));
                    let writes = (has("depthLoadOp") && !ro("depthReadOnly"))
                        || (has("stencilLoadOp") && !ro("stencilReadOnly"));
                    let name = "depth-stencil attachment".to_owned();
                    if writes {
                        let clear = ["depthLoadOp", "stencilLoadOp"]
                            .iter()
                            .any(|k| str_of(cap, cap.dict_get(ds, k)) == Some("clear"));
                        log.emit(
                            view,
                            Access::Write,
                            format!("{name} ({})", if clear { "clear" } else { "load" }),
                        );
                        enc.attachments.push((view, name));
                    } else {
                        log.emit(view, Access::Read, format!("{name} (read-only)"));
                    }
                }
                self.encs.insert(pass, enc);
            }
            ("GPUCommandEncoder", "beginComputePass") => {
                if let Some(pass) = c.result.as_obj() {
                    self.encs.insert(pass, Enc::default());
                }
            }
            ("GPUCommandEncoder", "copyBufferToBuffer") => {
                // (src, srcOffset, dst, dstOffset, size?) or (src, dst, size?)
                let dst = a.iter().skip(1).find_map(Value::as_obj);
                let size = match a.len() {
                    5 => n(4),
                    3 => n(2),
                    _ => None,
                };
                if let Some(dst) = dst {
                    log.emit(dst, Access::Write, format!("{m}{}", bytes_suffix(size)));
                }
                if let Some(src) = obj(0) {
                    log.emit(src, Access::Read, format!("copy source ({m})"));
                }
            }
            (
                "GPUCommandEncoder",
                "copyBufferToTexture" | "copyTextureToBuffer" | "copyTextureToTexture",
            ) => {
                let src = field(0, "texture")
                    .or_else(|| field(0, "buffer"))
                    .and_then(Value::as_obj);
                let dst = field(1, "texture")
                    .or_else(|| field(1, "buffer"))
                    .and_then(Value::as_obj);
                if let Some(dst) = dst {
                    let detail = if m == "copyTextureToBuffer" {
                        format!("{m}{}", ext(2))
                    } else {
                        format!("{m} mip {}{}", mip(1), ext(2))
                    };
                    log.emit(dst, Access::Write, detail);
                }
                if let Some(src) = src {
                    log.emit(src, Access::Read, format!("copy source ({m})"));
                }
            }
            ("GPUCommandEncoder", "clearBuffer") => {
                if let Some(b) = obj(0) {
                    log.emit(b, Access::Write, m);
                }
            }
            ("GPUCommandEncoder", "resolveQuerySet") => {
                if let Some(dst) = obj(3) {
                    log.emit(
                        dst,
                        Access::Write,
                        format!("{m} {} queries", n(2).unwrap_or(0.0)),
                    );
                }
                if let Some(qs) = obj(0) {
                    log.emit(qs, Access::Read, m);
                }
            }
            ("GPUQueue", "writeBuffer") => {
                if let Some(b) = obj(0) {
                    let bytes = bytes_suffix(data_bytes(cap, a.get(2), n(3), n(4)));
                    log.emit(
                        b,
                        Access::Write,
                        format!("{m}{bytes} @ {}", n(1).unwrap_or(0.0)),
                    );
                }
            }
            ("GPUQueue", "writeTexture") => {
                if let Some(t) = field(0, "texture").and_then(Value::as_obj) {
                    log.emit(t, Access::Write, format!("{m} mip {}{}", mip(0), ext(3)));
                }
            }
            ("GPUQueue", "copyExternalImageToTexture") => {
                if let Some(t) = field(1, "texture").and_then(Value::as_obj) {
                    log.emit(t, Access::Write, format!("{m} mip {}{}", mip(1), ext(2)));
                }
            }
            ("GPUBuffer", "mapAsync") => {
                let mode = n(0).unwrap_or(0.0) as u32;
                if mode & GPU_MAP_WRITE != 0 {
                    self.res
                        .mapped
                        .insert(c.this, Mapped::Write { wrote: false });
                } else if mode & GPU_MAP_READ != 0 {
                    self.res.mapped.insert(c.this, Mapped::Read);
                    log.emit(c.this, Access::Read, "mapAsync (read)");
                }
            }
            ("GPUBuffer", "webgd:mappedWrite") => {
                let bytes = bytes_suffix(data_bytes(cap, a.get(1), None, None));
                log.emit(
                    c.this,
                    Access::Write,
                    format!("mapped write{bytes} @ {}", n(0).unwrap_or(0.0)),
                );
                if let Some(Mapped::Write { wrote }) = self.res.mapped.get_mut(&c.this) {
                    *wrote = true;
                }
            }
            ("GPUBuffer", "unmap") => {
                // Mapped bytes recorded via `webgd:mappedWrite` already counted as the write.
                if let Some(Mapped::Write { wrote: false }) = self.res.mapped.remove(&c.this) {
                    log.emit(c.this, Access::Write, "unmap (mapped write)");
                }
            }
            (_, "destroy") => log.emit(c.this, Access::Destroy, m),
            ("GPURenderPassEncoder" | "GPUComputePassEncoder" | "GPURenderBundleEncoder", _) => {
                self.pass_call(log, c, m)
            }
            _ => {}
        }
    }

    fn pass_call(&mut self, log: &mut Log, c: &Call, m: &str) {
        let a = &c.args;
        let slot = |i: usize| a.get(i).and_then(Value::as_f64).map(|v| v as u32);
        let obj = |i: usize| a.get(i).and_then(Value::as_obj);
        let enc = self.encs.entry(c.this).or_default();
        match m {
            "setPipeline" => {
                if let Some(p) = obj(0) {
                    log.emit(p, Access::Read, m);
                }
            }
            "setBindGroup" => {
                let Some(g) = slot(0) else { return };
                assign(&mut enc.groups, g, obj(1));
                if let Some(bg) = obj(1) {
                    log.emit(bg, Access::Read, format!("{m} {g}"));
                }
            }
            "setVertexBuffer" => {
                if let Some(s) = slot(0) {
                    assign(&mut enc.vertex, s, obj(1));
                }
            }
            "setIndexBuffer" => enc.index = obj(0),
            "draw" | "drawIndexed" | "drawIndirect" | "drawIndexedIndirect" => {
                for (&s, &b) in &enc.vertex {
                    log.emit(b, Access::Read, format!("vertex buffer slot {s} ({m})"));
                }
                if m.starts_with("drawIndexed")
                    && let Some(i) = enc.index
                {
                    log.emit(i, Access::Read, format!("index buffer ({m})"));
                }
                if m.ends_with("Indirect")
                    && let Some(b) = obj(0)
                {
                    log.emit(b, Access::Read, format!("indirect args ({m})"));
                }
                self.res.bind_group_events(log, &enc.groups, false, m);
                for (view, name) in &enc.attachments {
                    log.emit(*view, Access::Write, format!("{name} ({m})"));
                }
            }
            "dispatchWorkgroups" | "dispatchWorkgroupsIndirect" => {
                if m.ends_with("Indirect")
                    && let Some(b) = obj(0)
                {
                    log.emit(b, Access::Read, format!("indirect args ({m})"));
                }
                self.res.bind_group_events(log, &enc.groups, true, m);
            }
            "executeBundles" => {
                for b in a
                    .first()
                    .and_then(Value::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(Value::as_obj)
                {
                    log.emit(b, Access::Read, m);
                }
                for (view, name) in &enc.attachments {
                    log.emit(*view, Access::Write, format!("{name} ({m})"));
                }
                // WebGPU resets pass state after executing bundles.
                enc.groups.clear();
                enc.vertex.clear();
                enc.index = None;
            }
            "end" => {
                self.encs.remove(&c.this);
            }
            _ => {}
        }
    }
}
