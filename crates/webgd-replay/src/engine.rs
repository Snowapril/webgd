//! Replay engine: setup → snapshot → frame[0..=stop] → WebGPU termination.
use crate::gl::{self, GlCtx};
use crate::js::{self, AwaitError};
use crate::trace::Instr;
use crate::value::to_js;
use crate::{gpu, ObjMap, ReplayError, ReplayMode, ReplayState, Runtime};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use webgd_core::{Call, Capture, ObjId, Section, Value};

pub const PROMISE_TIMEOUT_MS: f64 = 10_000.0;

pub type R = Result<(), String>;

pub fn jserr(e: JsValue) -> String {
    js::err_msg(&e)
}

pub struct Canvas {
    pub js: Option<JsValue>,
    pub w: u32,
    pub h: u32,
}

/// Replay-side stand-in for a page `GPUCanvasContext`.
pub struct GpuCanvasCtx {
    pub canvas: ObjId,
    pub cfg: Option<JsValue>,
    pub device: Option<JsValue>,
    pub tex: Option<JsValue>,
    /// A frame-section texture has been handed out (one per frame, like the browser).
    pub frame_tex: bool,
}

pub struct Engine {
    pub cap: Rc<Capture>,
    pub st: ReplayState,
    pub pending: HashMap<u32, JsValue>,
    pub mapped: HashMap<(ObjId, u64), JsValue>,
    pub open_passes: Vec<ObjId>,
    pub open_encoders: Vec<ObjId>,
    /// Open debug groups per frame encoder / pass (popped before termination).
    pub debug_depth: HashMap<ObjId, u32>,
    pub mapped_at_creation: Vec<ObjId>,
    pub canvases: HashMap<ObjId, Canvas>,
    pub last_gl: Option<usize>,
    pub pos: Rc<Cell<(usize, Section)>>,
    pub async_errors: Rc<RefCell<Vec<(usize, String)>>>,
    pub instr: Option<Instr>,
}

impl Engine {
    pub fn new(cap: &Rc<Capture>, mode: ReplayMode) -> Engine {
        let instr = match mode {
            ReplayMode::Inspect => None,
            ReplayMode::Trace(kind) => Some(Instr::new(cap, kind)),
        };
        Engine {
            cap: cap.clone(),
            st: ReplayState {
                objs: ObjMap { fwd: HashMap::new(), rev: js_sys::Map::new() },
                errors: Vec::new(),
                notes: HashMap::new(),
                stop: 0,
                rt: Runtime::default(),
            },
            pending: HashMap::new(),
            mapped: HashMap::new(),
            open_passes: Vec::new(),
            open_encoders: Vec::new(),
            debug_depth: HashMap::new(),
            mapped_at_creation: Vec::new(),
            canvases: HashMap::new(),
            last_gl: None,
            pos: Rc::new(Cell::new((usize::MAX, Section::Setup))),
            async_errors: Rc::new(RefCell::new(Vec::new())),
            instr,
        }
    }

    pub async fn run(mut self, stop: usize) -> ReplayState {
        let cap = self.cap.clone();
        // Navigator GPU objects and initial canvas sizes.
        let gpu_js = js::get(&js::get(&js::global(), "navigator"), "gpu");
        for (id, info) in &cap.objects {
            if cap.str(info.class) == "GPU" && js::is_obj(&gpu_js) {
                self.map(*id, gpu_js.clone());
            }
        }
        for call in &cap.snapshot {
            if cap.method(call) == "webgd:canvasSize" {
                let w = call.args.first().and_then(Value::as_f64).unwrap_or(300.0) as u32;
                let h = call.args.get(1).and_then(Value::as_f64).unwrap_or(150.0) as u32;
                self.canvases.insert(call.this, Canvas { js: None, w, h });
            }
        }

        for call in &cap.setup {
            self.exec(call, usize::MAX, Section::Setup).await;
        }
        self.check_gl_errors(usize::MAX, "setup: ");
        self.pos.set((usize::MAX, Section::Snapshot));
        self.apply_snapshot().await;
        self.check_gl_errors(usize::MAX, "snapshot: ");

        let stop = stop.min(cap.frame.len().saturating_sub(1));
        self.st.stop = stop;
        if !cap.frame.is_empty() {
            for (i, call) in cap.frame[..=stop].iter().enumerate() {
                self.exec(call, i, Section::Frame).await;
            }
        }
        self.pos.set((stop, Section::Frame));
        self.terminate().await;
        self.check_gl_errors(stop, "");
        if self.instr.is_some() {
            self.trace_collect().await;
        }
        // Let queued uncapturederror events fire.
        for dev in self.st.rt.devices.clone() {
            let p = js::call(&js::get(&dev, "queue"), "onSubmittedWorkDone", &[]).unwrap_or_default();
            let _ = js::await_promise(&p, 2000.0).await;
        }
        js::sleep(1.0).await;
        let async_errs = std::mem::take(&mut *self.async_errors.borrow_mut());
        self.st.errors.extend(async_errs);
        self.st.rt.trace = self.instr.take().map(|i| i.data);
        self.st
    }

