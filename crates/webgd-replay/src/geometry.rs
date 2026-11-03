//! Draw geometry extraction at the replayed point: draw arguments, vertex input layout, pre-VS
//! attribute fetch and post-VS capture (WebGPU: the vertex shader transformed into a compute
//! shader; WebGL2: transform feedback with a capture program sharing the original vertex shader).
use crate::engine::{await_value, jserr};
use crate::gl::c::*;
use crate::gl::inspect::{plain_uniforms, read_buffer as gl_read_buffer};
use crate::gl::uniforms::set_uniform;
use crate::gl::{GlCtx, enum_name, n};
use crate::gpu::{buf_usage, map_read};
use crate::util::{creation_call, creation_desc, get_num, get_str};
use crate::value::to_js;
use crate::{GeometryData, ReplayError, ReplayState, js};
use std::collections::HashMap;
use wasm_bindgen::JsValue;
use webgd_analysis::vsxform::{self, VsCompute, VsOutput};
use webgd_core::geometry::{
    self as geo, AttribFormat, AttribStream, IndexFormat, LayoutAttrib, LayoutBuffer, ScalarKind,
    Topology,
};
use webgd_core::track::commands::{CmdKind, classify};
use webgd_core::track::gpu::{PassState, state_at};
use webgd_core::{Call, Capture, ObjId, Value};

const CURRENT_VERTEX_ATTRIB: u32 = 0x8626;
const TRANSFORM_FEEDBACK: u32 = 0x8E22;
const TRANSFORM_FEEDBACK_ACTIVE: u32 = 0x8E24;
const TRANSFORM_FEEDBACK_BINDING: u32 = 0x8E25;
const INTERLEAVED_ATTRIBS: u32 = 0x8C8C;
const POINTS: u32 = 0;
const STREAM_READ: u32 = 0x88E1;
const NO_ERROR: u32 = 0;
/// Upper bound of the transform feedback capture buffer.
const MAX_TF_BYTES: f64 = 256.0 * 1024.0 * 1024.0;
/// Workgroup size of the generated compute entry (vsxform contract).
const VS_WORKGROUP: usize = 64;

type PostVs = Result<(Vec<AttribStream>, Option<usize>), String>;

fn unsup(s: impl Into<String>) -> ReplayError {
    ReplayError::Unsupported(s.into())
}

