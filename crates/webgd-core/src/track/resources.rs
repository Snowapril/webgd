//! Resource browser rows.
use crate::capture::{Call, Capture, ObjId, Section, Value};
use crate::track::gl;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    PreFrame,
    Frame(usize),
}

#[derive(Clone, PartialEq, Debug)]
pub struct ResourceRow {
    pub id: ObjId,
    pub class: String,
    pub label: String,
    pub summary: String,
    pub origin: Origin,
}

pub const RESOURCE_CLASSES: &[&str] = &[
    "WebGLBuffer", "WebGLTexture", "WebGLRenderbuffer", "WebGLFramebuffer", "WebGLShader",
    "WebGLProgram", "WebGLSampler", "WebGLVertexArrayObject", "WebGLQuery",
    "GPUBuffer", "GPUTexture", "GPUTextureView", "GPUSampler", "GPUShaderModule", "GPUBindGroup",
    "GPUBindGroupLayout", "GPUPipelineLayout", "GPURenderPipeline", "GPUComputePipeline",
    "GPUQuerySet", "GPURenderBundle",
];

/// `GPUBufferUsage` flag names in bit order.
pub const GPU_BUFFER_USAGE: &[(u32, &str)] = &[
    (0x1, "MAP_READ"), (0x2, "MAP_WRITE"), (0x4, "COPY_SRC"), (0x8, "COPY_DST"), (0x10, "INDEX"),
    (0x20, "VERTEX"), (0x40, "UNIFORM"), (0x80, "STORAGE"), (0x100, "INDIRECT"), (0x200, "QUERY_RESOLVE"),
];

/// `A|B|C` names of the set usage bits (unknown bits as hex).
pub fn gpu_buffer_usage_names(usage: u32) -> String {
    let mut names: Vec<String> = GPU_BUFFER_USAGE.iter().filter(|(b, _)| usage & b != 0).map(|(_, n)| n.to_string()).collect();
    let rest = usage & !0x3ff;
    if rest != 0 {
        names.push(format!("{rest:#x}"));
    }
    names.join("|")
}

/// Rows for live resources: objects created in setup/frame or referenced by a snapshot/frame
/// record. The wire object table is never pruned, so objects from earlier frames that are
/// neither are omitted.
pub fn list(cap: &Capture) -> Vec<ResourceRow> {
    let creators = creation_index(cap);
    let referenced = referenced_ids(cap);
    let labels = latest_labels(cap);
    let buffer_sizes = gl_buffer_sizes(cap);
    cap.objects
        .values()
        .filter(|o| RESOURCE_CLASSES.contains(&cap.str(o.class)))
        .filter(|o| creators.contains_key(&o.id) || referenced.contains(&o.id))
        .map(|o| {
            let created = creators.get(&o.id).copied();
            let creation = created.map(|(s, i)| &cap.section(s)[i]);
            let desc = creation.and_then(|c| c.args.first());
            let label = labels
                .get(&o.id)
                .cloned()
                .or_else(|| desc.and_then(|d| str_of(cap, cap.dict_get(d, "label"))).map(str::to_owned))
                .unwrap_or_default();
            ResourceRow {
                id: o.id,
                class: cap.str(o.class).to_owned(),
                label,
                summary: summary(cap, o.id, creation, &buffer_sizes),
                origin: match created {
                    Some((Section::Frame, i)) => Origin::Frame(i),
                    _ => Origin::PreFrame,
                },
            }
        })
        .collect()
}

/// First call (setup, then frame) whose result is each object; same rule as `gpu::creation_call`.
fn creation_index(cap: &Capture) -> HashMap<ObjId, (Section, usize)> {
    let mut map = HashMap::new();
    for s in [Section::Setup, Section::Frame] {
        for (i, c) in cap.section(s).iter().enumerate() {
            if let Some(id) = c.result.as_obj() {
                map.entry(id).or_insert((s, i));
            }
        }
    }
    map
}

