//! Replay engine (wasm only). Public API consumed by webgd-ui.
#![cfg(target_arch = "wasm32")]
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use webgd_core::pixels::Image;
use webgd_core::{Capture, ObjId};

mod engine;
mod gl;
mod gpu;
mod js;
pub mod trace;
mod util;
mod value;

pub use gl::enum_name as gl_enum_name;
pub use trace::{
    CounterSample, CounterTrack, QueueTrack, Span, TraceInstr, TraceResult, run_trace,
};

use engine::GpuCanvasCtx;
use gl::GlCtx;

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

impl ObjMap {
    /// Capture id of a live replay object.
    pub fn id_of(&self, v: &JsValue) -> Option<ObjId> {
        if !js::is_obj(v) {
            return None;
        }
        self.rev.get(v).as_f64().map(|n| ObjId(n as u32))
    }
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
    /// Frame index replayed up to (inclusive). For a frame-start state
    /// (`frame_start == true`, from `Replayer::replay_to_frame_start`) no frame
    /// call ran and `stop` is 0 by convention.
    pub stop: usize,
    /// True when the state reflects contents before `frame[0]` (setup + snapshot only).
    pub frame_start: bool,
    pub(crate) rt: Runtime,
}

type ErrorListener = Closure<dyn FnMut(JsValue)>;

/// Live replay contexts/devices owned by a `ReplayState`.
#[derive(Default)]
pub(crate) struct Runtime {
    pub cap: Rc<Capture>,
    pub gl: Vec<GlCtx>,
    pub devices: Vec<JsValue>,
    pub gpu_ctxs: HashMap<ObjId, GpuCanvasCtx>,
    pub listeners: Vec<(JsValue, ErrorListener)>,
    pub video_frames: Vec<JsValue>,
    pub trace: Option<trace::TraceData>,
}

impl Runtime {
    pub fn gl_index(&self, cap: &Capture, id: ObjId) -> Option<usize> {
        let ctx = util::ancestor(cap, id, |c| c == "WebGL2RenderingContext")?;
        self.gl.iter().position(|g| g.id == ctx)
    }

    pub fn device_of(&self, cap: &Capture, objs: &ObjMap, id: ObjId) -> Option<JsValue> {
        let a = util::ancestor(cap, id, |c| c == "GPUDevice" || c == "GPUCanvasContext")?;
        if cap.class_of(a) == "GPUCanvasContext" {
            self.gpu_ctxs.get(&a).and_then(|c| c.device.clone())
        } else {
            objs.fwd.get(&a).cloned()
        }
    }
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
    Leaf {
        name: String,
        value: String,
    },
    Group {
        name: String,
        children: Vec<StateNode>,
    },
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
        let gpu = js::get(&js::get(&js::global(), "navigator"), "gpu");
        let mut adapter = None;
        if js::is_obj(&gpu)
            && let Ok(p) = js::call(&gpu, "requestAdapter", &[])
            && let Ok(a) = js::await_promise(&p, engine::PROMISE_TIMEOUT_MS).await
            && js::is_obj(&a)
        {
            adapter = Some(a);
        }
        Replayer { adapter }
    }
    pub fn has_webgpu(&self) -> bool {
        self.adapter.is_some()
    }
    pub async fn replay_to(
        &self,
        cap: &Rc<Capture>,
        stop: usize,
        mode: ReplayMode,
    ) -> Result<ReplayState, ReplayError> {
        let mut eng = engine::Engine::new(cap, mode);
        eng.st.rt.cap = cap.clone();
        Ok(eng.run(Some(stop)).await)
    }

    /// Replays setup + snapshot and WebGPU termination without any frame call,
    /// i.e. resource contents at frame start. The returned state has
    /// `frame_start = true` and `stop = 0`; errors use `usize::MAX` like setup/snapshot.
    pub async fn replay_to_frame_start(
        &self,
        cap: &Rc<Capture>,
    ) -> Result<ReplayState, ReplayError> {
        let mut eng = engine::Engine::new(cap, ReplayMode::Inspect);
        eng.st.rt.cap = cap.clone();
        Ok(eng.run(None).await)
    }
}