pub(crate) async fn geometry(
    st: &ReplayState,
    cap: &Capture,
    index: usize,
    instance: u32,
) -> Result<GeometryData, ReplayError> {
    if st.frame_start {
        return Err(unsup(
            "geometry needs a selected draw (replay is at frame start)",
        ));
    }
    if index != st.stop {
        return Err(unsup(format!(
            "geometry of #{index} requested from a replay stopped at #{}",
            st.stop
        )));
    }
    let call = cap
        .frame
        .get(index)
        .ok_or_else(|| unsup(format!("no frame command #{index}")))?;
    if classify(cap, call) != CmdKind::Draw {
        return Err(unsup(format!(
            "#{index} {} is not a draw",
            cap.method(call)
        )));
    }
    if cap.class_of(call.this).starts_with("GPU") {
        gpu_geometry(st, cap, index, call, instance).await
    } else {
        gl_geometry(st, cap, index, call, instance)
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn index_size(f: IndexFormat) -> u64 {
    match f {
        IndexFormat::U8 => 1,
        IndexFormat::U16 => 2,
        IndexFormat::U32 => 4,
    }
}

fn restart_value(f: IndexFormat) -> u32 {
    match f {
        IndexFormat::U8 => 0xFF,
        IndexFormat::U16 => 0xFFFF,
        IndexFormat::U32 => 0xFFFF_FFFF,
    }
}

/// (draw order, sorted unique vertex ids without restart entries).
fn draw_ids(
    indices: Option<&[u32]>,
    first: u32,
    count: u32,
    base_vertex: i64,
    restart: Option<u32>,
) -> (Vec<u32>, Vec<u32>) {
    let (first, count) = match indices {
        Some(ix) => (0, ix.len() as u32),
        None => (first, count),
    };
    let order = geo::draw_vertex_ids(indices, first, count, base_vertex, restart);
    let mut ids: Vec<u32> = match indices {
        Some(ix) => ix
            .iter()
            .zip(&order)
            .filter(|(raw, _)| Some(**raw) != restart)
            .map(|(_, v)| *v)
            .collect(),
        None => order.clone(),
    };
    ids.sort_unstable();
    ids.dedup();
    (order, ids)
}

/// Decodes indices `[offset, offset + count·size)` of `bytes`, noting a short buffer.
fn read_indices(
    bytes: &[u8],
    format: IndexFormat,
    offset: u64,
    count: u32,
    notes: &mut Vec<String>,
) -> Vec<u32> {
    let ix = geo::decode_indices(bytes, format, offset, count);
    if ix.len() < count as usize {
        notes.push(format!(
            "index buffer holds only {} of {count} indices",
            ix.len()
        ));
    }
    ix
}

fn clamp_instance(instance: u32, count: u32, notes: &mut Vec<String>) -> u32 {
    if count == 0 {
        notes.push("instance count is 0: the draw produces no vertices".into());
        return 0;
    }
    if instance >= count {
        notes.push(format!(
            "instance {instance} out of range; showing instance {}",
            count - 1
        ));
        return count - 1;
    }
    instance
}

fn stream_label(s: &AttribStream) -> String {
    match (&s.name, s.location) {
        (name, _) if !name.is_empty() => name.clone(),
        (_, Some(l)) => format!("@location({l})"),
        _ => "attribute".into(),
    }
}

fn range_notes(inputs: &[AttribStream], notes: &mut Vec<String>) {
    for s in inputs {
        let miss = s.values.iter().filter(|v| v.is_none()).count();
        if miss > 0 {
            notes.push(format!(
                "{}: {miss} of {} vertices out of buffer range",
                stream_label(s),
                s.values.len()
            ));
        }
    }
}

fn kind_of(k: vsxform::ScalarKind) -> ScalarKind {
    match k {
        vsxform::ScalarKind::Float => ScalarKind::Float,
        vsxform::ScalarKind::Sint => ScalarKind::Sint,
        vsxform::ScalarKind::Uint => ScalarKind::Uint,
    }
}

fn out_format(kind: ScalarKind, components: u32) -> String {
    let base = match kind {
        ScalarKind::Float => "f32",
        ScalarKind::Sint => "i32",
        ScalarKind::Uint => "u32",
    };
    if components <= 1 {
        base.to_string()
    } else {
        format!("{base}x{components}")
    }
}

/// Decodes output records: output k's components are 32-bit words at `offsets[k]` inside each
/// `stride`-byte record. Invalid records (inputs unavailable) yield `None`.
fn decode_outputs(
    outs: &[VsOutput],
    bytes: &[u8],
    stride: usize,
    offsets: &[usize],
    valid: &[bool],
) -> (Vec<AttribStream>, Option<usize>) {
    let streams = outs
        .iter()
        .zip(offsets)
        .map(|(o, &off)| {
            let kind = kind_of(o.kind);
            let values = valid
                .iter()
                .enumerate()
                .map(|(r, ok)| {
                    if !ok {
                        return None;
                    }
                    let base = r * stride + off;
                    let mut v = [0.0, 0.0, 0.0, 1.0];
                    for (c, slot) in v.iter_mut().enumerate().take(o.components.min(4) as usize) {
                        let at = base + 4 * c;
                        let word = <[u8; 4]>::try_from(bytes.get(at..at + 4)?).ok()?;
                        let bits = u32::from_le_bytes(word);
                        *slot = match kind {
                            ScalarKind::Float => f32::from_bits(bits) as f64,
                            ScalarKind::Sint => bits as i32 as f64,
                            ScalarKind::Uint => bits as f64,
                        };
                    }
                    Some(v)
                })
                .collect();
            AttribStream {
                name: o.name.clone(),
                location: o.location,
                slot: None,
                format: out_format(kind, o.components),
                components: o.components,
                kind,
                values,
            }
        })
        .collect();
    (streams, outs.iter().position(|o| o.builtin_position))
}

fn split_post_vs(r: PostVs) -> (Result<Vec<AttribStream>, String>, Option<usize>) {
    match r {
        Ok((o, p)) => (Ok(o), p),
        Err(e) => (Err(e), None),
    }
}

// ---------------------------------------------------------------------------------------------
// WebGPU
// ---------------------------------------------------------------------------------------------

async fn gpu_geometry(
    st: &ReplayState,
    cap: &Capture,
    index: usize,
    call: &Call,
    instance: u32,
) -> Result<GeometryData, ReplayError> {
    let method = cap.method(call);
    if method == "executeBundles" {
        return Err(unsup("executeBundles: select a draw inside the bundle"));
    }
    let ps = state_at(cap, index)
        .pass
        .filter(|p| !p.compute && p.pass == call.this)
        .ok_or_else(|| unsup("draw outside a tracked render pass"))?;
    let pipeline = ps.pipeline.ok_or_else(|| unsup("no pipeline set"))?;
    let pdesc = creation_desc(cap, pipeline)
        .ok_or_else(|| unsup(format!("descriptor of pipeline #{} unknown", pipeline.0)))?;
    let vertex = cap
        .dict_get(pdesc, "vertex")
        .ok_or_else(|| unsup("pipeline descriptor has no vertex stage"))?;
    let mut notes = Vec::new();

    // Draw arguments: (indexed, count, first vertex/index, base vertex, instance count, first instance).
    let a = |i: usize, d: f64| call.args.get(i).and_then(Value::as_f64).unwrap_or(d);
    let (indexed, count, first, base_vertex, instance_count, first_instance) = match method {
        "draw" => (false, a(0, 0.0), a(2, 0.0), 0.0, a(1, 1.0), a(3, 0.0)),
        "drawIndexed" => (true, a(0, 0.0), a(2, 0.0), a(3, 0.0), a(1, 1.0), a(4, 0.0)),
        "drawIndirect" | "drawIndexedIndirect" => {
            let buf = call
                .args
                .first()
                .and_then(Value::as_obj)
                .ok_or_else(|| unsup("indirect buffer missing"))?;
            let off = a(1, 0.0) as usize;
            let bytes = st.read_buffer(cap, buf).await?;
            let words = if method == "drawIndirect" { 4 } else { 5 };
            let w: Vec<u32> = (0..words)
                .map(|k| {
                    let at = off + 4 * k;
                    bytes
                        .get(at..at + 4)
                        .and_then(|b| <[u8; 4]>::try_from(b).ok())
                        .map(u32::from_le_bytes)
                })
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    unsup(format!(
                        "indirect arguments at offset {off} exceed buffer #{}",
                        buf.0
                    ))
                })?;
            notes.push(format!(
                "indirect arguments read from buffer #{} at offset {off}",
                buf.0
            ));
            if words == 4 {
                (
                    false,
                    w[0] as f64,
                    w[2] as f64,
                    0.0,
                    w[1] as f64,
                    w[3] as f64,
                )
            } else {
                let bv = w[3] as i32 as f64;
                (true, w[0] as f64, w[2] as f64, bv, w[1] as f64, w[4] as f64)
            }
        }
        m => return Err(unsup(format!("{m} is not supported by the geometry view"))),
    };
    let count = count as u32;

    let topo = cap
        .dict_get(pdesc, "primitive")
        .and_then(|p| get_str(cap, p, "topology"))
        .unwrap_or("triangle-list");
    let topology =
        Topology::from_gpu(topo).ok_or_else(|| unsup(format!("primitive topology {topo}")))?;
    let strip = matches!(topology, Topology::LineStrip | Topology::TriangleStrip);

    let (indices, restart) = if indexed {
        let (ib, fmt, off, size) = ps
            .index_buffer
            .clone()
            .ok_or_else(|| unsup("indexed draw without an index buffer"))?;
        let format = match fmt.as_str() {
            "uint16" => IndexFormat::U16,
            "uint32" => IndexFormat::U32,
            f => return Err(unsup(format!("index format {f}"))),
        };
        let mut bytes = st.read_buffer(cap, ib).await?;
        if let Some(s) = size {
            bytes.truncate((off + s) as usize);
        }
        let start = off as u64 + first as u64 * index_size(format);
        let ix = read_indices(&bytes, format, start, count, &mut notes);
        (Some(ix), strip.then(|| restart_value(format)))
    } else {
        (None, None)
    };
    let (draw_order, vertex_ids) = draw_ids(
        indices.as_deref(),
        first as u32,
        count,
        base_vertex as i64,
        restart,
    );

    // Layout from `vertex.buffers` (null entries leave the slot empty).
    let mut layout = Vec::new();
    let bufs = cap
        .dict_get(vertex, "buffers")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for (slot, b) in bufs.iter().enumerate() {
        if b.is_nullish() {
            continue;
        }
        let slot = slot as u32;
        let per_instance = get_str(cap, b, "stepMode") == Some("instance");
        let attributes = cap
            .dict_get(b, "attributes")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|at| LayoutAttrib {
                location: get_num(cap, at, "shaderLocation").unwrap_or(0.0) as u32,
                name: String::new(),
                format: AttribFormat::Gpu(get_str(cap, at, "format").unwrap_or("").to_string()),
                offset: get_num(cap, at, "offset").unwrap_or(0.0) as u64,
            })
            .collect();
        let bound = ps.vertex_buffers.get(&slot);
        if bound.is_none() {
            notes.push(format!("vertex buffer slot {slot} not set"));
        }
        layout.push(LayoutBuffer {
            slot,
            buffer: bound.map(|b| b.0),
            buffer_offset: bound.map_or(0, |b| b.1 as u64),
            stride: get_num(cap, b, "arrayStride").unwrap_or(0.0) as u64,
            per_instance,
            divisor: per_instance as u32,
            attributes,
        });
    }

    // Vertex buffer contents, cut at the bound range.
    let mut cache: HashMap<ObjId, Vec<u8>> = HashMap::new();
    let mut buffers = HashMap::new();
    for lb in &layout {
        let Some(id) = lb.buffer else { continue };
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(id) {
            match st.read_buffer(cap, id).await {
                Ok(b) => {
                    e.insert(b);
                }
                Err(e) => {
                    notes.push(format!("vertex buffer #{}: {e}", id.0));
                    continue;
                }
            }
        }
        let mut bytes = cache[&id].clone();
        if let Some(size) = ps.vertex_buffers.get(&lb.slot).and_then(|b| b.2) {
            bytes.truncate((lb.buffer_offset as f64 + size) as usize);
        }
        buffers.insert(lb.slot, bytes);
    }

    let instance_count = instance_count as u32;
    let instance = clamp_instance(instance, instance_count, &mut notes);
    let instance_index = first_instance as u32 + instance;
    let inputs = geo::fetch_vertices(&layout, &buffers, &vertex_ids, instance_index);
    if layout.iter().any(|l| l.per_instance) {
        notes.push(format!(
            "per-instance attributes shown for instance {instance}"
        ));
    }
    range_notes(&inputs, &mut notes);

    let post = match st.rt.device_of(cap, &st.objs, pipeline) {
        Some(dev) => {
            gpu_post_vs(
                st,
                cap,
                &dev,
                &ps,
                vertex,
                &vertex_ids,
                &inputs,
                instance_index,
            )
            .await
        }
        None => Err("post-VS unavailable: no replay device for the pipeline".into()),
    };
    let (outputs, position_output) = split_post_vs(post);

    Ok(GeometryData {
        index,
        topology,
        draw_order,
        restart,
        vertex_ids,
        instance,
        instance_count,
        layout,
        inputs,
        outputs,
        position_output,
        notes,
    })
}