    // ---- helpers -------------------------------------------------------------------------

    pub fn err(&mut self, msg: impl Into<String>) {
        let (idx, sec) = self.pos.get();
        let m = msg.into();
        let m = match sec {
            Section::Setup => format!("setup: {m}"),
            Section::Snapshot => format!("snapshot: {m}"),
            Section::Frame => m,
        };
        self.st.errors.push((idx, m));
    }

    pub fn map(&mut self, id: ObjId, v: JsValue) {
        if id.0 == 0 || !js::is_obj(&v) {
            return;
        }
        self.st.objs.rev.set(&v, &JsValue::from_f64(id.0 as f64));
        self.st.objs.fwd.insert(id, v);
    }

    pub fn obj(&self, id: ObjId) -> Result<JsValue, String> {
        self.st.objs.fwd.get(&id).cloned().ok_or_else(|| format!("{}#{} not mapped in replay", self.cap.class_of(id), id.0))
    }

    pub fn arg(&self, v: &Value) -> Result<JsValue, String> {
        to_js(&self.cap, v, &self.st.objs).map_err(|e| e.to_string())
    }

    pub fn args(&self, call: &Call) -> Result<Vec<JsValue>, String> {
        call.args.iter().map(|a| self.arg(a)).collect()
    }

    pub fn gl_index(&self, id: ObjId) -> Option<usize> {
        self.st.rt.gl_index(&self.cap, id)
    }

    pub fn device_of(&self, id: ObjId) -> Option<JsValue> {
        self.st.rt.device_of(&self.cap, &self.st.objs, id)
    }

    fn check_gl_errors(&mut self, index: usize, prefix: &str) {
        let mut found = Vec::new();
        for ctx in &self.st.rt.gl {
            for _ in 0..8 {
                let e = js::u(&ctx.call("getError", &[]).unwrap_or_default());
                if e == 0 {
                    break;
                }
                found.push(format!("{prefix}GL error {} on WebGL2RenderingContext#{}", gl::enum_name(e), ctx.id.0));
            }
        }
        for m in found {
            self.st.errors.push((index, m));
        }
    }

    // ---- dispatch -------------------------------------------------------------------------

    pub async fn exec(&mut self, call: &Call, index: usize, sec: Section) {
        self.pos.set((index, sec));
        let cap = self.cap.clone();
        let method = cap.str(call.method);
        let class = cap.class_of(call.this);
        if let Err(e) = self.dispatch(call, method, class, index, sec).await {
            self.err(format!("{class}#{}.{method}: {e}", call.this.0));
        }
    }