/// Every object that is `this` of, or an ObjRef inside, a snapshot or frame record.
fn referenced_ids(cap: &Capture) -> HashSet<ObjId> {
    fn walk(v: &Value, out: &mut HashSet<ObjId>) {
        match v {
            Value::ObjRef(id) => {
                out.insert(*id);
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            Value::Dict(entries) => entries.iter().for_each(|(_, i)| walk(i, out)),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    for c in cap.snapshot.iter().chain(&cap.frame) {
        out.insert(c.this);
        c.args.iter().chain([&c.result]).for_each(|v| walk(v, &mut out));
    }
    out
}

fn latest_labels(cap: &Capture) -> HashMap<ObjId, String> {
    let mut map = HashMap::new();
    for c in cap.setup.iter().chain(&cap.frame) {
        if cap.method(c) == "set:label" {
            map.insert(c.this, str_of(cap, c.args.first()).unwrap_or("").to_owned());
        }
    }
    map
}

fn str_of<'a>(cap: &'a Capture, v: Option<&Value>) -> Option<&'a str> {
    match v {
        Some(Value::String(s)) => Some(cap.str(*s)),
        _ => None,
    }
}

fn summary(cap: &Capture, id: ObjId, creation: Option<&Call>, buffer_sizes: &HashMap<ObjId, f64>) -> String {
    let desc = creation.and_then(|c| c.args.first());
    let dn = |k: &str| desc.and_then(|d| cap.dict_get(d, k)).and_then(Value::as_f64);
    match cap.class_of(id) {
        "GPUBuffer" => match dn("size") {
            Some(size) => {
                let usage = gpu_buffer_usage_names(dn("usage").unwrap_or(0.0) as u32);
                format!("{size} B {usage}")
            }
            None => String::new(),
        },
        "GPUTexture" => {
            if creation.is_some_and(|c| cap.method(c) == "getCurrentTexture") {
                return "canvas texture".into();
            }
            let Some(d) = desc else { return String::new() };
            let format = str_of(cap, cap.dict_get(d, "format")).unwrap_or("?");
            let size = cap.dict_get(d, "size");
            let dim = |i: usize, key: &str| -> f64 {
                let v = match size {
                    Some(Value::Array(a)) => a.get(i),
                    Some(s) => cap.dict_get(s, key),
                    None => None,
                };
                v.and_then(Value::as_f64).unwrap_or(1.0)
            };
            let mips = dn("mipLevelCount").unwrap_or(1.0);
            format!("{format} {}×{}×{} {mips} mips", dim(0, "width"), dim(1, "height"), dim(2, "depthOrArrayLayers"))
        }
        "GPUTextureView" => match cap.objects.get(&id).and_then(|o| o.parent) {
            Some(tex) => format!("view of {}#{}", cap.class_of(tex), tex.0),
            None => String::new(),
        },
        "GPUShaderModule" => {
            let code = desc.and_then(|d| str_of(cap, cap.dict_get(d, "code"))).unwrap_or("");
            wgsl_entry_points(code).iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", ")
        }
        "GPUQuerySet" => {
            let ty = desc.and_then(|d| str_of(cap, cap.dict_get(d, "type"))).unwrap_or("?");
            format!("{ty} ×{}", dn("count").unwrap_or(0.0))
        }
        "WebGLShader" => match creation.and_then(|c| c.args.first()).and_then(Value::as_f64) {
            Some(35633.0) => "vertex".into(),
            Some(35632.0) => "fragment".into(),
            _ => String::new(),
        },
        "WebGLBuffer" => buffer_sizes.get(&id).map(|s| format!("{s} B")).unwrap_or_default(),
        "WebGLTexture" => match gl::texture_dims(cap, usize::MAX, id) {
            Some(d) => {
                let target = match d.target {
                    gl::TEXTURE_2D => "2D",
                    gl::TEXTURE_CUBE_MAP => "CUBE",
                    gl::TEXTURE_3D => "3D",
                    gl::TEXTURE_2D_ARRAY => "2D_ARRAY",
                    _ => "?",
                };
                let mips = d.levels.keys().filter(|k| k.1 == 0).count();
                match d.levels.get(&(0, 0)) {
                    Some((w, h, z)) => format!("{target} {:#06x} {w}×{h}×{z} {mips} mips", d.internalformat),
                    None => format!("{target} {:#06x}", d.internalformat),
                }
            }
            None => String::new(),
        },
        "WebGLRenderbuffer" => snapshot_dict(cap, "webgd:glRenderbuffer", id)
            .map(|d| {
                let n = |k: &str| cap.dict_get(d, k).and_then(Value::as_f64).unwrap_or(0.0);
                let samples = n("samples");
                let ms = if samples > 0.0 { format!(" ×{samples} samples") } else { String::new() };
                format!("{:#06x} {}×{}{ms}", n("internalformat") as u32, n("width"), n("height"))
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Second arg of the snapshot record `method(ObjRef(id), Dict)`.
fn snapshot_dict<'a>(cap: &'a Capture, method: &str, id: ObjId) -> Option<&'a Value> {
    cap.snapshot
        .iter()
        .find(|c| cap.method(c) == method && c.args.first().and_then(Value::as_obj) == Some(id))
        .and_then(|c| c.args.get(1))
}

/// `(stage, name)` of each `@vertex` / `@fragment` / `@compute` function in WGSL source.
pub fn wgsl_entry_points(code: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (attr, stage) in [("@vertex", "vertex"), ("@fragment", "fragment"), ("@compute", "compute")] {
        let mut rest = code;
        while let Some(p) = rest.find(attr) {
            rest = &rest[p + attr.len()..];
            if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let Some(f) = find_fn_keyword(rest) else { break };
            let name: String = rest[f..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            out.push((stage, name));
        }
    }
    out
}

/// Byte offset just past the next `fn` keyword and following whitespace.
fn find_fn_keyword(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(p) = s[i..].find("fn") {
        let at = i + p;
        let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let after = at + 2;
        if before_ok && b.get(after).is_some_and(|c| c.is_ascii_whitespace()) {
            let skip = s[after..].len() - s[after..].trim_start().len();
            return Some(after + skip);
        }
        i = at + 2;
    }
    None
}

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
/// (bind target, glState binding key) for non-VAO buffer targets.
const BUFFER_TARGETS: &[(u32, &str)] = &[
    (ARRAY_BUFFER, "34964"),
    (36662, "36662"), // COPY_READ_BUFFER
    (36663, "36663"), // COPY_WRITE_BUFFER
    (35051, "35053"), // PIXEL_PACK_BUFFER
    (35052, "35055"), // PIXEL_UNPACK_BUFFER
    (35345, "35368"), // UNIFORM_BUFFER
    (35982, "35983"), // TRANSFORM_FEEDBACK_BUFFER
];

/// Latest known size of every GL buffer: snapshot `webgd:glBuffer` sizes, then frame `bufferData`
/// through tracked `bindBuffer*` / `bindVertexArray` bindings.
fn gl_buffer_sizes(cap: &Capture) -> HashMap<ObjId, f64> {
    let mut sizes = HashMap::new();
    // (ctx, target) → buffer; element buffers per (ctx, vao) with ObjId(0) = default VAO.
    let mut bound: HashMap<(ObjId, u32), ObjId> = HashMap::new();
    let mut vao: HashMap<ObjId, ObjId> = HashMap::new();
    let mut elements: HashMap<(ObjId, ObjId), ObjId> = HashMap::new();
    for c in &cap.snapshot {
        let a = &c.args;
        match cap.method(c) {
            "webgd:glBuffer" => {
                if let (Some(id), Some(size)) = (a.first().and_then(Value::as_obj), a.get(1).and_then(|d| cap.dict_get(d, "size")).and_then(Value::as_f64)) {
                    sizes.insert(id, size);
                }
            }
            "webgd:glState" => {
                let Some(s) = a.first() else { continue };
                for &(t, key) in BUFFER_TARGETS {
                    if let Some(b) = cap.dict_get(s, key).and_then(Value::as_obj) {
                        bound.insert((c.this, t), b);
                    }
                }
                vao.insert(c.this, cap.dict_get(s, "34229").and_then(Value::as_obj).unwrap_or(ObjId(0)));
            }
            "webgd:glVertexArray" => {
                let v = a.first().and_then(Value::as_obj).unwrap_or(ObjId(0));
                if let Some(e) = a.get(1).and_then(|d| cap.dict_get(d, "elementBuffer")).and_then(Value::as_obj) {
                    elements.insert((c.this, v), e);
                }
            }
            _ => {}
        }
    }
    for c in &cap.frame {
        let a = &c.args;
        let n = |i: usize| a.get(i).and_then(Value::as_f64);
        let ctx = c.this;
        let cur_vao = |vao: &HashMap<ObjId, ObjId>| vao.get(&ctx).copied().unwrap_or(ObjId(0));
        match cap.method(c) {
            "bindVertexArray" => {
                vao.insert(ctx, a.first().and_then(Value::as_obj).unwrap_or(ObjId(0)));
            }
            "bindBuffer" | "bindBufferBase" | "bindBufferRange" => {
                let Some(t) = n(0).map(|t| t as u32) else { continue };
                let buf_arg = if cap.method(c) == "bindBuffer" { 1 } else { 2 };
                let buf = a.get(buf_arg).and_then(Value::as_obj);
                let key = (ctx, cur_vao(&vao));
                match (t == ELEMENT_ARRAY_BUFFER, buf) {
                    (true, Some(b)) => {
                        elements.insert(key, b);
                    }
                    (true, None) => {
                        elements.remove(&key);
                    }
                    (false, Some(b)) => {
                        bound.insert((ctx, t), b);
                    }
                    (false, None) => {
                        bound.remove(&(ctx, t));
                    }
                }
            }
            "bufferData" => {
                let Some(t) = n(0).map(|t| t as u32) else { continue };
                let buf = if t == ELEMENT_ARRAY_BUFFER {
                    elements.get(&(ctx, cur_vao(&vao))).copied()
                } else {
                    bound.get(&(ctx, t)).copied()
                };
                let Some(buf) = buf else { continue };
                let size = match a.get(1) {
                    Some(Value::Number(s)) => *s,
                    Some(Value::Typed { kind, blob }) => {
                        let es = kind.element_size() as f64;
                        let total = cap.blob(*blob).len() as f64;
                        let off = n(3).unwrap_or(0.0) * es;
                        match n(4).filter(|&l| l > 0.0) {
                            Some(len) => len * es,
                            None => (total - off).max(0.0),
                        }
                    }
                    _ => continue,
                };
                sizes.insert(buf, size);
            }
            _ => {}
        }
    }
    sizes
}