fn encode_word(kind: vsxform::ScalarKind, v: f64) -> u32 {
    match kind {
        vsxform::ScalarKind::Float => (v as f32).to_bits(),
        vsxform::ScalarKind::Sint => (v as i32) as u32,
        vsxform::ScalarKind::Uint => v as u32,
    }
}

fn make_buffer(dev: &JsValue, size: usize, usage: u32, label: &str) -> Result<JsValue, String> {
    js::call(
        dev,
        "createBuffer",
        &[js::obj(&[
            ("size", js::num(size.max(16).next_multiple_of(4) as f64)),
            ("usage", js::num(usage as f64)),
            ("label", label.into()),
        ])],
    )
    .map_err(jserr)
}

fn write_words(queue: &JsValue, buf: &JsValue, words: &[u32]) -> Result<(), String> {
    let data = js_sys::Uint32Array::from(words);
    js::call(
        queue,
        "writeBuffer",
        &[buf.clone(), js::num(0.0), data.into()],
    )
    .map(|_| ())
    .map_err(jserr)
}

#[allow(clippy::too_many_arguments)]
async fn gpu_post_vs(
    st: &ReplayState,
    cap: &Capture,
    dev: &JsValue,
    ps: &PassState,
    vertex: &Value,
    vertex_ids: &[u32],
    inputs: &[AttribStream],
    instance_index: u32,
) -> PostVs {
    let module = cap
        .dict_get(vertex, "module")
        .and_then(Value::as_obj)
        .ok_or("post-VS unavailable: pipeline has no vertex module")?;
    let code = creation_desc(cap, module)
        .and_then(|d| get_str(cap, d, "code"))
        .ok_or_else(|| {
            format!(
                "post-VS unavailable: source of shader module #{} unknown",
                module.0
            )
        })?;
    let entry = get_str(cap, vertex, "entryPoint");
    let vc = vsxform::wgsl_vertex_to_compute(code, entry)
        .map_err(|e| format!("post-VS unavailable: {e}"))?;
    let n = vertex_ids.len();
    let m = vc.outputs.len();
    let stride = 16 * m;
    let offsets: Vec<usize> = (0..m).map(|k| 16 * k).collect();
    if n == 0 {
        return Ok(decode_outputs(&vc.outputs, &[], stride, &offsets, &[]));
    }

    // Inputs: one vec4<u32> of raw bits per (record, input slot).
    let k_in = vc.inputs.len();
    let mut in_words = vec![0u32; (n * k_in * 4).max(4)];
    let mut valid = vec![true; n];
    for (k, inp) in vc.inputs.iter().enumerate() {
        let s = inputs
            .iter()
            .find(|s| s.location == Some(inp.location))
            .ok_or_else(|| {
                format!(
                    "post-VS unavailable: no vertex attribute feeds @location({})",
                    inp.location
                )
            })?;
        for (r, v) in s.values.iter().enumerate().take(n) {
            match v {
                Some(v) => {
                    for (c, x) in v.iter().enumerate() {
                        in_words[(r * k_in + k) * 4 + c] = encode_word(inp.kind, *x);
                    }
                }
                None => valid[r] = false,
            }
        }
    }

    let limits = js::get(dev, "limits");
    let limit = |k: &str| js::as_f64(&js::get(&limits, k)).unwrap_or(f64::MAX);
    let out_size = n * stride.max(16);
    if (out_size.max(in_words.len() * 4)) as f64 > limit("maxStorageBufferBindingSize") {
        return Err(format!(
            "post-VS unavailable: {n} vertices exceed maxStorageBufferBindingSize"
        ));
    }
    if n.div_ceil(VS_WORKGROUP) as f64 > limit("maxComputeWorkgroupsPerDimension") {
        return Err(format!(
            "post-VS unavailable: {n} vertices exceed maxComputeWorkgroupsPerDimension"
        ));
    }

    let mut temps = Vec::new();
    let res = run_vs_compute(
        st,
        cap,
        dev,
        ps,
        vertex,
        &vc,
        &in_words,
        vertex_ids,
        instance_index,
        out_size,
        &mut temps,
    )
    .await;
    for b in &temps {
        let _ = js::call(b, "destroy", &[]);
    }
    let bytes = res?;
    Ok(decode_outputs(
        &vc.outputs,
        &bytes,
        stride,
        &offsets,
        &valid,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_vs_compute(
    st: &ReplayState,
    cap: &Capture,
    dev: &JsValue,
    ps: &PassState,
    vertex: &Value,
    vc: &VsCompute,
    in_words: &[u32],
    ids: &[u32],
    instance_index: u32,
    out_size: usize,
    temps: &mut Vec<JsValue>,
) -> Result<Vec<u8>, String> {
    let queue = js::get(dev, "queue");
    let storage_in = buf_usage::STORAGE | buf_usage::COPY_DST;
    let mut mk = |size: usize, usage: u32, label: &str| -> Result<JsValue, String> {
        let b = make_buffer(dev, size, usage, label)?;
        temps.push(b.clone());
        Ok(b)
    };
    let in_buf = mk(in_words.len() * 4, storage_in, "webgd:vs-in")?;
    let ids_buf = mk(ids.len() * 4, storage_in, "webgd:vs-ids")?;
    let params = mk(
        16,
        buf_usage::UNIFORM | buf_usage::COPY_DST,
        "webgd:vs-params",
    )?;
    let out_buf = mk(
        out_size,
        buf_usage::STORAGE | buf_usage::COPY_SRC,
        "webgd:vs-out",
    )?;
    let staging = mk(
        out_size,
        buf_usage::MAP_READ | buf_usage::COPY_DST,
        "webgd:vs-readback",
    )?;
    write_words(&queue, &in_buf, in_words)?;
    write_words(&queue, &ids_buf, ids)?;
    write_words(&queue, &params, &[ids.len() as u32, instance_index, 0, 0])?;

    const SCOPES: [&str; 3] = ["validation", "out-of-memory", "internal"];
    for f in SCOPES {
        let _ = js::call(dev, "pushErrorScope", &[f.into()]);
    }
    let built = (|| -> Result<(), String> {
        let module = js::call(
            dev,
            "createShaderModule",
            &[js::obj(&[
                ("code", vc.wgsl.as_str().into()),
                ("label", "webgd:vs-compute".into()),
            ])],
        )
        .map_err(jserr)?;
        let mut compute = vec![("module", module), ("entryPoint", vc.entry.as_str().into())];
        if let Some(c) = cap.dict_get(vertex, "constants") {
            compute.push((
                "constants",
                to_js(cap, c, &st.objs).map_err(|e| e.to_string())?,
            ));
        }
        let pipeline = js::call(
            dev,
            "createComputePipeline",
            &[js::obj(&[
                ("layout", "auto".into()),
                ("compute", js::obj(&compute)),
                ("label", "webgd:vs-compute".into()),
            ])],
        )
        .map_err(jserr)?;
        let bind_group = |g: u32, entries: JsValue| -> Result<JsValue, String> {
            let layout =
                js::call(&pipeline, "getBindGroupLayout", &[js::num(g as f64)]).map_err(jserr)?;
            js::call(
                dev,
                "createBindGroup",
                &[js::obj(&[("layout", layout), ("entries", entries)])],
            )
            .map_err(jserr)
        };
        let mut groups = Vec::new();
        for (g, bindings) in &vc.used_groups {
            let entries = group_entries(st, cap, ps, *g, bindings)?;
            groups.push((*g, bind_group(*g, entries)?));
        }
        let buf_entry = |binding: u32, buf: &JsValue| {
            js::obj(&[
                ("binding", js::num(binding as f64)),
                ("resource", js::obj(&[("buffer", buf.clone())])),
            ])
        };
        let io = js::arr(&[
            buf_entry(0, &in_buf),
            buf_entry(1, &ids_buf),
            buf_entry(2, &params),
            buf_entry(3, &out_buf),
        ]);
        groups.push((vc.io_group, bind_group(vc.io_group, io)?));

        let enc = js::call(dev, "createCommandEncoder", &[]).map_err(jserr)?;
        let pass = js::call(&enc, "beginComputePass", &[]).map_err(jserr)?;
        js::call(&pass, "setPipeline", std::slice::from_ref(&pipeline)).map_err(jserr)?;
        for (g, bg) in groups {
            js::call(&pass, "setBindGroup", &[js::num(g as f64), bg]).map_err(jserr)?;
        }
        let wgs = ids.len().div_ceil(VS_WORKGROUP);
        js::call(&pass, "dispatchWorkgroups", &[js::num(wgs as f64)]).map_err(jserr)?;
        js::call(&pass, "end", &[]).map_err(jserr)?;
        js::call(
            &enc,
            "copyBufferToBuffer",
            &[
                out_buf.clone(),
                js::num(0.0),
                staging.clone(),
                js::num(0.0),
                js::num(out_size as f64),
            ],
        )
        .map_err(jserr)?;
        let cb = js::call(&enc, "finish", &[]).map_err(jserr)?;
        js::call(&queue, "submit", &[js::arr(&[cb])]).map_err(jserr)?;
        Ok(())
    })();
    let mut scope_err = None;
    for _ in SCOPES {
        let p = js::call(dev, "popErrorScope", &[]).map_err(jserr)?;
        let e = await_value(&p).await?;
        if js::is_obj(&e) && scope_err.is_none() {
            scope_err = Some(js::get(&e, "message").as_string().unwrap_or_default());
        }
    }
    built.map_err(|e| format!("post-VS unavailable: {e}"))?;
    if let Some(e) = scope_err {
        return Err(format!("post-VS unavailable: {e}"));
    }
    map_read(&staging, out_size as u32).await
}

/// Binding → dynamic offset, for an explicit layout's `hasDynamicOffset` buffer entries
/// (offsets apply in increasing binding order).
fn dynamic_offsets(cap: &Capture, desc: &Value, offsets: &[u32]) -> HashMap<u32, u32> {
    let Some(layout) = cap.dict_get(desc, "layout").and_then(Value::as_obj) else {
        return HashMap::new();
    };
    let Some(ldesc) = creation_call(cap, layout)
        .filter(|c| cap.method(c) == "createBindGroupLayout")
        .and_then(|c| c.args.first())
    else {
        return HashMap::new();
    };
    let mut dynamic: Vec<u32> = cap
        .dict_get(ldesc, "entries")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter(|le| {
            cap.dict_get(le, "buffer")
                .and_then(|b| cap.dict_get(b, "hasDynamicOffset"))
                .and_then(Value::as_f64)
                == Some(1.0)
        })
        .filter_map(|le| get_num(cap, le, "binding").map(|b| b as u32))
        .collect();
    dynamic.sort_unstable();
    dynamic.into_iter().zip(offsets.iter().copied()).collect()
}

/// Entries of the bound bind group `g` restricted to `bindings`, with dynamic offsets folded
/// into the buffer offsets (the new bind group uses the compute pipeline's auto layout).
fn group_entries(
    st: &ReplayState,
    cap: &Capture,
    ps: &PassState,
    g: u32,
    bindings: &[u32],
) -> Result<JsValue, String> {
    let (bg, offsets) = ps
        .bind_groups
        .get(&g)
        .ok_or_else(|| format!("bind group {g} not set"))?;
    let desc = creation_desc(cap, *bg)
        .ok_or_else(|| format!("descriptor of bind group #{} unknown", bg.0))?;
    let dyn_offsets = dynamic_offsets(cap, desc, offsets);
    let buffer_res =
        |buf: ObjId, off: f64, size: Option<f64>, binding: u32| -> Result<JsValue, String> {
            let js_buf = st
                .objs
                .fwd
                .get(&buf)
                .ok_or_else(|| format!("buffer #{} not mapped in replay", buf.0))?;
            let size = size.unwrap_or_else(|| js::f(&js::get(js_buf, "size")) - off);
            let dynamic = dyn_offsets.get(&binding).copied().unwrap_or(0) as f64;
            Ok(js::obj(&[
                ("buffer", js_buf.clone()),
                ("offset", js::num(off + dynamic)),
                ("size", js::num(size)),
            ]))
        };
    let out = js_sys::Array::new();
    let mut found = Vec::new();
    for e in cap
        .dict_get(desc, "entries")
        .and_then(Value::as_array)
        .unwrap_or(&[])
    {
        let Some(binding) = get_num(cap, e, "binding").map(|b| b as u32) else {
            continue;
        };
        if !bindings.contains(&binding) {
            continue;
        }
        let res = cap.dict_get(e, "resource").unwrap_or(&Value::Undefined);
        let js_res = match res {
            Value::Dict(_) if cap.dict_get(res, "buffer").is_some() => {
                let buf = cap
                    .dict_get(res, "buffer")
                    .and_then(Value::as_obj)
                    .ok_or_else(|| format!("@group({g}) @binding({binding}): buffer missing"))?;
                buffer_res(
                    buf,
                    get_num(cap, res, "offset").unwrap_or(0.0),
                    get_num(cap, res, "size"),
                    binding,
                )?
            }
            Value::ObjRef(id) if cap.class_of(*id) == "GPUBuffer" => {
                buffer_res(*id, 0.0, None, binding)?
            }
            v => to_js(cap, v, &st.objs)
                .map_err(|e| format!("@group({g}) @binding({binding}): {e}"))?,
        };
        out.push(&js::obj(&[
            ("binding", js::num(binding as f64)),
            ("resource", js_res),
        ]));
        found.push(binding);
    }
    if let Some(missing) = bindings.iter().find(|b| !found.contains(b)) {
        return Err(format!(
            "@group({g}) @binding({missing}) not present in bind group #{}",
            bg.0
        ));
    }
    Ok(out.into())
}

// ---------------------------------------------------------------------------------------------
// WebGL2
// ---------------------------------------------------------------------------------------------

/// Active attribute of a program: (name, base location, type).
fn active_attribs(ctx: &GlCtx, prog: &JsValue) -> Vec<(String, u32, u32)> {
    if !js::is_obj(prog) {
        return Vec::new();
    }
    let count = js::u(
        &ctx.call("getProgramParameter", &[prog.clone(), n(ACTIVE_ATTRIBUTES)])
            .unwrap_or_default(),
    );
    let mut out = Vec::new();
    for i in 0..count {
        let info = ctx
            .call("getActiveAttrib", &[prog.clone(), n(i)])
            .unwrap_or_default();
        let Some(name) = js::get(&info, "name").as_string() else {
            continue;
        };
        if name.starts_with("gl_") {
            continue;
        }
        let loc = js::f(
            &ctx.call("getAttribLocation", &[prog.clone(), name.as_str().into()])
                .unwrap_or_default(),
        );
        if loc < 0.0 {
            continue;
        }
        out.push((name, loc as u32, js::u(&js::get(&info, "type"))));
    }
    out
}

/// (locations used, components per location, kind) of an attribute type.
fn attrib_shape(ty: u32) -> (u32, u32, ScalarKind) {
    use ScalarKind::*;
    match ty {
        0x1406 => (1, 1, Float),
        0x8B50 => (1, 2, Float),
        0x8B51 => (1, 3, Float),
        0x8B52 => (1, 4, Float),
        0x1404 => (1, 1, Sint),
        0x8B53 => (1, 2, Sint),
        0x8B54 => (1, 3, Sint),
        0x8B55 => (1, 4, Sint),
        0x1405 => (1, 1, Uint),
        0x8DC6 => (1, 2, Uint),
        0x8DC7 => (1, 3, Uint),
        0x8DC8 => (1, 4, Uint),
        0x8B5A => (2, 2, Float),
        0x8B5B => (3, 3, Float),
        0x8B5C => (4, 4, Float),
        0x8B65 => (2, 3, Float),
        0x8B66 => (2, 4, Float),
        0x8B67 => (3, 2, Float),
        0x8B68 => (3, 4, Float),
        0x8B69 => (4, 2, Float),
        0x8B6A => (4, 3, Float),
        _ => (1, 4, Float),
    }
}

/// Location → (display name, components, kind) for every location an active attribute occupies.
fn location_names(attribs: &[(String, u32, u32)]) -> HashMap<u32, (String, u32, ScalarKind)> {
    let mut map = HashMap::new();
    for (name, loc, ty) in attribs {
        let (cols, comps, kind) = attrib_shape(*ty);
        for c in 0..cols {
            let label = if cols > 1 {
                format!("{name}[{c}]")
            } else {
                name.clone()
            };
            map.insert(loc + c, (label, comps, kind));
        }
    }
    map
}

fn gl_geometry(
    st: &ReplayState,
    cap: &Capture,
    index: usize,
    call: &Call,
    instance: u32,
) -> Result<GeometryData, ReplayError> {
    let gi = st
        .rt
        .gl_index(cap, call.this)
        .ok_or_else(|| unsup("draw on an unknown GL context"))?;
    let ctx = &st.rt.gl[gi];
    let method = cap.method(call);
    let a = |i: usize| call.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    // (mode, Some((index type, byte offset)), first, count, instance count, base vertex, base instance)
    let (mode, elements, first, count, instance_count, base_vertex, base_instance) = match method {
        "drawArrays" => (a(0), None, a(1), a(2), 1.0, 0.0, 0.0),
        "drawArraysInstanced" => (a(0), None, a(1), a(2), a(3), 0.0, 0.0),
        "drawElements" => (a(0), Some((a(2), a(3))), 0.0, a(1), 1.0, 0.0, 0.0),
        "drawElementsInstanced" => (a(0), Some((a(2), a(3))), 0.0, a(1), a(4), 0.0, 0.0),
        "drawRangeElements" => (a(0), Some((a(4), a(5))), 0.0, a(3), 1.0, 0.0, 0.0),
        "drawArraysInstancedBaseInstanceWEBGL" => (a(0), None, a(1), a(2), a(3), 0.0, a(4)),
        "drawElementsInstancedBaseVertexBaseInstanceWEBGL" => {
            (a(0), Some((a(2), a(3))), 0.0, a(1), a(4), a(5), a(6))
        }
        m => return Err(unsup(format!("{m} is not supported by the geometry view"))),
    };
    if base_instance != 0.0 {
        return Err(unsup(
            "draws with a non-zero base instance are not supported by the geometry view",
        ));
    }
    let topology = Topology::from_gl(mode as u32)
        .ok_or_else(|| unsup(format!("draw mode {}", enum_name(mode as u32))))?;
    let count = count as u32;
    let mut notes = Vec::new();

    let (indices, restart) = match elements {
        Some((ty, off)) => {
            let format = match ty as u32 {
                UNSIGNED_BYTE => IndexFormat::U8,
                UNSIGNED_SHORT => IndexFormat::U16,
                UNSIGNED_INT => IndexFormat::U32,
                t => return Err(unsup(format!("index type {}", enum_name(t)))),
            };
            let eb = ctx.param(ELEMENT_ARRAY_BUFFER_BINDING);
            let id = st
                .objs
                .id_of(&eb)
                .ok_or_else(|| unsup("no element array buffer bound"))?;
            let bytes = gl_read_buffer(ctx, cap, id, &eb)
                .map_err(|message| ReplayError::Js { index, message })?;
            let ix = read_indices(&bytes, format, off as u64, count, &mut notes);
            // WebGL2 always enables PRIMITIVE_RESTART_FIXED_INDEX.
            (Some(ix), Some(restart_value(format)))
        }
        None => (None, None),
    };
    let (draw_order, vertex_ids) = draw_ids(
        indices.as_deref(),
        first as u32,
        count,
        base_vertex as i64,
        restart,
    );

    let prog = ctx.param(CURRENT_PROGRAM);
    let attribs = active_attribs(ctx, &prog);
    let names = location_names(&attribs);
    let va = |i: u32, p: u32| {
        ctx.call("getVertexAttrib", &[n(i), n(p)])
            .unwrap_or_default()
    };
    let max_attribs = js::u(&ctx.param(MAX_VERTEX_ATTRIBS));
    let mut layout = Vec::new();
    let mut generic = Vec::new();
    for i in 0..max_attribs {
        if !js::truthy(&va(i, VERTEX_ATTRIB_ARRAY_ENABLED)) {
            // Disabled but read by the program: constant generic value.
            if let Some((name, comps, kind)) = names.get(&i) {
                let cur = va(i, CURRENT_VERTEX_ATTRIB);
                let v = [0, 1, 2, 3].map(|k| js::f(&js::get_idx(&cur, k)));
                generic.push(AttribStream {
                    name: name.clone(),
                    location: Some(i),
                    slot: None,
                    format: "generic (constant)".into(),
                    components: *comps,
                    kind: *kind,
                    values: vec![Some(v); vertex_ids.len()],
                });
                notes.push(format!(
                    "{name} (location {i}) uses the constant generic attribute value"
                ));
            }
            continue;
        }
        let format = AttribFormat::Gl {
            size: js::u(&va(i, VERTEX_ATTRIB_ARRAY_SIZE)),
            ty: js::u(&va(i, VERTEX_ATTRIB_ARRAY_TYPE)),
            normalized: js::truthy(&va(i, VERTEX_ATTRIB_ARRAY_NORMALIZED)),
            integer: js::truthy(&va(i, VERTEX_ATTRIB_ARRAY_INTEGER)),
        };
        let stride = match js::u(&va(i, VERTEX_ATTRIB_ARRAY_STRIDE)) {
            0 => format.byte_size() as u64,
            s => s as u64,
        };
        let divisor = js::u(&va(i, VERTEX_ATTRIB_ARRAY_DIVISOR));
        let buf = va(i, VERTEX_ATTRIB_ARRAY_BUFFER_BINDING);
        let buffer = st.objs.id_of(&buf);
        if buffer.is_none() {
            notes.push(format!("attribute {i}: no buffer bound"));
        }
        let offset = js::f(
            &ctx.call(
                "getVertexAttribOffset",
                &[n(i), n(VERTEX_ATTRIB_ARRAY_POINTER)],
            )
            .unwrap_or_default(),
        ) as u64;
        layout.push(LayoutBuffer {
            slot: i,
            buffer,
            buffer_offset: offset,
            stride,
            per_instance: divisor > 0,
            divisor,
            attributes: vec![LayoutAttrib {
                location: i,
                name: names.get(&i).map(|e| e.0.clone()).unwrap_or_default(),
                format,
                offset: 0,
            }],
        });
    }

    let mut cache: HashMap<ObjId, Vec<u8>> = HashMap::new();
    let mut buffers = HashMap::new();
    for lb in &layout {
        let Some(id) = lb.buffer else { continue };
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(id) {
            let Some(obj) = st.objs.fwd.get(&id) else {
                notes.push(format!("vertex buffer #{} not mapped in replay", id.0));
                continue;
            };
            match gl_read_buffer(ctx, cap, id, obj) {
                Ok(b) => {
                    e.insert(b);
                }
                Err(e) => {
                    notes.push(format!("vertex buffer #{}: {e}", id.0));
                    continue;
                }
            }
        }
        buffers.insert(lb.slot, cache[&id].clone());
    }

    let instance_count = instance_count as u32;
    let instance = clamp_instance(instance, instance_count, &mut notes);
    let mut inputs = geo::fetch_vertices(&layout, &buffers, &vertex_ids, instance);
    inputs.extend(generic);
    inputs.sort_by_key(|s| s.location);
    if layout.iter().any(|l| l.per_instance) {
        notes.push(format!(
            "per-instance attributes shown for instance {instance}"
        ));
    }
    range_notes(&inputs, &mut notes);

    let valid: Vec<bool> = (0..vertex_ids.len())
        .map(|r| {
            inputs
                .iter()
                .filter(|s| s.location.is_some_and(|l| names.contains_key(&l)))
                .all(|s| s.values.get(r).is_some_and(Option::is_some))
        })
        .collect();
    let post = gl_post_vs(
        ctx,
        &prog,
        &attribs,
        &layout,
        &inputs,
        &vertex_ids,
        &valid,
        instance,
        &mut notes,
    );
    let (outputs, position_output) = split_post_vs(post);

    Ok(GeometryData {
        index,
        topology,
        draw_order,
        restart,
        vertex_ids,
        instance,
        instance_count,
        layout,
        inputs,
        outputs,
        position_output,
        notes,
    })
}

fn vertex_shader(ctx: &GlCtx, prog: &JsValue) -> Option<JsValue> {
    js::array_items(
        &ctx.call("getAttachedShaders", std::slice::from_ref(prog))
            .unwrap_or_default(),
    )
    .into_iter()
    .find(|s| {
        js::u(
            &ctx.call("getShaderParameter", &[s.clone(), n(SHADER_TYPE)])
                .unwrap_or_default(),
        ) == VERTEX_SHADER
    })
}

/// Links `vs` + `fs` capturing `varyings` (interleaved), with the original attribute locations.
fn link_tf(
    ctx: &GlCtx,
    vs: &JsValue,
    fs: &JsValue,
    attribs: &[(String, u32, u32)],
    varyings: &[&str],
) -> Result<JsValue, String> {
    let p = ctx
        .call("createProgram", &[])
        .map_err(|e| js::err_msg(&e))?;
    let _ = ctx.call("attachShader", &[p.clone(), vs.clone()]);
    let _ = ctx.call("attachShader", &[p.clone(), fs.clone()]);
    for (name, loc, _) in attribs {
        let _ = ctx.call(
            "bindAttribLocation",
            &[p.clone(), n(*loc), name.as_str().into()],
        );
    }
    let names: Vec<JsValue> = varyings.iter().map(|v| JsValue::from_str(v)).collect();
    let _ = ctx.call(
        "transformFeedbackVaryings",
        &[p.clone(), js::arr(&names), n(INTERLEAVED_ATTRIBS)],
    );
    let _ = ctx.call("linkProgram", std::slice::from_ref(&p));
    if js::truthy(
        &ctx.call("getProgramParameter", &[p.clone(), n(LINK_STATUS)])
            .unwrap_or_default(),
    ) {
        Ok(p)
    } else {
        let log = ctx
            .call("getProgramInfoLog", std::slice::from_ref(&p))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let _ = ctx.call("deleteProgram", &[p]);
        Err(log.trim().to_string())
    }
}

/// Copies plain uniform values (by name) and uniform block bindings from `from` to `to` (bound).
fn copy_uniforms(ctx: &GlCtx, from: &JsValue, to: &JsValue) {
    for (name, ty) in plain_uniforms(ctx, from) {
        let lo = ctx
            .call("getUniformLocation", &[from.clone(), name.as_str().into()])
            .unwrap_or(JsValue::NULL);
        let ln = ctx
            .call("getUniformLocation", &[to.clone(), name.as_str().into()])
            .unwrap_or(JsValue::NULL);
        if lo.is_null() || ln.is_null() {
            continue;
        }
        let v = ctx
            .call("getUniform", &[from.clone(), lo])
            .unwrap_or_default();
        let _ = set_uniform(&ctx.gl, ty, &ln, &v);
    }
    let blocks = js::u(
        &ctx.call(
            "getProgramParameter",
            &[to.clone(), n(ACTIVE_UNIFORM_BLOCKS)],
        )
        .unwrap_or_default(),
    );
    for i in 0..blocks {
        let name = ctx
            .call("getActiveUniformBlockName", &[to.clone(), n(i)])
            .unwrap_or_default();
        let oi = ctx
            .call("getUniformBlockIndex", &[from.clone(), name])
            .unwrap_or_default();
        if js::u(&oi) == u32::MAX {
            continue;
        }
        let b = ctx
            .call(
                "getActiveUniformBlockParameter",
                &[from.clone(), oi, n(UNIFORM_BLOCK_BINDING)],
            )
            .unwrap_or_default();
        let _ = ctx.call("uniformBlockBinding", &[to.clone(), n(i), b]);
    }
}

/// Contiguous (first, count) runs of sorted unique ids.
fn id_runs(ids: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &id in ids {
        match out.last_mut() {
            Some((f, c)) if *f as u64 + *c as u64 == id as u64 => *c += 1,
            _ => out.push((id, 1)),
        }
    }
    out
}

fn set_generic(ctx: &GlCtx, loc: u32, kind: ScalarKind, value: &JsValue) {
    let m = match kind {
        ScalarKind::Float => "vertexAttrib4fv",
        ScalarKind::Sint => "vertexAttribI4iv",
        ScalarKind::Uint => "vertexAttribI4uiv",
    };
    let _ = ctx.call(m, &[n(loc), value.clone()]);
}

/// Kind of a `getVertexAttrib(CURRENT_VERTEX_ATTRIB)` typed array.
fn current_kind(v: &JsValue) -> ScalarKind {
    match js::get(&js::get(v, "constructor"), "name")
        .as_string()
        .as_deref()
    {
        Some("Int32Array") => ScalarKind::Sint,
        Some("Uint32Array") => ScalarKind::Uint,
        _ => ScalarKind::Float,
    }
}

/// Post-VS capture through transform feedback. Every touched binding/state is restored.
#[allow(clippy::too_many_arguments)]
fn gl_post_vs(
    ctx: &GlCtx,
    prog: &JsValue,
    attribs: &[(String, u32, u32)],
    layout: &[LayoutBuffer],
    inputs: &[AttribStream],
    vertex_ids: &[u32],
    valid: &[bool],
    instance: u32,
    notes: &mut Vec<String>,
) -> PostVs {
    if !js::is_obj(prog) {
        return Err("post-VS unavailable: no program bound".into());
    }
    if js::truthy(&ctx.param(TRANSFORM_FEEDBACK_ACTIVE)) {
        return Err("post-VS unavailable: transform feedback is active at this draw".into());
    }
    let vs = vertex_shader(ctx, prog).ok_or("post-VS unavailable: program has no vertex shader")?;
    let src = ctx
        .call("getShaderSource", std::slice::from_ref(&vs))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let mut outs =
        vsxform::glsl_vertex_outputs(&src).map_err(|e| format!("post-VS unavailable: {e}"))?;

    // Per-instance values for instance > 0: either draw instance+1 instances and keep the last
    // one (needed when the shader reads gl_InstanceID), or feed per-instance attributes as
    // constant generic values of the selected instance.
    let instanced = instance > 0 && src.contains("gl_InstanceID");
    let draw_instances = if instanced { instance + 1 } else { 1 };
    let mut constants: Vec<(u32, ScalarKind, [f64; 4])> = Vec::new();
    if instance > 0 && !instanced {
        for lb in layout.iter().filter(|l| l.per_instance) {
            for at in &lb.attributes {
                let v = inputs
                    .iter()
                    .find(|s| s.location == Some(at.location))
                    .and_then(|s| s.values.iter().flatten().next().copied())
                    .ok_or_else(|| {
                        format!(
                            "post-VS unavailable: per-instance attribute {} out of buffer range for instance {instance}",
                            at.location
                        )
                    })?;
                constants.push((at.location, at.format.kind(), v));
            }
        }
    }

    let fs_src = if src.trim_start().starts_with("#version 300 es") {
        "#version 300 es\nprecision mediump float;\nvoid main(){}"
    } else {
        "precision mediump float;\nvoid main(){}"
    };
    let fs = ctx
        .compile(FRAGMENT_SHADER, fs_src)
        .map_err(|e| format!("post-VS unavailable: capture fragment shader: {e}"))?;
    let linked = {
        let names: Vec<&str> = outs.iter().map(|o| o.name.as_str()).collect();
        match link_tf(ctx, &vs, &fs, attribs, &names) {
            Ok(p) => Ok(p),
            Err(full) => {
                // Drop outputs the linker rejects individually (e.g. optimized-away varyings).
                let (kept, dropped): (Vec<VsOutput>, Vec<VsOutput>) =
                    outs.drain(..).partition(|o| {
                        match link_tf(ctx, &vs, &fs, attribs, &[o.name.as_str()]) {
                            Ok(p) => {
                                let _ = ctx.call("deleteProgram", &[p]);
                                true
                            }
                            Err(_) => false,
                        }
                    });
                outs = kept;
                if outs.is_empty() || dropped.is_empty() {
                    Err(format!(
                        "post-VS unavailable: capture program link failed: {full}"
                    ))
                } else {
                    let names: Vec<&str> = outs.iter().map(|o| o.name.as_str()).collect();
                    let r = link_tf(ctx, &vs, &fs, attribs, &names).map_err(|e| {
                        format!("post-VS unavailable: capture program link failed: {e}")
                    });
                    if r.is_ok() {
                        let list: Vec<&str> = dropped.iter().map(|o| o.name.as_str()).collect();
                        notes.push(format!(
                            "post-VS outputs not capturable: {}",
                            list.join(", ")
                        ));
                    }
                    r
                }
            }
        }
    };
    let _ = ctx.call("deleteShader", std::slice::from_ref(&fs));
    let cprog = linked?;

    let mut offsets = Vec::with_capacity(outs.len());
    let mut stride = 0usize;
    for o in &outs {
        offsets.push(stride);
        stride += o.components as usize * 4;
    }
    let total = vertex_ids.len();
    let size = draw_instances as f64 * total as f64 * stride as f64;
    let res = if total == 0 {
        Ok(Vec::new())
    } else if size > MAX_TF_BYTES {
        Err(format!(
            "post-VS unavailable: capture needs {size} bytes of transform feedback (limit {MAX_TF_BYTES})"
        ))
    } else {
        tf_capture(
            ctx,
            prog,
            &cprog,
            vertex_ids,
            draw_instances,
            stride,
            size as usize,
            &constants,
        )
    };
    let _ = ctx.call("deleteProgram", &[cprog]);
    let bytes = res?;
    Ok(decode_outputs(&outs, &bytes, stride, &offsets, valid))
}

/// Runs the capture draws and returns `vertex_ids.len()` records (of the last drawn instance).
#[allow(clippy::too_many_arguments)]
fn tf_capture(
    ctx: &GlCtx,
    prog: &JsValue,
    cprog: &JsValue,
    vertex_ids: &[u32],
    draw_instances: u32,
    stride: usize,
    size: usize,
    constants: &[(u32, ScalarKind, [f64; 4])],
) -> Result<Vec<u8>, String> {
    let saved = ctx.save(
        &[CURRENT_PROGRAM, DRAW_FRAMEBUFFER_BINDING],
        &[RASTERIZER_DISCARD],
    );
    let saved_tf = ctx.param(TRANSFORM_FEEDBACK_BINDING);
    let saved_tf_buf = ctx.param(TRANSFORM_FEEDBACK_BUFFER_BINDING);
    let saved_generic: Vec<(u32, JsValue)> = constants
        .iter()
        .map(|(loc, _, _)| {
            (
                *loc,
                ctx.call("getVertexAttrib", &[n(*loc), n(CURRENT_VERTEX_ATTRIB)])
                    .unwrap_or_default(),
            )
        })
        .collect();
    // Drain stale error flags so the check below reflects the capture only.
    for _ in 0..16 {
        if js::u(&ctx.call("getError", &[]).unwrap_or_default()) == NO_ERROR {
            break;
        }
    }

    // Default framebuffer: always complete, no feedback loops or output type mismatches.
    ctx.bind_fb(DRAW_FRAMEBUFFER, &JsValue::NULL);
    let _ = ctx.call("useProgram", std::slice::from_ref(cprog));
    copy_uniforms(ctx, prog, cprog);
    for (loc, kind, v) in constants {
        ctx.c("disableVertexAttribArray", &[*loc as f64]);
        let arr = js::arr(&v.map(js::num));
        set_generic(ctx, *loc, *kind, &arr);
    }
    let tf = ctx.call("createTransformFeedback", &[]).unwrap_or_default();
    let buf = ctx.call("createBuffer", &[]).unwrap_or_default();
    let _ = ctx.call(
        "bindTransformFeedback",
        &[n(TRANSFORM_FEEDBACK), tf.clone()],
    );
    let _ = ctx.call("bindBuffer", &[n(TRANSFORM_FEEDBACK_BUFFER), buf.clone()]);
    ctx.c(
        "bufferData",
        &[
            TRANSFORM_FEEDBACK_BUFFER as f64,
            size as f64,
            STREAM_READ as f64,
        ],
    );
    let _ = ctx.call(
        "bindBufferBase",
        &[n(TRANSFORM_FEEDBACK_BUFFER), n(0), buf.clone()],
    );
    ctx.c("enable", &[RASTERIZER_DISCARD as f64]);
    ctx.c("beginTransformFeedback", &[POINTS as f64]);
    let runs = id_runs(vertex_ids);
    for (first, count) in &runs {
        ctx.c(
            "drawArraysInstanced",
            &[
                POINTS as f64,
                *first as f64,
                *count as f64,
                draw_instances as f64,
            ],
        );
    }
    ctx.c("endTransformFeedback", &[]);
    let err = js::u(&ctx.call("getError", &[]).unwrap_or_default());
    let read = if err != NO_ERROR {
        Err(format!(
            "post-VS unavailable: transform feedback capture failed ({})",
            enum_name(err)
        ))
    } else {
        js::construct("Uint8Array", &[n(size as u32)])
            .and_then(|dst| {
                ctx.call(
                    "getBufferSubData",
                    &[n(TRANSFORM_FEEDBACK_BUFFER), n(0), dst.clone()],
                )
                .map(|_| dst)
            })
            .map(|dst| js::bytes_of(&dst))
            .map_err(|e| format!("post-VS unavailable: {}", js::err_msg(&e)))
    };

    // Restore.
    let _ = ctx.call(
        "bindBufferBase",
        &[n(TRANSFORM_FEEDBACK_BUFFER), n(0), JsValue::NULL],
    );
    let _ = ctx.call("bindTransformFeedback", &[n(TRANSFORM_FEEDBACK), saved_tf]);
    let _ = ctx.call("bindBuffer", &[n(TRANSFORM_FEEDBACK_BUFFER), saved_tf_buf]);
    let _ = ctx.call("deleteBuffer", &[buf]);
    let _ = ctx.call("deleteTransformFeedback", &[tf]);
    for (loc, cur) in saved_generic {
        ctx.c("enableVertexAttribArray", &[loc as f64]);
        set_generic(ctx, loc, current_kind(&cur), &cur);
    }
    ctx.restore(saved);

    // Keep the records of the last drawn instance of each run.
    let bytes = read?;
    let per_instance = draw_instances as usize - 1;
    let mut out = Vec::with_capacity(vertex_ids.len() * stride);
    let mut base = 0usize;
    for (_, count) in runs {
        let count = count as usize;
        let start = (base + per_instance * count) * stride;
        let chunk = bytes
            .get(start..start + count * stride)
            .ok_or("post-VS unavailable: transform feedback buffer shorter than expected")?;
        out.extend_from_slice(chunk);
        base += draw_instances as usize * count;
    }
    Ok(out)
}