    async fn dispatch(&mut self, call: &Call, method: &str, class: &str, index: usize, sec: Section) -> R {
        if let Some(i) = self.gl_index(call.this) {
            self.last_gl = Some(i);
        }
        match method {
            "webgd:resolved" => {
                let token = call.args.first().and_then(Value::as_f64).unwrap_or(-1.0) as u32;
                if let Some(Value::ObjRef(id)) = call.args.get(1) {
                    let v = self.pending.get(&token).cloned().ok_or_else(|| format!("promise {token} has no replay value"))?;
                    self.map(*id, v);
                }
                return Ok(());
            }
            "webgd:pushMarker" | "webgd:popMarker" | "webgd:insertMarker" => return Ok(()),
            "webgd:canvasSize" => {
                let w = call.args.first().and_then(Value::as_f64).map(|v| v as u32);
                let h = call.args.get(1).and_then(Value::as_f64).map(|v| v as u32);
                self.resize_canvas(call.this, w, h);
                return Ok(());
            }
            "set:width" | "set:height" => {
                let v = call.args.first().and_then(Value::as_f64).map(|v| v as u32);
                if method == "set:width" {
                    self.resize_canvas(call.this, v, None);
                } else {
                    self.resize_canvas(call.this, None, v);
                }
                return Ok(());
            }
            "get:queue" => {
                let dev = self.obj(call.this)?;
                if let Value::ObjRef(id) = call.result {
                    self.map(id, js::get(&dev, "queue"));
                }
                return Ok(());
            }
            "set:label" => {
                let o = self.obj(call.this)?;
                let v = match call.args.first() {
                    Some(a) => self.arg(a)?,
                    None => JsValue::UNDEFINED,
                };
                js::set(&o, "label", &v);
                return Ok(());
            }
            "webgd:mappedWrite" => {
                let off = call.args.first().and_then(Value::as_f64).unwrap_or(0.0) as u64;
                let ab = self.mapped.get(&(call.this, off)).cloned().ok_or("no mapped range at offset")?;
                let data = self.arg(call.args.get(1).unwrap_or(&Value::Undefined))?;
                js::call(&js::construct("Uint8Array", &[ab]).map_err(jserr)?, "set", &[data]).map_err(jserr)?;
                return Ok(());
            }
            "webgd:bindForUpload" => {
                let i = self.gl_index(call.this).ok_or("not a WebGL2 context")?;
                let mut target = call.args.first().and_then(Value::as_f64).unwrap_or(0.0) as u32;
                if (gl::c::TEXTURE_CUBE_MAP_POSITIVE_X..gl::c::TEXTURE_CUBE_MAP_POSITIVE_X + 6).contains(&target) {
                    target = gl::c::TEXTURE_CUBE_MAP;
                }
                let tex = self.arg(call.args.get(1).unwrap_or(&Value::Null))?;
                let ctx = &self.st.rt.gl[i];
                ctx.c("activeTexture", &[(gl::c::TEXTURE0 + ctx.scratch_unit) as f64]);
                ctx.bind_tex(target, &tex);
                return Ok(());
            }
            "webgd:pixelStore" => {
                let i = self.gl_index(call.this).ok_or("not a WebGL2 context")?;
                if let Some(Value::Dict(entries)) = call.args.first() {
                    for (k, v) in entries {
                        let Ok(pname) = self.cap.str(*k).parse::<u32>() else { continue };
                        let v = self.arg(v)?;
                        let _ = self.st.rt.gl[i].call("pixelStorei", &[gl::n(pname), v]);
                    }
                }
                return Ok(());
            }
            "getContext" if matches!(class, "HTMLCanvasElement" | "OffscreenCanvas") => {
                return self.get_context(call).await;
            }
            _ => {}
        }

        if sec == Section::Frame {
            match method {
                "pushDebugGroup" => *self.debug_depth.entry(call.this).or_default() += 1,
                "popDebugGroup" => {
                    let d = self.debug_depth.entry(call.this).or_default();
                    *d = d.saturating_sub(1);
                }
                _ => {}
            }
        }
        if sec == Section::Frame && self.instr.is_some() && self.trace_gpu(call, method, class, index).await? {
            return Ok(());
        }

        match (class, method) {
            ("GPU", "requestAdapter") => {
                let gpu = self.obj(call.this)?;
                let args = self.args(call)?;
                let p = js::call(&gpu, "requestAdapter", &args).map_err(jserr)?;
                let a = await_value(&p).await?;
                if a.is_null() || a.is_undefined() {
                    return Err(ReplayError::Unsupported("WebGPU unavailable in webgd tab".into()).to_string());
                }
                if let Value::Pending(t) = call.result {
                    self.pending.insert(t, a);
                }
                Ok(())
            }
            ("GPUAdapter", "requestDevice") => self.request_device(call).await,
            ("GPUDevice", "createBuffer") | ("GPUDevice", "createTexture") => {
                let this = self.obj(call.this)?;
                let mut args = self.args(call)?;
                if let Some(desc) = args.first_mut() {
                    *desc = js::clone_obj(desc);
                    let usage = js::f(&js::get(desc, "usage")) as u32;
                    let usage = if method == "createBuffer" {
                        gpu::augment_buffer_usage(usage)
                    } else {
                        gpu::augment_texture_usage(usage, js::get(desc, "sampleCount").as_f64().unwrap_or(1.0) as u32)
                    };
                    js::set(desc, "usage", &js::num(usage as f64));
                    if method == "createBuffer" && js::truthy(&js::get(desc, "mappedAtCreation")) && sec == Section::Setup
                        && let Value::ObjRef(id) = call.result {
                            self.mapped_at_creation.push(id);
                        }
                }
                self.invoke(call, &this, method, args, index).await
            }
            ("GPUDevice", "importExternalTexture") => {
                let this = self.obj(call.this)?;
                let mut args = self.args(call)?;
                if let (Some(desc), Some(Value::Image { width, height, blob })) =
                    (args.first_mut(), call.args.first().and_then(|d| self.cap.dict_get(d, "source")))
                {
                    let data = js::u8_array(self.cap.blob(*blob));
                    let init = js::obj(&[
                        ("format", "RGBA".into()),
                        ("codedWidth", js::num(*width as f64)),
                        ("codedHeight", js::num(*height as f64)),
                        ("timestamp", js::num(0.0)),
                    ]);
                    let frame = js::construct("VideoFrame", &[data.into(), init]).map_err(jserr)?;
                    js::set(desc, "source", &frame);
                    // Closing the frame would expire the external texture; closed on dispose.
                    self.st.rt.video_frames.push(frame);
                }
                self.invoke(call, &this, method, args, index).await
            }
            ("GPUCanvasContext", _) => self.canvas_ctx_call(call, method, sec),
            ("GPUBuffer", "getMappedRange") => {
                let this = self.obj(call.this)?;
                let args = self.args(call)?;
                let r = js::call(&this, method, &args).map_err(jserr)?;
                let off = call.args.first().and_then(Value::as_f64).unwrap_or(0.0) as u64;
                self.mapped.insert((call.this, off), r);
                Ok(())
            }
            ("GPUBuffer", "unmap") => {
                self.mapped.retain(|(b, _), _| *b != call.this);
                let this = self.obj(call.this)?;
                self.invoke(call, &this, method, Vec::new(), index).await
            }
            ("GPUDevice", "createCommandEncoder") => {
                let this = self.obj(call.this)?;
                let args = self.args(call)?;
                self.invoke(call, &this, method, args, index).await?;
                if let (Value::ObjRef(id), Section::Frame) = (&call.result, sec) {
                    self.open_encoders.push(*id);
                }
                Ok(())
            }
            ("GPUCommandEncoder", "beginRenderPass" | "beginComputePass") => {
                let this = self.obj(call.this)?;
                let args = self.args(call)?;
                self.invoke(call, &this, method, args, index).await?;
                if let Value::ObjRef(id) = call.result {
                    self.open_passes.push(id);
                }
                Ok(())
            }
            ("GPURenderPassEncoder" | "GPUComputePassEncoder", "end") => {
                self.open_passes.retain(|p| *p != call.this);
                let this = self.obj(call.this)?;
                self.invoke(call, &this, method, Vec::new(), index).await
            }
            ("GPUCommandEncoder", "finish") => {
                self.open_encoders.retain(|e| *e != call.this);
                let this = self.obj(call.this)?;
                if self.instr.is_some() {
                    self.trace_before_finish(call.this, &this);
                }
                let args = self.args(call)?;
                self.invoke(call, &this, method, args, index).await
            }
            ("WebGLRenderingContext", _) => Err("WebGL1 contexts are unsupported".into()),
            _ => {
                let this = self.obj(call.this)?;
                let args = self.args(call)?;
                self.invoke(call, &this, method, args, index).await
            }
        }
    }

