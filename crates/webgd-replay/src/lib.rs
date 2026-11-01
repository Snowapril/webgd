//! Replay engine (wasm only). Public API consumed by webgd-ui.
#![cfg(target_arch = "wasm32")]
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::JsValue;
use webgd_core::pixels::Image;
use webgd_core::{Capture, ObjId};

pub mod trace;
pub use trace::{CounterSample, CounterTrack, QueueTrack, Span, TraceInstr, TraceResult, run_trace};

#[derive(Debug, Clone)]
pub enum ReplayError {
    Unmapped(ObjId),
    Js { index: usize, message: String },
    Unsupported(String),
    Timeout(usize),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::Unmapped(id) => write!(f, "object #{} not mapped in replay", id.0),
            ReplayError::Js { index, message } => write!(f, "#{index}: {message}"),
            ReplayError::Unsupported(s) => write!(f, "unsupported: {s}"),
            ReplayError::Timeout(i) => write!(f, "#{i}: promise timed out"),
        }
    }
}

pub enum ReplayMode {
    Inspect,
    Trace(TraceInstr),
}

/// Live replay objects: capture id → JS object, plus reverse lookup.
pub struct ObjMap {
    pub fwd: HashMap<ObjId, JsValue>,
    pub rev: js_sys::Map,
}

pub struct Replayer {
    adapter: Option<JsValue>,
}

pub struct ReplayState {
    pub objs: ObjMap,
    /// (frame index, message); setup/snapshot errors use usize::MAX.
    pub errors: Vec<(usize, String)>,
    /// Per-resource notes (e.g. "pre-frame contents not captured (depth/stencil)").
    pub notes: HashMap<ObjId, Vec<String>>,
    /// Frame index replayed up to (inclusive).
    pub stop: usize,
}

pub struct Inspection {
    pub index: usize,
    pub targets: Vec<TargetView>,
    pub pipeline: Vec<StateNode>,
    pub bindings: Vec<BindingRow>,
    pub notes: Vec<String>,
}

pub struct TargetView {
    pub label: String,
    pub resource: Option<ObjId>,
    pub image: Option<Image>,
    pub note: Option<String>,
}

pub enum StateNode {
    Leaf { name: String, value: String },
    Group { name: String, children: Vec<StateNode> },
}

pub struct BindingRow {
    pub group: String,
    pub slot: String,
    pub resource: Option<ObjId>,
    pub detail: String,
}

pub struct ProgramInfo {
    pub link_status: bool,
    pub info_log: String,
    pub attributes: Vec<String>,
    pub uniforms: Vec<String>,
}

impl Replayer {
    pub async fn new() -> Replayer {
        todo!("replay agent")
    }
    pub fn has_webgpu(&self) -> bool {
        self.adapter.is_some()
    }
    pub async fn replay_to(&self, cap: &Rc<Capture>, stop: usize, mode: ReplayMode) -> Result<ReplayState, ReplayError> {
        let _ = (cap, stop, mode);
        todo!("replay agent")
    }
}

impl ReplayState {
    pub async fn inspect(&self, cap: &Capture, index: usize) -> Inspection {
        let _ = (cap, index);
        todo!("replay agent")
    }
    pub async fn read_buffer(&self, cap: &Capture, id: ObjId) -> Result<Vec<u8>, ReplayError> {
        let _ = (cap, id);
        todo!("replay agent")
    }
    /// `id` may be a texture, texture view (resolved to its texture) or GL renderbuffer.
    pub async fn read_texture(&self, cap: &Capture, id: ObjId, mip: u32, layer: u32) -> Result<Image, ReplayError> {
        let _ = (cap, id, mip, layer);
        todo!("replay agent")
    }
    /// getShaderInfoLog on the replayed WebGLShader.
    pub fn shader_info_log(&self, id: ObjId) -> Option<String> {
        let _ = id;
        todo!("replay agent")
    }
    pub fn program_info(&self, id: ObjId) -> Option<ProgramInfo> {
        let _ = id;
        todo!("replay agent")
    }
    pub fn dispose(self) {
        todo!("replay agent")
    }
}
