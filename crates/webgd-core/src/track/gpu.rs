//! WebGPU pass-state tracker.
use crate::capture::{Capture, ObjId, Section, TypedKind, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct GpuState {
    pub pass: Option<PassState>,
}

#[derive(Clone, Debug)]
pub struct PassState {
    pub encoder: ObjId,
    pub pass: ObjId,
    pub compute: bool,
    /// beginRenderPass / beginComputePass descriptor (Undefined if none).
    pub desc: Value,
    pub pipeline: Option<ObjId>,
    pub bind_groups: BTreeMap<u32, (ObjId, Vec<u32>)>,
    /// slot → (buffer, offset, size)
    pub vertex_buffers: BTreeMap<u32, (ObjId, f64, Option<f64>)>,
    /// (buffer, format, offset, size)
    pub index_buffer: Option<(ObjId, String, f64, Option<f64>)>,
    pub viewport: Option<[f64; 6]>,
    pub scissor: Option<[f64; 4]>,
    pub blend_constant: Option<Value>,
    pub stencil_ref: Option<f64>,
}

fn is_pass_class(class: &str) -> bool {
    matches!(class, "GPURenderPassEncoder" | "GPUComputePassEncoder")
}

/// State of the tracked pass at `frame[index]` (inclusive).
///
/// The tracked pass is the pass encoder that is `this` of `frame[index]`; otherwise the most
/// recently begun pass not yet ended at `index`; otherwise none.
pub fn state_at(cap: &Capture, index: usize) -> GpuState {
    let Some(cur) = cap.frame.get(index) else { return GpuState::default() };
    let pass_id = if is_pass_class(cap.class_of(cur.this)) {
        Some(cur.this)
    } else {
        // Most recent begin whose pass has no `end` in (begin, index].
        let mut open: Vec<ObjId> = Vec::new();
        for call in &cap.frame[..=index] {
            match cap.method(call) {
                "beginRenderPass" | "beginComputePass" => open.extend(call.result.as_obj()),
                "end" => open.retain(|&p| p != call.this),
                _ => {}
            }
        }
        open.last().copied()
    };
    let Some(pass) = pass_id else { return GpuState::default() };

    let begin = cap.frame[..=index].iter().rposition(|c| {
        matches!(cap.method(c), "beginRenderPass" | "beginComputePass")
            && c.result.as_obj() == Some(pass)
    });
    let (encoder, desc, start) = match begin {
        Some(b) => {
            let c = &cap.frame[b];
            (c.this, c.args.first().cloned().unwrap_or(Value::Undefined), b + 1)
        }
        None => (
            cap.objects.get(&pass).and_then(|o| o.parent).unwrap_or(ObjId(0)),
            Value::Undefined,
            0,
        ),
    };
    let mut st = PassState {
        encoder,
        pass,
        compute: cap.class_of(pass) == "GPUComputePassEncoder",
        desc,
        pipeline: None,
        bind_groups: BTreeMap::new(),
        vertex_buffers: BTreeMap::new(),
        index_buffer: None,
        viewport: None,
        scissor: None,
        blend_constant: None,
        stencil_ref: None,
    };
    let num = |v: Option<&Value>| v.and_then(Value::as_f64);
    for call in cap.frame[start..=index].iter().filter(|c| c.this == pass) {
        let a = &call.args;
        match cap.method(call) {
            "setPipeline" => st.pipeline = a.first().and_then(Value::as_obj),
            "setBindGroup" => {
                let Some(slot) = num(a.first()) else { continue };
                match a.get(1).and_then(Value::as_obj) {
                    Some(bg) => {
                        let offsets = dynamic_offsets(cap, &a[2.min(a.len())..]);
                        st.bind_groups.insert(slot as u32, (bg, offsets));
                    }
                    None => {
                        st.bind_groups.remove(&(slot as u32));
                    }
                }
            }
            "setVertexBuffer" => {
                let Some(slot) = num(a.first()) else { continue };
                match a.get(1).and_then(Value::as_obj) {
                    Some(buf) => {
                        let entry = (buf, num(a.get(2)).unwrap_or(0.0), num(a.get(3)));
                        st.vertex_buffers.insert(slot as u32, entry);
                    }
                    None => {
                        st.vertex_buffers.remove(&(slot as u32));
                    }
                }
            }
            "setIndexBuffer" => {
                st.index_buffer = a.first().and_then(Value::as_obj).map(|buf| {
                    let format = match a.get(1) {
                        Some(Value::String(s)) => cap.str(*s).to_owned(),
                        _ => String::new(),
                    };
                    (buf, format, num(a.get(2)).unwrap_or(0.0), num(a.get(3)))
                });
            }
            "setViewport" => {
                let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                if let Ok(v) = <[f64; 6]>::try_from(v) {
                    st.viewport = Some(v);
                }
            }
            "setScissorRect" => {
                let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                if let Ok(v) = <[f64; 4]>::try_from(v) {
                    st.scissor = Some(v);
                }
            }
            "setBlendConstant" => st.blend_constant = a.first().cloned(),
            "setStencilReference" => st.stencil_ref = num(a.first()),
            // WebGPU resets these after executing bundles.
            "executeBundles" => {
                st.pipeline = None;
                st.bind_groups.clear();
                st.vertex_buffers.clear();
                st.index_buffer = None;
            }
            _ => {}
        }
    }
    GpuState { pass: Some(st) }
}

/// `setBindGroup` trailing args: `[offsets: Array]` or `[Uint32Array, start, length]`.
fn dynamic_offsets(cap: &Capture, rest: &[Value]) -> Vec<u32> {
    match rest.first() {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_f64).map(|n| n as u32).collect(),
        Some(Value::Typed { kind: TypedKind::Uint32, blob }) => {
            let all: Vec<u32> = cap
                .blob(*blob)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect();
            let start = rest.get(1).and_then(Value::as_f64).map_or(0, |n| n as usize).min(all.len());
            let len = rest.get(2).and_then(Value::as_f64).map_or(all.len() - start, |n| n as usize);
            all[start..(start + len).min(all.len())].to_vec()
        }
        _ => Vec::new(),
    }
}

/// The call that created `id` (its result is `ObjRef(id)`): searches setup then frame.
pub fn creation_call(cap: &Capture, id: ObjId) -> Option<(Section, usize)> {
    let target = Value::ObjRef(id);
    for section in [Section::Setup, Section::Frame] {
        if let Some(i) = cap.section(section).iter().position(|c| c.result == target) {
            return Some((section, i));
        }
    }
    None
}