impl ReplayState {
    /// GL context that `frame[index]` addresses, or the most recent one used before it.
    fn gl_for_index(&self, cap: &Capture, index: usize) -> Option<usize> {
        let upto = index.min(cap.frame.len().saturating_sub(1));
        for i in (0..=upto).rev() {
            let Some(c) = cap.frame.get(i) else { break };
            if cap.class_of(c.this).starts_with("GPU") && i == upto {
                return None;
            }
            if let Some(g) = self.rt.gl_index(cap, c.this) {
                return Some(g);
            }
        }
        if self.rt.gl.len() == 1 && self.rt.devices.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    pub async fn inspect(&self, cap: &Capture, index: usize) -> Inspection {
        if self.frame_start {
            return Inspection {
                index,
                targets: Vec::new(),
                pipeline: Vec::new(),
                bindings: Vec::new(),
                notes: vec!["frame start: no command selected".into()],
            };
        }
        match self.gl_for_index(cap, index) {
            Some(g) => gl::inspect::inspect(self, cap, index, &self.rt.gl[g]),
            None => gpu::inspect::inspect(self, cap, index).await,
        }
    }

    pub async fn read_buffer(&self, cap: &Capture, id: ObjId) -> Result<Vec<u8>, ReplayError> {
        let obj = self.objs.fwd.get(&id).ok_or(ReplayError::Unmapped(id))?;
        match cap.class_of(id) {
            "WebGLBuffer" => {
                let g = self
                    .rt
                    .gl_index(cap, id)
                    .ok_or_else(|| ReplayError::Unsupported("no GL context".into()))?;
                gl::inspect::read_buffer(&self.rt.gl[g], cap, id, obj).map_err(|m| {
                    ReplayError::Js {
                        index: self.stop,
                        message: m,
                    }
                })
            }
            "GPUBuffer" => {
                let dev = self
                    .rt
                    .device_of(cap, &self.objs, id)
                    .ok_or_else(|| ReplayError::Unsupported("no device".into()))?;
                gpu::read_buffer(&dev, obj)
                    .await
                    .map_err(|m| ReplayError::Js {
                        index: self.stop,
                        message: m,
                    })
            }
            c => Err(ReplayError::Unsupported(format!("{c} is not a buffer"))),
        }
    }

    /// `id` may be a texture, texture view (resolved to its texture) or GL renderbuffer.
    pub async fn read_texture(
        &self,
        cap: &Capture,
        id: ObjId,
        mip: u32,
        layer: u32,
    ) -> Result<Image, ReplayError> {
        let js_err = |m: String| ReplayError::Js {
            index: self.stop,
            message: m,
        };
        match cap.class_of(id) {
            "GPUTextureView" => {
                let tex = cap
                    .objects
                    .get(&id)
                    .and_then(|o| o.parent)
                    .ok_or(ReplayError::Unmapped(id))?;
                Box::pin(self.read_texture(cap, tex, mip, layer)).await
            }
            "GPUTexture" => {
                let obj = self.objs.fwd.get(&id).ok_or(ReplayError::Unmapped(id))?;
                let dev = self
                    .rt
                    .device_of(cap, &self.objs, id)
                    .ok_or_else(|| ReplayError::Unsupported("no device".into()))?;
                gpu::read_texture(&dev, obj, mip, layer)
                    .await
                    .map_err(js_err)
            }
            "WebGLTexture" | "WebGLRenderbuffer" => {
                let obj = self.objs.fwd.get(&id).ok_or(ReplayError::Unmapped(id))?;
                let g = self
                    .rt
                    .gl_index(cap, id)
                    .ok_or_else(|| ReplayError::Unsupported("no GL context".into()))?;
                gl::inspect::read_image(&self.rt.gl[g], cap, self.stop, id, obj, mip, layer)
                    .map_err(js_err)
            }
            c => Err(ReplayError::Unsupported(format!("{c} is not a texture"))),
        }
    }

    /// getShaderInfoLog on the replayed WebGLShader.
    pub fn shader_info_log(&self, id: ObjId) -> Option<String> {
        let obj = self.objs.fwd.get(&id)?;
        let g = self.rt.gl_index(&self.rt.cap, id)?;
        self.rt.gl[g]
            .call("getShaderInfoLog", std::slice::from_ref(obj))
            .ok()?
            .as_string()
    }

    pub fn program_info(&self, id: ObjId) -> Option<ProgramInfo> {
        let obj = self.objs.fwd.get(&id)?;
        let g = self.rt.gl_index(&self.rt.cap, id)?;
        Some(gl::inspect::program_info(&self.rt.gl[g], obj))
    }

    pub fn dispose(self) {
        for (target, cb) in &self.rt.listeners {
            let _ = js::call(
                target,
                "removeEventListener",
                &["uncapturederror".into(), cb.as_ref().clone()],
            );
        }
        for dev in &self.rt.devices {
            let _ = js::call(dev, "destroy", &[]);
        }
        for ctx in &self.rt.gl {
            ctx.lose();
        }
        for f in &self.rt.video_frames {
            let _ = js::call(f, "close", &[]);
        }
    }
}