    /// Generic call + result mapping (+ GL trace wrapping in the frame section).
    async fn invoke(&mut self, call: &Call, this: &JsValue, method: &str, args: Vec<JsValue>, index: usize) -> R {
        let traced_gl = index != usize::MAX && self.instr.is_some() && self.trace_before_gl(call, index, this, method, &args);
        let r = js::call(this, method, &args);
        if traced_gl {
            self.trace_after_gl(call, index);
        }
        let r = r.map_err(jserr)?;
        match &call.result {
            Value::Pending(tok) => {
                let v = await_value(&r).await?;
                self.pending.insert(*tok, v);
            }
            Value::ObjRef(id) => {
                if js::is_obj(&r) {
                    self.map(*id, r);
                } else if !self.st.objs.fwd.contains_key(id) {
                    return Err(format!("returned {} (capture: {}#{})", js::err_msg(&r), self.cap.class_of(*id), id.0));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn request_device(&mut self, call: &Call) -> R {
        let adapter = self.obj(call.this)?;
        let mut args = self.args(call)?;
        if args.is_empty() {
            args.push(js::obj(&[]));
        }
        let desc = js::clone_obj(&args[0]);
        let feats = js::get(&adapter, "features");
        let has = |f: &JsValue| js::truthy(&js::call(&feats, "has", std::slice::from_ref(f)).unwrap_or_default());
        let mut out = Vec::new();
        for f in js::array_items(&js::get(&desc, "requiredFeatures")) {
            if has(&f) {
                out.push(f);
            } else {
                self.err(format!("missing feature {}", f.as_string().unwrap_or_default()));
            }
        }
        let ts: JsValue = "timestamp-query".into();
        if has(&ts) && !out.contains(&ts) {
            out.push(ts);
        }
        js::set(&desc, "requiredFeatures", &js::arr(&out));
        let req = js::get(&desc, "requiredLimits");
        if js::is_obj(&req) {
            let limits = js::get(&adapter, "limits");
            let clamped = js_sys::Object::new();
            for k in js_sys::Object::keys(req.unchecked_ref::<js_sys::Object>()).iter() {
                let key = k.as_string().unwrap_or_default();
                let (Some(v), Some(al)) = (js::get(&req, &key).as_f64(), js::get(&limits, &key).as_f64()) else { continue };
                let v = if key.starts_with("min") { v.max(al) } else { v.min(al) };
                js::set(&clamped, &key, &js::num(v));
            }
            js::set(&desc, "requiredLimits", &clamped.into());
        }
        let p = js::call(&adapter, "requestDevice", &[desc]).map_err(jserr)?;
        let dev = await_value(&p).await?;
        let pos = self.pos.clone();
        let sink = self.async_errors.clone();
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
            let (idx, sec) = pos.get();
            let msg = js::err_msg(&js::get(&ev, "error"));
            let msg = match sec {
                Section::Setup => format!("setup: {msg}"),
                Section::Snapshot => format!("snapshot: {msg}"),
                Section::Frame => msg,
            };
            sink.borrow_mut().push((idx, msg));
        });
        let _ = js::call(&dev, "addEventListener", &["uncapturederror".into(), cb.as_ref().clone()]);
        self.st.rt.listeners.push((dev.clone(), cb));
        self.st.rt.devices.push(dev.clone());
        if let Value::Pending(t) = call.result {
            self.pending.insert(t, dev);
        }
        Ok(())
    }

    async fn get_context(&mut self, call: &Call) -> R {
        let ty = match call.args.first() {
            Some(Value::String(s)) => self.cap.str(*s).to_string(),
            _ => return Err("bad context type".into()),
        };
        let Value::ObjRef(ctx_id) = call.result else { return Ok(()) };
        if self.st.objs.fwd.contains_key(&ctx_id) {
            return Ok(());
        }
        match ty.as_str() {
            "webgl2" => {
                let canvas = self.canvas_js(call.this)?;
                let attrs = match call.args.get(1) {
                    Some(a) => js::clone_obj(&self.arg(a)?),
                    None => js::obj(&[]),
                };
                js::set(&attrs, "preserveDrawingBuffer", &JsValue::TRUE);
                let gl = js::call(&canvas, "getContext", &["webgl2".into(), attrs]).map_err(jserr)?;
                if !js::is_obj(&gl) {
                    return Err("webgl2 context creation failed in webgd tab".into());
                }
                self.map(ctx_id, gl.clone());
                self.st.rt.gl.push(GlCtx::new(ctx_id, gl));
                Ok(())
            }
            "webgpu" => {
                self.map(ctx_id, js::obj(&[]));
                self.st.rt.gpu_ctxs.insert(
                    ctx_id,
                    GpuCanvasCtx { canvas: call.this, cfg: None, device: None, tex: None, frame_tex: false },
                );
                Ok(())
            }
            "webgl" | "experimental-webgl" => Err(ReplayError::Unsupported("WebGL1 context".into()).to_string()),
            _ => Ok(()),
        }
    }

    fn canvas_js(&mut self, id: ObjId) -> Result<JsValue, String> {
        let c = self.canvases.entry(id).or_insert(Canvas { js: None, w: 300, h: 150 });
        if let Some(js) = &c.js {
            return Ok(js.clone());
        }
        let js = js::construct("OffscreenCanvas", &[js::num(c.w as f64), js::num(c.h as f64)]).map_err(jserr)?;
        c.js = Some(js.clone());
        self.map(id, js.clone());
        Ok(js)
    }

    fn resize_canvas(&mut self, id: ObjId, w: Option<u32>, h: Option<u32>) {
        let c = self.canvases.entry(id).or_insert(Canvas { js: None, w: 300, h: 150 });
        let (nw, nh) = (w.unwrap_or(c.w), h.unwrap_or(c.h));
        if (nw, nh) == (c.w, c.h) {
            return;
        }
        c.w = nw;
        c.h = nh;
        if let Some(js) = &c.js {
            js::set(js, "width", &js::num(nw as f64));
            js::set(js, "height", &js::num(nh as f64));
        }
        for ctx in self.st.rt.gpu_ctxs.values_mut() {
            if ctx.canvas == id {
                ctx.tex = None;
            }
        }
    }

    fn canvas_ctx_call(&mut self, call: &Call, method: &str, sec: Section) -> R {
        let id = call.this;
        match method {
            "configure" => {
                let cfg = self.arg(call.args.first().unwrap_or(&Value::Undefined))?;
                let device = js::get(&cfg, "device");
                let ctx = self.st.rt.gpu_ctxs.get_mut(&id).ok_or("unknown canvas context")?;
                ctx.cfg = Some(cfg);
                ctx.device = Some(device);
                ctx.tex = None;
                Ok(())
            }
            "unconfigure" => {
                let ctx = self.st.rt.gpu_ctxs.get_mut(&id).ok_or("unknown canvas context")?;
                ctx.cfg = None;
                ctx.tex = None;
                Ok(())
            }
            "getConfiguration" => Ok(()),
            "getCurrentTexture" => {
                let (canvas, needs_new) = {
                    let ctx = self.st.rt.gpu_ctxs.get(&id).ok_or("unknown canvas context")?;
                    (ctx.canvas, ctx.tex.is_none() || (sec == Section::Frame && !ctx.frame_tex))
                };
                let (w, h) = self.canvases.get(&canvas).map(|c| (c.w, c.h)).unwrap_or((300, 150));
                let ctx = self.st.rt.gpu_ctxs.get_mut(&id).ok_or("unknown canvas context")?;
                if needs_new {
                    let cfg = ctx.cfg.clone().ok_or("getCurrentTexture before configure")?;
                    let dev = ctx.device.clone().ok_or("no device")?;
                    let usage = js::get(&cfg, "usage").as_f64().unwrap_or(0x10 as f64) as u32
                        | gpu::tex_usage::RENDER_ATTACHMENT
                        | gpu::tex_usage::COPY_SRC
                        | gpu::tex_usage::COPY_DST
                        | gpu::tex_usage::TEXTURE_BINDING;
                    let mut desc = vec![
                        ("size", js::arr(&[js::num(w as f64), js::num(h as f64)])),
                        ("format", js::get(&cfg, "format")),
                        ("usage", js::num(usage as f64)),
                        ("label", "webgd:canvas".into()),
                    ];
                    let vf = js::get(&cfg, "viewFormats");
                    if !vf.is_undefined() {
                        desc.push(("viewFormats", vf));
                    }
                    let tex = js::call(&dev, "createTexture", &[js::obj(&desc)]).map_err(jserr)?;
                    ctx.tex = Some(tex);
                    if sec == Section::Frame {
                        ctx.frame_tex = true;
                    }
                }
                let tex = ctx.tex.clone().unwrap_or_default();
                if let Value::ObjRef(rid) = call.result {
                    self.map(rid, tex);
                }
                Ok(())
            }
            _ => Err(format!("unsupported GPUCanvasContext method {method}")),
        }
    }

    // ---- snapshot -------------------------------------------------------------------------

    async fn apply_snapshot(&mut self) {
        let cap = self.cap.clone();
        for call in &cap.snapshot {
            if cap.method(call) == "webgd:canvasSize" {
                self.exec(call, usize::MAX, Section::Snapshot).await;
            }
        }
        // GL, per context.
        for i in 0..self.st.rt.gl.len() {
            let ctx_id = self.st.rt.gl[i].id;
            let calls: Vec<&Call> = cap
                .snapshot
                .iter()
                .filter(|c| cap.method(c).starts_with("webgd:gl") && c.this == ctx_id)
                .collect();
            let mut errs = Vec::new();
            gl::snapshot::apply(
                &self.st.rt.gl[i],
                &cap,
                &calls,
                &self.st.objs,
                &mut self.st.notes,
                &mut errs,
            );
            for e in errs {
                self.err(e);
            }
        }
        // GPU.
        let snapshot_mapped: HashSet<ObjId> = cap
            .snapshot
            .iter()
            .filter(|c| {
                cap.method(c) == "webgd:gpuBuffer"
                    && matches!(c.args.get(1), Some(Value::String(s)) if cap.str(*s) == "mapped")
            })
            .map(|c| c.this)
            .collect();
        for call in &cap.snapshot {
            let m = cap.method(call);
            if m == "webgd:gpuBuffer" || m == "webgd:gpuTexture" {
                if let Err(e) = gpu::snapshot::apply(self, call, m) {
                    self.err(format!("{}#{}: {e}", cap.class_of(call.this), call.this.0));
                }
            } else if !m.starts_with("webgd:gl") && m != "webgd:canvasSize" {
                let ctx_known = self.st.objs.fwd.contains_key(&call.this);
                if ctx_known {
                    self.err(format!("unknown snapshot record {m}"));
                }
            }
        }
        // Buffers left mapped from setup mappedAtCreation (the page had unmapped them).
        for id in std::mem::take(&mut self.mapped_at_creation) {
            if snapshot_mapped.contains(&id) {
                continue;
            }
            if let Some(b) = self.st.objs.fwd.get(&id)
                && js::get(b, "mapState").as_string().as_deref() == Some("mapped")
            {
                let _ = js::call(b, "unmap", &[]);
            }
        }
        self.mapped.clear();
    }

    // ---- termination ----------------------------------------------------------------------

    fn pop_debug_groups(&mut self, id: ObjId, obj: &JsValue) {
        for _ in 0..self.debug_depth.remove(&id).unwrap_or(0) {
            let _ = js::call(obj, "popDebugGroup", &[]);
        }
    }

    async fn terminate(&mut self) {
        for pass in std::mem::take(&mut self.open_passes) {
            let Some(p) = self.st.objs.fwd.get(&pass).cloned() else { continue };
            self.pop_debug_groups(pass, &p);
            if let Err(e) = js::call(&p, "end", &[]) {
                self.err(format!("terminate pass: {}", jserr(e)));
            }
        }
        let mut per_device: Vec<(JsValue, Vec<JsValue>)> = Vec::new();
        for enc in std::mem::take(&mut self.open_encoders) {
            let Some(e) = self.st.objs.fwd.get(&enc).cloned() else { continue };
            self.pop_debug_groups(enc, &e);
            if self.instr.is_some() {
                self.trace_before_finish(enc, &e);
            }
            match js::call(&e, "finish", &[]) {
                Ok(cb) => {
                    let Some(dev) = self.device_of(enc) else { continue };
                    match per_device.iter_mut().find(|(d, _)| *d == dev) {
                        Some((_, v)) => v.push(cb),
                        None => per_device.push((dev, vec![cb])),
                    }
                }
                Err(err) => self.err(format!("terminate encoder: {}", jserr(err))),
            }
        }
        for (dev, cbs) in per_device {
            if let Err(e) = js::call(&js::get(&dev, "queue"), "submit", &[js::arr(&cbs)]) {
                self.err(format!("terminate submit: {}", jserr(e)));
            }
        }
    }
}

pub async fn await_value(p: &JsValue) -> Result<JsValue, String> {
    match js::await_promise(p, PROMISE_TIMEOUT_MS).await {
        Ok(v) => Ok(v),
        Err(AwaitError::Timeout) => Err("promise timed out (10 s)".into()),
        Err(AwaitError::Rejected(e)) => Err(format!("promise rejected: {}", jserr(e))),
    }
}
