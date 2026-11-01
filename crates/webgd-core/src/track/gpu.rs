//! WebGPU pass-state tracker.
use crate::capture::{Capture, ObjId, Value};
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

/// State of the tracked pass at `frame[index]` (inclusive).
pub fn state_at(cap: &Capture, index: usize) -> GpuState {
    let _ = (cap, index);
    todo!("core agent")
}

/// Descriptor (first arg) of the call that created `id` (searches setup then frame), if any.
pub fn creation_call(cap: &Capture, id: ObjId) -> Option<(crate::capture::Section, usize)> {
    let _ = (cap, id);
    todo!("core agent")
}
