//! GPU trace runner: timestamp/timer-query instrumented replays + estimated counters.
//!
//! WebGPU: render/compute passes are split into one segment pass per draw/dispatch, each with
//! `timestampWrites` (2 queries per slot; slot `s` → set `s / 2048`, queries `2(s % 2048)`,
//! `+1`, resolved to offset `(s % 2048) · 256`) and, for draws, an occlusion query (`k` → set
//! `k / 4096`, resolved to `(k % 4096) · 256`). Passes using occlusion queries, timestamps or
//! render bundles run whole with pass-level timestamps.
//! WebGL: each item is wrapped in a TIME_ELAPSED_EXT query and laid out sequentially; a separate
//! untimed run counts rasterized fragments per draw.
use crate::engine::{Engine, R, jserr};
use crate::gl::c::*;
use crate::gl::inspect::plain_uniforms;
use crate::gl::uniforms::set_uniform;
use crate::gl::{self, n};
use crate::gpu::{self, buf_usage};
use crate::util::{ancestor, creation_desc, get_str, num_list};
use crate::{ReplayError, ReplayMode, Replayer, js};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::JsValue;
use webgd_analysis::{Stage, StageStats, glsl_stats, occupancy, wgsl_stats};
use webgd_core::display::{call_label, obj_label};
use webgd_core::track::commands::{CmdKind, build_tree};
use webgd_core::track::gl::texture_dims;
use webgd_core::{Call, Capture, ObjId, Value};

/// Instrumented replays per trace; the run with the median total GPU time is kept.
pub const TRACE_RUNS: usize = 5;
const GL_DISJOINT_RETRIES: usize = 3;
const SLOTS_PER_SET: u32 = 2048;
const OCC_PER_SET: u32 = 4096;
const RESOLVE_STRIDE: u32 = 256;
const GL_POLL_MS: f64 = 16.0;
const GL_POLL_TIMEOUT_MS: f64 = 5000.0;
const NOMINAL_ITEM_NS: u64 = 1000;
pub const CHROME_TIMESTAMP_NOTE: &str = "Chrome quantizes WebGPU timestamps unless chrome://flags/#enable-webgpu-developer-features is enabled";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TraceInstr {
    /// Timed run (WebGPU timestamp + occlusion queries, GL timer queries).
    Timed,
    /// Untimed GL run counting rasterized fragments per draw.
    GlFragmentCount,
}

pub struct TraceResult {
    pub queues: Vec<QueueTrack>,
    pub counters: Vec<CounterTrack>,
    pub notes: Vec<String>,
    pub total_ns: u64,
}

pub struct QueueTrack {
    pub name: String,
    pub passes: Vec<Span>,
    pub markers: Vec<Span>,
    pub items: Vec<Span>,
}

#[derive(Clone)]
pub struct Span {
    pub start_ns: u64,
    pub end_ns: u64,
    pub label: String,
    pub depth: u8,
    /// Frame command index.
    pub index: usize,
}

pub struct CounterTrack {
    pub name: String,
    pub unit: String,
    pub samples: Vec<CounterSample>,
}

pub struct CounterSample {
    pub start_ns: u64,
    pub end_ns: u64,
    pub value: f64,
    pub total: f64,
}

// ---------------------------------------------------------------------------------------------
// Instrumentation state (lives inside the replay engine during a Trace replay)
// ---------------------------------------------------------------------------------------------

/// One timestamp pair; `item` = None for pass-level slots.
pub(crate) struct SlotRec {
    pub item: Option<usize>,
    pub pass: ObjId,
    pub begin: u64,
    pub end: u64,
}

impl SlotRec {
    fn valid(&self) -> bool {
        self.begin > 0 && self.end >= self.begin
    }
}

/// Raw results of one instrumented replay.
#[derive(Default)]
pub(crate) struct TraceData {
    pub slots: Vec<SlotRec>,
    pub unsplit_items: Vec<(usize, ObjId)>,
    /// item → occlusion samples / pass sample count.
    pub occ: HashMap<usize, f64>,
    pub indirect: HashMap<usize, Vec<u32>>,
    pub gl_dur: HashMap<usize, u64>,
    pub gl_fb: HashMap<usize, String>,
    pub gl_src: HashMap<usize, (String, String)>,
    pub frags: HashMap<usize, f64>,
    pub notes: Vec<String>,
    pub disjoint: bool,
    pub errors: usize,
}

impl TraceData {
    fn note(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !self.notes.contains(&s) {
            self.notes.push(s);
        }
    }

    fn total_ns(&self) -> u64 {
        let gpu: u64 = self
            .slots
            .iter()
            .filter(|s| s.item.is_some() && s.valid())
            .map(|s| s.end - s.begin)
            .sum();
        gpu + self.gl_dur.values().sum::<u64>()
    }
}

enum Query {
    Ts(u32),
    Occ(u32),
}

struct TimingDev {
    device: JsValue,
    /// (query set, resolve buffer) per 2048 slots.
    ts: Vec<(JsValue, JsValue)>,
    occ: Vec<(JsValue, JsValue)>,
    /// Slot index → `TraceData::slots` index.
    slots: Vec<usize>,
    /// Occlusion query k → (item, sample count).
    occ_items: Vec<(usize, f64)>,
    indirect: Vec<(usize, JsValue, u32)>,
}

struct PassRun {
    enc: JsValue,
    enc_id: ObjId,
    dev: usize,
    desc: JsValue,
    compute: bool,
    unsplit: bool,
    sample_count: f64,
    dyn_state: BTreeMap<String, (String, Vec<JsValue>)>,
    segments: u32,
}

#[derive(Default)]
struct FragCtx {
    progs: Option<js_sys::Map>,
    target: Option<(JsValue, JsValue, u32, u32)>,
}

pub(crate) struct Instr {
    pub kind: TraceInstr,
    pub data: TraceData,
    items: HashMap<usize, CmdKind>,
    unsplit: HashSet<ObjId>,
    devs: Vec<TimingDev>,
    no_ts: Vec<JsValue>,
    passes: HashMap<ObjId, PassRun>,
    enc_queries: HashMap<ObjId, Vec<(usize, Query)>>,
    gl_cur: Option<(usize, JsValue)>,
    gl_queries: Vec<(usize, usize, JsValue)>,
    frag: HashMap<usize, FragCtx>,
}

fn is_item(k: &CmdKind) -> bool {
    matches!(k, CmdKind::Draw | CmdKind::Dispatch | CmdKind::Clear)
}

impl Instr {
    pub fn new(cap: &Capture, kind: TraceInstr) -> Instr {
        let items = build_tree(cap)
            .into_iter()
            .filter(|n| {
                is_item(&n.kind)
                    && cap
                        .frame
                        .get(n.index)
                        .is_some_and(|c| cap.class_of(c.this) != "GPURenderBundleEncoder")
            })
            .map(|n| (n.index, n.kind))
            .collect();
        let mut unsplit = HashSet::new();
        for c in &cap.frame {
            match (cap.class_of(c.this), cap.method(c)) {
                (
                    "GPURenderPassEncoder",
                    "beginOcclusionQuery" | "executeBundles" | "writeTimestamp",
                ) => {
                    unsplit.insert(c.this);
                }
                ("GPUCommandEncoder", "beginRenderPass" | "beginComputePass") => {
                    let d = c.args.first();
                    let special = d.is_some_and(|d| {
                        cap.dict_get(d, "occlusionQuerySet")
                            .is_some_and(|v| !v.is_nullish())
                            || cap
                                .dict_get(d, "timestampWrites")
                                .is_some_and(|v| !v.is_nullish())
                    });
                    if special && let Some(id) = c.result.as_obj() {
                        unsplit.insert(id);
                    }
                }
                _ => {}
            }
        }
        Instr {
            kind,
            data: TraceData::default(),
            items,
            unsplit,
            devs: Vec::new(),
            no_ts: Vec::new(),
            passes: HashMap::new(),
            enc_queries: HashMap::new(),
            gl_cur: None,
            gl_queries: Vec::new(),
            frag: HashMap::new(),
        }
    }
}

fn create_buffer(dev: &JsValue, size: u32, usage: u32, label: &str) -> Result<JsValue, String> {
    js::call(
        dev,
        "createBuffer",
        &[js::obj(&[
            ("size", js::num(size as f64)),
            ("usage", js::num(usage as f64)),
            ("label", label.into()),
        ])],
    )
    .map_err(jserr)
}

fn create_query_set(dev: &JsValue, ty: &str, count: u32) -> Result<JsValue, String> {
    js::call(
        dev,
        "createQuerySet",
        &[js::obj(&[
            ("type", ty.into()),
            ("count", js::num(count as f64)),
        ])],
    )
    .map_err(jserr)
}

fn ts_writes(set: &JsValue, q: u32) -> JsValue {
    js::obj(&[
        ("querySet", set.clone()),
        ("beginningOfPassWriteIndex", js::num(q as f64)),
        ("endOfPassWriteIndex", js::num((q + 1) as f64)),
    ])
}

/// Segment descriptor: later segments load what earlier ones stored.
fn segment_desc(base: &JsValue, compute: bool, first: bool) -> JsValue {
    let d = js::clone_obj(base);
    let _ = js_sys::Reflect::delete_property(
        d.unchecked_ref::<js_sys::Object>(),
        &"timestampWrites".into(),
    );
    let _ = js_sys::Reflect::delete_property(
        d.unchecked_ref::<js_sys::Object>(),
        &"occlusionQuerySet".into(),
    );
    if compute {
        return d;
    }
    let colors: Vec<JsValue> = js::array_items(&js::get(&d, "colorAttachments"))
        .into_iter()
        .map(|a| {
            if !js::is_obj(&a) {
                return a;
            }
            let c = js::clone_obj(&a);
            if !first {
                js::set(&c, "loadOp", &"load".into());
            }
            js::set(&c, "storeOp", &"store".into());
            c
        })
        .collect();
    js::set(&d, "colorAttachments", &js::arr(&colors));
    let ds = js::get(&d, "depthStencilAttachment");
    if js::is_obj(&ds) {
        let c = js::clone_obj(&ds);
        for (load, store) in [
            ("depthLoadOp", "depthStoreOp"),
            ("stencilLoadOp", "stencilStoreOp"),
        ] {
            if !first && !js::get(&c, load).is_undefined() {
                js::set(&c, load, &"load".into());
            }
            if !js::get(&c, store).is_undefined() {
                js::set(&c, store, &"store".into());
            }
        }
        js::set(&d, "depthStencilAttachment", &c);
    }
    d
}

use wasm_bindgen::JsCast;

impl Engine {
    fn instr(&mut self) -> &mut Instr {
        self.instr.as_mut().expect("trace mode")
    }

    fn tnote(&mut self, s: impl Into<String>) {
        self.instr().data.note(s);
    }

    /// Timing device index for `dev`; None (with a note) if timestamp-query is unavailable.
    fn ensure_dev(&mut self, dev: &JsValue) -> Option<usize> {
        let ins = self.instr();
        if let Some(i) = ins.devs.iter().position(|d| d.device == *dev) {
            return Some(i);
        }
        if ins.no_ts.contains(dev) {
            return None;
        }
        let has = js::truthy(
            &js::call(
                &js::get(dev, "features"),
                "has",
                &["timestamp-query".into()],
            )
            .unwrap_or_default(),
        );
        if !has {
            ins.no_ts.push(dev.clone());
            ins.data
                .note("timestamp-query unsupported: WebGPU items use a nominal layout");
            return None;
        }
        ins.devs.push(TimingDev {
            device: dev.clone(),
            ts: Vec::new(),
            occ: Vec::new(),
            slots: Vec::new(),
            occ_items: Vec::new(),
            indirect: Vec::new(),
        });
        Some(ins.devs.len() - 1)
    }

    /// Allocates a timestamp pair → (query set, first query index, slot).
    fn alloc_slot(
        &mut self,
        dev: usize,
        item: Option<usize>,
        pass: ObjId,
    ) -> Result<(JsValue, u32, u32), String> {
        let ins = self.instr();
        let slot = ins.devs[dev].slots.len() as u32;
        if slot.is_multiple_of(SLOTS_PER_SET) {
            let d = ins.devs[dev].device.clone();
            let set = create_query_set(&d, "timestamp", SLOTS_PER_SET * 2)?;
            let resolve = create_buffer(
                &d,
                SLOTS_PER_SET * RESOLVE_STRIDE,
                buf_usage::QUERY_RESOLVE | buf_usage::COPY_SRC,
                "webgd:ts-resolve",
            )?;
            ins.devs[dev].ts.push((set, resolve));
        }
        ins.data.slots.push(SlotRec {
            item,
            pass,
            begin: 0,
            end: 0,
        });
        let rec = ins.data.slots.len() - 1;
        ins.devs[dev].slots.push(rec);
        let set = ins.devs[dev].ts[(slot / SLOTS_PER_SET) as usize].0.clone();
        Ok((set, (slot % SLOTS_PER_SET) * 2, slot))
    }

    fn alloc_occ(
        &mut self,
        dev: usize,
        item: usize,
        sample_count: f64,
    ) -> Result<(JsValue, u32, u32), String> {
        let ins = self.instr();
        let k = ins.devs[dev].occ_items.len() as u32;
        if k.is_multiple_of(OCC_PER_SET) {
            let d = ins.devs[dev].device.clone();
            let set = create_query_set(&d, "occlusion", OCC_PER_SET)?;
            let resolve = create_buffer(
                &d,
                OCC_PER_SET * RESOLVE_STRIDE,
                buf_usage::QUERY_RESOLVE | buf_usage::COPY_SRC,
                "webgd:occ-resolve",
            )?;
            ins.devs[dev].occ.push((set, resolve));
        }
        ins.devs[dev].occ_items.push((item, sample_count));
        let set = ins.devs[dev].occ[(k / OCC_PER_SET) as usize].0.clone();
        Ok((set, k % OCC_PER_SET, k))
    }

    fn view_sample_count(&self, view: Option<ObjId>) -> Option<f64> {
        let tex = self.cap.objects.get(&view?)?.parent?;
        js::get(self.st.objs.fwd.get(&tex)?, "sampleCount").as_f64()
    }

    /// WebGPU timed-run interception. Returns true if the call was fully handled.
    pub(crate) async fn trace_gpu(
        &mut self,
        call: &Call,
        method: &str,
        class: &str,
        index: usize,
    ) -> Result<bool, String> {
        if self.instr().kind != TraceInstr::Timed {
            return Ok(false);
        }
        let cap = self.cap.clone();
        if class == "GPUCommandEncoder" && matches!(method, "beginRenderPass" | "beginComputePass")
        {
            let enc = self.obj(call.this)?;
            let Some(dev) = self.device_of(call.this) else {
                return Ok(false);
            };
            let Some(devi) = self.ensure_dev(&dev) else {
                return Ok(false);
            };
            let Some(pass_id) = call.result.as_obj() else {
                return Ok(false);
            };
            let desc = match call.args.first() {
                Some(a) => js::clone_obj(&self.arg(a)?),
                None => js::obj(&[]),
            };
            let compute = method == "beginComputePass";
            let sample_count = if compute {
                1.0
            } else {
                let d = call.args.first();
                let color0 = d
                    .and_then(|d| cap.dict_get(d, "colorAttachments"))
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find(|x| !x.is_nullish()))
                    .and_then(|a| cap.dict_get(a, "view"))
                    .and_then(Value::as_obj);
                let depth = d
                    .and_then(|d| cap.dict_get(d, "depthStencilAttachment"))
                    .and_then(|a| cap.dict_get(a, "view"))
                    .and_then(Value::as_obj);
                self.view_sample_count(color0)
                    .or_else(|| self.view_sample_count(depth))
                    .unwrap_or(1.0)
                    .max(1.0)
            };
            let unsplit = self.instr().unsplit.contains(&pass_id);
            if unsplit {
                let (set, q, slot) = self.alloc_slot(devi, None, pass_id)?;
                js::set(&desc, "timestampWrites", &ts_writes(&set, q));
                let p = js::call(&enc, method, std::slice::from_ref(&desc)).map_err(jserr)?;
                self.map(pass_id, p);
                self.open_passes.push(pass_id);
                self.instr()
                    .enc_queries
                    .entry(call.this)
                    .or_default()
                    .push((devi, Query::Ts(slot)));
            }
            self.instr().passes.insert(
                pass_id,
                PassRun {
                    enc,
                    enc_id: call.this,
                    dev: devi,
                    desc,
                    compute,
                    unsplit,
                    sample_count,
                    dyn_state: BTreeMap::new(),
                    segments: 0,
                },
            );
            return Ok(true);
        }
        let Some(unsplit) = self.instr().passes.get(&call.this).map(|p| p.unsplit) else {
            return Ok(false);
        };
        if unsplit {
            if self.instr().items.contains_key(&index) {
                self.instr().data.unsplit_items.push((index, call.this));
            }
            return Ok(false);
        }
        let args = self.args(call)?;
        let key = match method {
            "setBindGroup" | "setVertexBuffer" => Some(format!(
                "{method}:{}",
                call.args.first().and_then(Value::as_f64).unwrap_or(0.0)
            )),
            "setPipeline"
            | "setIndexBuffer"
            | "setViewport"
            | "setScissorRect"
            | "setBlendConstant"
            | "setStencilReference" => Some(method.to_string()),
            _ => None,
        };
        if let Some(k) = key {
            if let Some(p) = self.instr().passes.get_mut(&call.this) {
                p.dyn_state.insert(k, (method.to_string(), args));
            }
            return Ok(true);
        }
        match method {
            "pushDebugGroup" | "popDebugGroup" | "insertDebugMarker" => Ok(true),
            "end" => {
                if self
                    .instr()
                    .passes
                    .get(&call.this)
                    .is_some_and(|p| p.segments == 0)
                {
                    self.run_segment(call.this, None)?;
                }
                Ok(true)
            }
            m if m.starts_with("draw") || m.starts_with("dispatch") => {
                self.run_segment(call.this, Some((index, method, args)))?;
                Ok(true)
            }
            _ => Err(format!("{method} is not supported inside a split pass")),
        }
    }

    fn run_segment(&mut self, pass_id: ObjId, item: Option<(usize, &str, Vec<JsValue>)>) -> R {
        let (enc, enc_id, devi, desc, compute, first, sample_count, dyn_calls) = {
            let p = self.instr().passes.get(&pass_id).ok_or("unknown pass")?;
            (
                p.enc.clone(),
                p.enc_id,
                p.dev,
                segment_desc(&p.desc, p.compute, p.segments == 0),
                p.compute,
                p.segments == 0,
                p.sample_count,
                p.dyn_state.values().cloned().collect::<Vec<_>>(),
            )
        };
        let _ = first;
        let (set, q, slot) = self.alloc_slot(devi, item.as_ref().map(|i| i.0), pass_id)?;
        js::set(&desc, "timestampWrites", &ts_writes(&set, q));
        let is_draw = item.as_ref().is_some_and(|i| i.1.starts_with("draw"));
        let mut occ = None;
        if !compute && is_draw {
            let idx = item.as_ref().map(|i| i.0).unwrap_or(0);
            let (oset, oq, k) = self.alloc_occ(devi, idx, sample_count)?;
            js::set(&desc, "occlusionQuerySet", &oset);
            occ = Some((oq, k));
        }
        let p = js::call(
            &enc,
            if compute {
                "beginComputePass"
            } else {
                "beginRenderPass"
            },
            &[desc],
        )
        .map_err(jserr)?;
        let mut errs = Vec::new();
        for (m, a) in &dyn_calls {
            if let Err(e) = js::call(&p, m, a) {
                errs.push(format!("{m}: {}", jserr(e)));
            }
        }
        if let Some((index, method, args)) = &item {
            if let Some((oq, _)) = occ {
                let _ = js::call(&p, "beginOcclusionQuery", &[js::num(oq as f64)]);
            }
            if let Err(e) = js::call(&p, method, args) {
                errs.push(format!("{method}: {}", jserr(e)));
            }
            if occ.is_some() {
                let _ = js::call(&p, "endOcclusionQuery", &[]);
            }
            js::call(&p, "end", &[]).map_err(jserr)?;
            let words = match *method {
                "drawIndirect" => 4,
                "drawIndexedIndirect" => 5,
                "dispatchWorkgroupsIndirect" => 3,
                _ => 0,
            };
            if words > 0 {
                let dev = self.instr().devs[devi].device.clone();
                let st = create_buffer(
                    &dev,
                    32,
                    buf_usage::MAP_READ | buf_usage::COPY_DST,
                    "webgd:indirect",
                )?;
                let off = args.get(1).cloned().unwrap_or(js::num(0.0));
                js::call(
                    &enc,
                    "copyBufferToBuffer",
                    &[
                        args[0].clone(),
                        off,
                        st.clone(),
                        js::num(0.0),
                        js::num((words * 4) as f64),
                    ],
                )
                .map_err(jserr)?;
                self.instr().devs[devi].indirect.push((*index, st, words));
            }
        } else {
            js::call(&p, "end", &[]).map_err(jserr)?;
        }
        let ins = self.instr();
        let qs = ins.enc_queries.entry(enc_id).or_default();
        qs.push((devi, Query::Ts(slot)));
        if let Some((_, k)) = occ {
            qs.push((devi, Query::Occ(k)));
        }
        if let Some(p) = ins.passes.get_mut(&pass_id) {
            p.segments += 1;
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    /// Resolves the encoder's queries before `finish()`.
    pub(crate) fn trace_before_finish(&mut self, enc_id: ObjId, enc: &JsValue) {
        let Some(qs) = self.instr().enc_queries.remove(&enc_id) else {
            return;
        };
        let ins = self.instr();
        for (devi, q) in qs {
            let d = &ins.devs[devi];
            let r = match q {
                Query::Ts(s) => {
                    let (set, buf) = &d.ts[(s / SLOTS_PER_SET) as usize];
                    let i = s % SLOTS_PER_SET;
                    js::call(
                        enc,
                        "resolveQuerySet",
                        &[
                            set.clone(),
                            n(i * 2),
                            n(2),
                            buf.clone(),
                            n(i * RESOLVE_STRIDE),
                        ],
                    )
                }
                Query::Occ(k) => {
                    let (set, buf) = &d.occ[(k / OCC_PER_SET) as usize];
                    let i = k % OCC_PER_SET;
                    js::call(
                        enc,
                        "resolveQuerySet",
                        &[set.clone(), n(i), n(1), buf.clone(), n(i * RESOLVE_STRIDE)],
                    )
                }
            };
            if let Err(e) = r {
                ins.data
                    .note(format!("resolveQuerySet failed: {}", jserr(e)));
            }
        }
    }

    /// GL hooks around an item call. Returns true if a timer query was begun.
    pub(crate) fn trace_before_gl(
        &mut self,
        call: &Call,
        index: usize,
        this: &JsValue,
        method: &str,
        args: &[JsValue],
    ) -> bool {
        let Some(kind) = self.instr().items.get(&index).cloned() else {
            return false;
        };
        let Some(ci) = self.gl_index(call.this) else {
            return false;
        };
        match self.instr().kind {
            TraceInstr::Timed => {
                let ctx = &self.st.rt.gl[ci];
                let fb = ctx.param(DRAW_FRAMEBUFFER_BINDING);
                let fb_label = match self.st.objs.id_of(&fb) {
                    Some(id) => obj_label(&self.cap, id),
                    None => "default".to_string(),
                };
                let src = (kind == CmdKind::Draw)
                    .then(|| program_sources(ctx))
                    .flatten();
                let has_timer = ctx.timer_ext.is_some();
                let q = if has_timer {
                    let q = ctx.call("createQuery", &[]).unwrap_or_default();
                    let _ = ctx.call("beginQuery", &[n(TIME_ELAPSED_EXT), q.clone()]);
                    Some(q)
                } else {
                    None
                };
                let ins = self.instr();
                ins.data.gl_fb.insert(index, fb_label);
                if let Some(s) = src {
                    ins.data.gl_src.insert(index, s);
                }
                match q {
                    Some(q) => {
                        ins.gl_cur = Some((ci, q));
                        true
                    }
                    None => {
                        ins.data.note(
                            "EXT_disjoint_timer_query_webgl2 unavailable: GL items have no timing",
                        );
                        false
                    }
                }
            }
            TraceInstr::GlFragmentCount => {
                if kind == CmdKind::Draw {
                    self.count_fragments(ci, index, this, method, args);
                }
                false
            }
        }
    }

    pub(crate) fn trace_after_gl(&mut self, _call: &Call, index: usize) {
        let Some((ci, q)) = self.instr().gl_cur.take() else {
            return;
        };
        let _ = self.st.rt.gl[ci].call("endQuery", &[n(TIME_ELAPSED_EXT)]);
        self.instr().gl_queries.push((ci, index, q));
    }

    fn counter_program(&mut self, ci: usize, prog: &JsValue) -> Result<JsValue, String> {
        let map = self
            .instr()
            .frag
            .entry(ci)
            .or_default()
            .progs
            .get_or_insert_with(js_sys::Map::new)
            .clone();
        let cached = map.get(prog);
        if js::is_obj(&cached) {
            return Ok(cached);
        }
        let ctx = &self.st.rt.gl[ci];
        let shaders = js::array_items(
            &ctx.call("getAttachedShaders", std::slice::from_ref(prog))
                .unwrap_or_default(),
        );
        let vs = shaders
            .into_iter()
            .find(|s| {
                js::u(
                    &ctx.call("getShaderParameter", &[s.clone(), n(SHADER_TYPE)])
                        .unwrap_or_default(),
                ) == VERTEX_SHADER
            })
            .ok_or("program has no vertex shader")?;
        let src = ctx
            .call("getShaderSource", std::slice::from_ref(&vs))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let fs = if src.trim_start().starts_with("#version 300 es") {
            "#version 300 es\nprecision mediump float;\nout vec4 webgd_o;\nvoid main(){webgd_o=vec4(1.0);}"
        } else {
            "precision mediump float;\nvoid main(){gl_FragColor=vec4(1.0);}"
        };
        let count = js::u(
            &ctx.call("getProgramParameter", &[prog.clone(), n(ACTIVE_ATTRIBUTES)])
                .unwrap_or_default(),
        );
        let mut attribs = Vec::new();
        for i in 0..count {
            let info = ctx
                .call("getActiveAttrib", &[prog.clone(), n(i)])
                .unwrap_or_default();
            let Some(name) = js::get(&info, "name").as_string() else {
                continue;
            };
            let loc = ctx
                .call("getAttribLocation", &[prog.clone(), name.as_str().into()])
                .unwrap_or_default();
            if js::f(&loc) >= 0.0 {
                attribs.push((name, loc));
            }
        }
        let p = ctx.build_program(&vs, fs, &attribs)?;
        map.set(prog, &p);
        Ok(p)
    }

    /// Size of the current draw target (attachment 0 / depth, default framebuffer, else viewport).
    fn target_size(&self, ci: usize, index: usize) -> (u32, u32) {
        let ctx = &self.st.rt.gl[ci];
        let fb = ctx.param(DRAW_FRAMEBUFFER_BINDING);
        let clamp = |(w, h): (u32, u32)| (w.clamp(1, 8192), h.clamp(1, 8192));
        if fb.is_null() {
            return clamp((
                js::u(&js::get(&ctx.gl, "drawingBufferWidth")),
                js::u(&js::get(&ctx.gl, "drawingBufferHeight")),
            ));
        }
        let ap = |att: u32, pn: u32| {
            ctx.call(
                "getFramebufferAttachmentParameter",
                &[n(DRAW_FRAMEBUFFER), n(att), n(pn)],
            )
            .unwrap_or(JsValue::NULL)
        };
        for att in [
            COLOR_ATTACHMENT0,
            DEPTH_ATTACHMENT,
            DEPTH_STENCIL_ATTACHMENT,
        ] {
            let ty = js::u(&ap(att, FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE));
            let obj = ap(att, FRAMEBUFFER_ATTACHMENT_OBJECT_NAME);
            if ty == RENDERBUFFER {
                let prev = ctx.param(RENDERBUFFER_BINDING);
                let _ = ctx.call("bindRenderbuffer", &[n(RENDERBUFFER), obj]);
                let p = |pn| {
                    js::u(
                        &ctx.call("getRenderbufferParameter", &[n(RENDERBUFFER), n(pn)])
                            .unwrap_or_default(),
                    )
                };
                let r = (p(RENDERBUFFER_WIDTH), p(RENDERBUFFER_HEIGHT));
                let _ = ctx.call("bindRenderbuffer", &[n(RENDERBUFFER), prev]);
                return clamp(r);
            }
            if ty == TEXTURE
                && let Some(id) = self.st.objs.id_of(&obj)
                && let Some(d) = texture_dims(&self.cap, index, id)
            {
                let level = js::u(&ap(att, FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL));
                let face = js::u(&ap(att, FRAMEBUFFER_ATTACHMENT_TEXTURE_CUBE_MAP_FACE));
                let face = face
                    .checked_sub(TEXTURE_CUBE_MAP_POSITIVE_X)
                    .filter(|f| *f < 6)
                    .unwrap_or(0);
                if let Some(&(w, h, _)) = d.levels.get(&(level, face)) {
                    return clamp((w, h));
                }
            }
        }
        let vp: Vec<u32> = js_sys::Array::from(&ctx.param(VIEWPORT))
            .iter()
            .map(|v| js::u(&v))
            .collect();
        if vp.len() == 4 {
            clamp((vp[0] + vp[2], vp[1] + vp[3]))
        } else {
            (1, 1)
        }
    }

    fn count_fragments(
        &mut self,
        ci: usize,
        index: usize,
        this: &JsValue,
        method: &str,
        args: &[JsValue],
    ) {
        if !self.st.rt.gl[ci].has_cbf {
            self.tnote("float render targets unsupported: no GL fragment counts");
            return;
        }
        let ctx_disc = js::truthy(
            &self.st.rt.gl[ci]
                .call("isEnabled", &[n(RASTERIZER_DISCARD)])
                .unwrap_or_default(),
        );
        if ctx_disc {
            self.instr().data.frags.insert(index, 0.0);
            return;
        }
        let prog = self.st.rt.gl[ci].param(CURRENT_PROGRAM);
        if !js::is_obj(&prog) {
            return;
        }
        let counter = match self.counter_program(ci, &prog) {
            Ok(p) => p,
            Err(e) => {
                self.tnote(format!("fragment counting program failed: {e}"));
                return;
            }
        };
        let (w, h) = self.target_size(ci, index);
        let ctx = &self.st.rt.gl[ci];
        let saved = ctx.save(
            &[
                DRAW_FRAMEBUFFER_BINDING,
                READ_FRAMEBUFFER_BINDING,
                CURRENT_PROGRAM,
                BLEND_SRC_RGB,
                BLEND_DST_RGB,
                BLEND_SRC_ALPHA,
                BLEND_DST_ALPHA,
                BLEND_EQUATION_RGB,
                BLEND_EQUATION_ALPHA,
                COLOR_WRITEMASK,
                COLOR_CLEAR_VALUE,
                ACTIVE_TEXTURE,
            ],
            &[
                BLEND,
                DEPTH_TEST,
                STENCIL_TEST,
                SCISSOR_TEST,
                SAMPLE_ALPHA_TO_COVERAGE,
            ],
        );
        let scissor_on = js::truthy(
            &ctx.call("isEnabled", &[n(SCISSOR_TEST)])
                .unwrap_or_default(),
        );
        // Scratch R16F target sized to the current draw target.
        let cur = self
            .instr
            .as_ref()
            .and_then(|i| i.frag.get(&ci))
            .and_then(|f| f.target.clone());
        let ctx = &self.st.rt.gl[ci];
        let (fbo, new_target) = match cur {
            Some((_, fbo, tw, th)) if (tw, th) == (w, h) => (fbo, None),
            old => {
                if let Some((t, f, _, _)) = old {
                    let _ = ctx.call("deleteTexture", &[t]);
                    let _ = ctx.call("deleteFramebuffer", &[f]);
                }
                let tex = ctx.call("createTexture", &[]).unwrap_or_default();
                ctx.c("activeTexture", &[(TEXTURE0 + ctx.scratch_unit) as f64]);
                let prev = ctx.param(TEXTURE_BINDING_2D);
                ctx.bind_tex(TEXTURE_2D, &tex);
                ctx.c(
                    "texStorage2D",
                    &[TEXTURE_2D as f64, 1.0, R16F as f64, w as f64, h as f64],
                );
                ctx.bind_tex(TEXTURE_2D, &prev);
                let fbo = ctx.call("createFramebuffer", &[]).unwrap_or_default();
                ctx.bind_fb(DRAW_FRAMEBUFFER, &fbo);
                let _ = ctx.call(
                    "framebufferTexture2D",
                    &[
                        n(DRAW_FRAMEBUFFER),
                        n(COLOR_ATTACHMENT0),
                        n(TEXTURE_2D),
                        tex.clone(),
                        n(0),
                    ],
                );
                let _ = ctx.call("drawBuffers", &[js::arr(&[n(COLOR_ATTACHMENT0)])]);
                (fbo.clone(), Some((tex, fbo, w, h)))
            }
        };
        ctx.bind_fb(DRAW_FRAMEBUFFER, &fbo);
        let _ = ctx.call(
            "colorMask",
            &[JsValue::TRUE, JsValue::TRUE, JsValue::TRUE, JsValue::TRUE],
        );
        ctx.c("disable", &[SCISSOR_TEST as f64]);
        ctx.c("clearColor", &[0.0, 0.0, 0.0, 0.0]);
        ctx.c("clear", &[COLOR_BUFFER_BIT as f64]);
        if scissor_on {
            ctx.c("enable", &[SCISSOR_TEST as f64]);
        }
        let _ = ctx.call("useProgram", std::slice::from_ref(&counter));
        for (name, ty) in plain_uniforms(ctx, &prog) {
            let lo = ctx
                .call("getUniformLocation", &[prog.clone(), name.as_str().into()])
                .unwrap_or(JsValue::NULL);
            let ln = ctx
                .call(
                    "getUniformLocation",
                    &[counter.clone(), name.as_str().into()],
                )
                .unwrap_or(JsValue::NULL);
            if lo.is_null() || ln.is_null() {
                continue;
            }
            let v = ctx
                .call("getUniform", &[prog.clone(), lo])
                .unwrap_or_default();
            let _ = set_uniform(&ctx.gl, ty, &ln, &v);
        }
        let blocks = js::u(
            &ctx.call(
                "getProgramParameter",
                &[counter.clone(), n(ACTIVE_UNIFORM_BLOCKS)],
            )
            .unwrap_or_default(),
        );
        for i in 0..blocks {
            let name = ctx
                .call("getActiveUniformBlockName", &[counter.clone(), n(i)])
                .unwrap_or_default();
            let oi = ctx
                .call("getUniformBlockIndex", &[prog.clone(), name])
                .unwrap_or_default();
            if js::u(&oi) == u32::MAX {
                continue;
            }
            let b = ctx
                .call(
                    "getActiveUniformBlockParameter",
                    &[prog.clone(), oi, n(UNIFORM_BLOCK_BINDING)],
                )
                .unwrap_or_default();
            let _ = ctx.call("uniformBlockBinding", &[counter.clone(), n(i), b]);
        }
        ctx.c("enable", &[BLEND as f64]);
        ctx.c("blendFunc", &[ONE as f64, ONE as f64]);
        ctx.c("blendEquation", &[FUNC_ADD as f64]);
        for c in [DEPTH_TEST, STENCIL_TEST, SAMPLE_ALPHA_TO_COVERAGE] {
            ctx.c("disable", &[c as f64]);
        }
        let drew = js::call(this, method, args).is_ok();
        ctx.bind_fb(READ_FRAMEBUFFER, &fbo);
        let sum = if drew {
            ctx.read_pixels(w, h, gl::formats::ReadClass::Float)
                .ok()
                .map(|img| {
                    img.data
                        .as_chunks::<16>()
                        .0
                        .iter()
                        .map(|t| f32::from_le_bytes([t[0], t[1], t[2], t[3]]) as f64)
                        .sum::<f64>()
                })
        } else {
            None
        };
        ctx.restore(saved);
        let ins = self.instr();
        if let Some(t) = new_target {
            ins.frag.entry(ci).or_default().target = Some(t);
        }
        if let Some(s) = sum {
            ins.data.frags.insert(index, s);
        }
    }

    /// After the frame: read back timestamps/occlusion/indirect args and GL timer queries.
    pub(crate) async fn trace_collect(&mut self) {
        let devs = std::mem::take(&mut self.instr().devs);
        for d in &devs {
            if let Err(e) = self.collect_device(d).await {
                self.tnote(format!("timestamp readback failed: {e}"));
            }
        }
        let queries = std::mem::take(&mut self.instr().gl_queries);
        if !queries.is_empty() {
            for ctx in &self.st.rt.gl {
                let _ = ctx.call("flush", &[]);
            }
            let mut waited = 0.0;
            loop {
                let all = queries.iter().all(|(ci, _, q)| {
                    js::truthy(
                        &self.st.rt.gl[*ci]
                            .call("getQueryParameter", &[q.clone(), n(QUERY_RESULT_AVAILABLE)])
                            .unwrap_or_default(),
                    )
                });
                if all || waited >= GL_POLL_TIMEOUT_MS {
                    break;
                }
                js::sleep(GL_POLL_MS).await;
                waited += GL_POLL_MS;
            }
            let disjoint = self
                .st
                .rt
                .gl
                .iter()
                .any(|c| js::truthy(&c.param(GPU_DISJOINT_EXT)));
            let mut durs = HashMap::new();
            let mut missing = 0;
            for (ci, idx, q) in &queries {
                let ctx = &self.st.rt.gl[*ci];
                if js::truthy(
                    &ctx.call("getQueryParameter", &[q.clone(), n(QUERY_RESULT_AVAILABLE)])
                        .unwrap_or_default(),
                ) {
                    durs.insert(
                        *idx,
                        js::f(
                            &ctx.call("getQueryParameter", &[q.clone(), n(QUERY_RESULT)])
                                .unwrap_or_default(),
                        ) as u64,
                    );
                } else {
                    missing += 1;
                }
                let _ = ctx.call("deleteQuery", std::slice::from_ref(q));
            }
            let ins = self.instr();
            ins.data.gl_dur = durs;
            ins.data.disjoint = disjoint;
            if missing > 0 {
                ins.data.note(format!(
                    "{missing} GL timer queries did not complete within 5 s"
                ));
            }
        }
        let errors = self.st.errors.len();
        self.instr().data.errors = errors;
    }

    async fn collect_device(&mut self, d: &TimingDev) -> R {
        let dev = &d.device;
        let enc = js::call(dev, "createCommandEncoder", &[]).map_err(jserr)?;
        let mut ts_st = Vec::new();
        let mut occ_st = Vec::new();
        for (list, out, per) in [
            (&d.ts, &mut ts_st, SLOTS_PER_SET),
            (&d.occ, &mut occ_st, OCC_PER_SET),
        ] {
            for (_, resolve) in list.iter() {
                let size = per * RESOLVE_STRIDE;
                let st = create_buffer(
                    dev,
                    size,
                    buf_usage::MAP_READ | buf_usage::COPY_DST,
                    "webgd:query-readback",
                )?;
                js::call(
                    &enc,
                    "copyBufferToBuffer",
                    &[resolve.clone(), n(0), st.clone(), n(0), n(size)],
                )
                .map_err(jserr)?;
                out.push(st);
            }
        }
        let cb = js::call(&enc, "finish", &[]).map_err(jserr)?;
        js::call(&js::get(dev, "queue"), "submit", &[js::arr(&[cb])]).map_err(jserr)?;
        let u64_at = |b: &[u8], off: usize| {
            b.get(off..off + 8)
                .map(|s| u64::from_le_bytes(s.try_into().unwrap_or([0; 8])))
                .unwrap_or(0)
        };
        let mut ts_bytes = Vec::new();
        for st in &ts_st {
            ts_bytes.push(gpu::map_read(st, SLOTS_PER_SET * RESOLVE_STRIDE).await?);
            let _ = js::call(st, "destroy", &[]);
        }
        let mut occ_bytes = Vec::new();
        for st in &occ_st {
            occ_bytes.push(gpu::map_read(st, OCC_PER_SET * RESOLVE_STRIDE).await?);
            let _ = js::call(st, "destroy", &[]);
        }
        let mut indirect = Vec::new();
        for (idx, st, words) in &d.indirect {
            let b = gpu::map_read(st, 32).await?;
            let w: Vec<u32> = b
                .as_chunks::<4>()
                .0
                .iter()
                .take(*words as usize)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            indirect.push((*idx, w));
            let _ = js::call(st, "destroy", &[]);
        }
        let ins = self.instr();
        for (s, rec) in d.slots.iter().enumerate() {
            let s = s as u32;
            let Some(b) = ts_bytes.get((s / SLOTS_PER_SET) as usize) else {
                continue;
            };
            let off = ((s % SLOTS_PER_SET) * RESOLVE_STRIDE) as usize;
            ins.data.slots[*rec].begin = u64_at(b, off);
            ins.data.slots[*rec].end = u64_at(b, off + 8);
        }
        for (k, (item, sc)) in d.occ_items.iter().enumerate() {
            let k = k as u32;
            let Some(b) = occ_bytes.get((k / OCC_PER_SET) as usize) else {
                continue;
            };
            let v = u64_at(b, ((k % OCC_PER_SET) * RESOLVE_STRIDE) as usize);
            ins.data.occ.insert(*item, v as f64 / sc.max(1.0));
        }
        ins.data.indirect.extend(indirect);
        for (set, buf) in d.ts.iter().chain(d.occ.iter()) {
            let _ = js::call(set, "destroy", &[]);
            let _ = js::call(buf, "destroy", &[]);
        }
        Ok(())
    }
}

/// (vertex, fragment) sources of the current program.
fn program_sources(ctx: &gl::GlCtx) -> Option<(String, String)> {
    let prog = ctx.param(CURRENT_PROGRAM);
    if !js::is_obj(&prog) {
        return None;
    }
    let mut vs = String::new();
    let mut fs = String::new();
    for s in js::array_items(&ctx.call("getAttachedShaders", &[prog]).unwrap_or_default()) {
        let src = ctx
            .call("getShaderSource", std::slice::from_ref(&s))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        match js::u(
            &ctx.call("getShaderParameter", &[s, n(SHADER_TYPE)])
                .unwrap_or_default(),
        ) {
            VERTEX_SHADER => vs = src,
            _ => fs = src,
        }
    }
    Some((vs, fs))
}

// ---------------------------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------------------------

async fn timed_run(
    rep: &Replayer,
    cap: &Rc<Capture>,
    last: usize,
    mode: TraceInstr,
) -> Result<TraceData, ReplayError> {
    let mut st = rep.replay_to(cap, last, ReplayMode::Trace(mode)).await?;
    let data = st.rt.trace.take().unwrap_or_default();
    st.dispose();
    Ok(data)
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum QueueKey {
    Gpu(ObjId),
    Gl(ObjId),
}

fn queue_of(cap: &Capture, id: ObjId) -> Option<QueueKey> {
    if let Some(d) = ancestor(cap, id, |c| c == "GPUDevice") {
        return Some(QueueKey::Gpu(d));
    }
    ancestor(cap, id, |c| c == "WebGL2RenderingContext").map(QueueKey::Gl)
}

fn queue_of_call(cap: &Capture, call: &Call) -> Option<QueueKey> {
    queue_of(cap, call.this)
}

pub async fn run_trace(rep: &Replayer, cap: &Rc<Capture>) -> Result<TraceResult, ReplayError> {
    if cap.frame.is_empty() {
        return Err(ReplayError::Unsupported(
            "capture has an empty frame".into(),
        ));
    }
    let last = cap.frame.len() - 1;
    let mut runs = Vec::new();
    let mut disjoint_retries = 0;
    let mut notes: Vec<String> = Vec::new();
    while runs.len() < TRACE_RUNS {
        let data = timed_run(rep, cap, last, TraceInstr::Timed).await?;
        if data.disjoint && disjoint_retries < GL_DISJOINT_RETRIES {
            disjoint_retries += 1;
            continue;
        }
        if data.disjoint {
            notes.push("GPU_DISJOINT_EXT set: GL timings may be unreliable".into());
        }
        runs.push(data);
    }
    let tree = build_tree(cap);
    let has_gl_draw = tree.iter().any(|nd| {
        nd.kind == CmdKind::Draw && cap.class_of(cap.frame[nd.index].this).starts_with("WebGL")
    });
    let frag_run = if has_gl_draw {
        Some(timed_run(rep, cap, last, TraceInstr::GlFragmentCount).await?)
    } else {
        None
    };

    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by_key(|&i| runs[i].total_ns());
    let data = runs.swap_remove(order[order.len() / 2]);
    for n in data
        .notes
        .iter()
        .chain(frag_run.iter().flat_map(|f| f.notes.iter()))
    {
        if !notes.contains(n) {
            notes.push(n.clone());
        }
    }
    if data.errors > 0 {
        notes.push(format!(
            "{} replay errors during the trace run (see the Log panel for details)",
            data.errors
        ));
    }
    Ok(assemble(cap, &tree, &data, frag_run.as_ref(), notes))
}

struct ItemSpan {
    index: usize,
    queue: usize,
    start: u64,
    end: u64,
    unsplit: bool,
}

fn assemble(
    cap: &Capture,
    tree: &[webgd_core::track::commands::CmdNode],
    data: &TraceData,
    frag_run: Option<&TraceData>,
    mut notes: Vec<String>,
) -> TraceResult {
    // Queues in order of first appearance.
    let mut keys: Vec<QueueKey> = Vec::new();
    let queue_idx = |k: QueueKey, keys: &mut Vec<QueueKey>| match keys.iter().position(|x| *x == k)
    {
        Some(i) => i,
        None => {
            keys.push(k);
            keys.len() - 1
        }
    };
    let items: Vec<(usize, CmdKind)> = tree
        .iter()
        .filter(|nd| {
            is_item(&nd.kind) && cap.class_of(cap.frame[nd.index].this) != "GPURenderBundleEncoder"
        })
        .map(|nd| (nd.index, nd.kind.clone()))
        .collect();

    // GPU timing.
    let t0 = data
        .slots
        .iter()
        .filter(|s| s.valid())
        .map(|s| s.begin)
        .min()
        .unwrap_or(0);
    let mut pass_span: HashMap<ObjId, (u64, u64)> = HashMap::new();
    for s in data.slots.iter().filter(|s| s.valid()) {
        let e = pass_span.entry(s.pass).or_insert((u64::MAX, 0));
        e.0 = e.0.min(s.begin - t0);
        e.1 = e.1.max(s.end - t0);
    }
    let item_ts: HashMap<usize, (u64, u64)> = data
        .slots
        .iter()
        .filter(|s| s.valid())
        .filter_map(|s| s.item.map(|i| (i, (s.begin - t0, s.end - t0))))
        .collect();
    let unsplit: HashMap<usize, ObjId> = data.unsplit_items.iter().copied().collect();

    let mut spans: Vec<ItemSpan> = Vec::new();
    let mut gl_cursor: HashMap<usize, u64> = HashMap::new();
    let mut nominal_cursor: HashMap<usize, u64> = HashMap::new();
    let mut nominal_used = false;
    for (idx, _) in &items {
        let call = &cap.frame[*idx];
        let Some(qk) = queue_of_call(cap, call) else {
            continue;
        };
        let q = queue_idx(qk.clone(), &mut keys);
        let (start, end, uns) = match qk {
            QueueKey::Gl(_) => {
                let c = gl_cursor.entry(q).or_insert(0);
                let d = data.gl_dur.get(idx).copied().unwrap_or(0);
                let s = *c;
                *c += d;
                (s, s + d, false)
            }
            QueueKey::Gpu(_) => {
                if let Some(&(s, e)) = item_ts.get(idx) {
                    (s, e, false)
                } else if let Some(&(s, e)) = unsplit.get(idx).and_then(|p| pass_span.get(p)) {
                    (s, e, true)
                } else {
                    nominal_used = true;
                    let c = nominal_cursor.entry(q).or_insert(0);
                    let s = *c;
                    *c += NOMINAL_ITEM_NS;
                    (s, s + NOMINAL_ITEM_NS, false)
                }
            }
        };
        spans.push(ItemSpan {
            index: *idx,
            queue: q,
            start,
            end,
            unsplit: uns,
        });
    }
    if nominal_used {
        notes.push("WebGPU items without timestamps are laid out with a nominal 1 µs each".into());
    }
    if spans.iter().any(|s| s.unsplit) {
        notes.push("unsplit pass: items in passes with occlusion queries, timestamps or render bundles share the pass span".into());
    }
    // Queues referenced by markers / passes even without items.
    for c in &cap.frame {
        if matches!(
            cap.method(c),
            "pushDebugGroup" | "webgd:pushMarker" | "beginRenderPass" | "beginComputePass"
        ) && let Some(k) = queue_of_call(cap, c)
        {
            queue_idx(k, &mut keys);
        }
    }

    let mut queues: Vec<QueueTrack> = keys
        .iter()
        .map(|k| QueueTrack {
            name: match k {
                QueueKey::Gpu(d) => format!("Queue ({})", obj_label(cap, *d)),
                QueueKey::Gl(c) => format!("GL context #{}", c.0),
            },
            passes: Vec::new(),
            markers: Vec::new(),
            items: Vec::new(),
        })
        .collect();
    if keys.iter().any(|k| matches!(k, QueueKey::Gpu(_))) {
        notes.push(CHROME_TIMESTAMP_NOTE.into());
    }
    if keys.iter().any(|k| matches!(k, QueueKey::Gl(_))) {
        notes.push("WebGL: sequential layout, no GPU timestamps".into());
    }
    for s in &spans {
        let label = call_label(cap, &cap.frame[s.index]);
        let label = if s.unsplit {
            format!("{label} (unsplit pass)")
        } else {
            label
        };
        queues[s.queue].items.push(Span {
            start_ns: s.start,
            end_ns: s.end,
            label,
            depth: 0,
            index: s.index,
        });
    }

    // Passes.
    for (i, c) in cap.frame.iter().enumerate() {
        let m = cap.method(c);
        if !matches!(m, "beginRenderPass" | "beginComputePass") {
            continue;
        }
        let (Some(pass), Some(qk)) = (c.result.as_obj(), queue_of_call(cap, c)) else {
            continue;
        };
        let Some(&(s, e)) = pass_span.get(&pass) else {
            continue;
        };
        let q = keys.iter().position(|k| *k == qk).unwrap_or(0);
        let label = c
            .args
            .first()
            .and_then(|d| get_str(cap, d, "label"))
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{} {}",
                    if m == "beginRenderPass" {
                        "Render pass"
                    } else {
                        "Compute pass"
                    },
                    obj_label(cap, pass)
                )
            });
        queues[q].passes.push(Span {
            start_ns: s,
            end_ns: e,
            label,
            depth: 0,
            index: i,
        });
    }
    for (q, k) in keys.iter().enumerate() {
        if !matches!(k, QueueKey::Gl(_)) {
            continue;
        }
        let mut run: Option<(String, u64, u64, usize)> = None;
        let mut out = Vec::new();
        for s in spans.iter().filter(|s| s.queue == q) {
            let fb = data
                .gl_fb
                .get(&s.index)
                .cloned()
                .unwrap_or_else(|| "default".into());
            match &mut run {
                Some((l, _, e, _)) if *l == fb => *e = s.end,
                _ => {
                    if let Some((l, st, e, idx)) = run.take() {
                        out.push(Span {
                            start_ns: st,
                            end_ns: e,
                            label: l,
                            depth: 0,
                            index: idx,
                        });
                    }
                    run = Some((fb, s.start, s.end, s.index));
                }
            }
        }
        if let Some((l, st, e, idx)) = run {
            out.push(Span {
                start_ns: st,
                end_ns: e,
                label: l,
                depth: 0,
                index: idx,
            });
        }
        queues[q].passes = out;
    }

    // Markers.
    let total_end = spans.iter().map(|s| s.end).max().unwrap_or(0);
    let next_start = |q: usize, after: usize| {
        spans
            .iter()
            .filter(|s| s.queue == q && s.index > after)
            .map(|s| s.start)
            .next()
            .unwrap_or(total_end)
    };
    let range_span = |q: usize, from: usize, to: usize| {
        let mut it = spans
            .iter()
            .filter(|s| s.queue == q && s.index > from && s.index < to)
            .peekable();
        it.peek()?;
        let (mut a, mut b) = (u64::MAX, 0);
        for s in it {
            a = a.min(s.start);
            b = b.max(s.end);
        }
        Some((a, b))
    };
    let mut stacks: HashMap<ObjId, Vec<(String, usize)>> = HashMap::new();
    let close = |queues: &mut Vec<QueueTrack>,
                 q: usize,
                 label: String,
                 depth: usize,
                 push: usize,
                 pop: usize| {
        let (s, e) = range_span(q, push, pop).unwrap_or_else(|| {
            let t = next_start(q, pop);
            (t, t)
        });
        queues[q].markers.push(Span {
            start_ns: s,
            end_ns: e,
            label,
            depth: depth.min(255) as u8,
            index: push,
        });
    };
    let mut roots: HashMap<ObjId, usize> = HashMap::new();
    for (i, c) in cap.frame.iter().enumerate() {
        let m = cap.method(c);
        if !matches!(
            m,
            "pushDebugGroup"
                | "popDebugGroup"
                | "insertDebugMarker"
                | "webgd:pushMarker"
                | "webgd:popMarker"
                | "webgd:insertMarker"
        ) {
            continue;
        }
        let Some(qk) = queue_of_call(cap, c) else {
            continue;
        };
        let Some(q) = keys.iter().position(|k| *k == qk) else {
            continue;
        };
        let root = ancestor(cap, c.this, |cl| {
            cl == "GPUCommandEncoder" || cl == "WebGL2RenderingContext"
        })
        .unwrap_or(c.this);
        roots.insert(root, q);
        let label = match c.args.first() {
            Some(Value::String(s)) => cap.str(*s).to_string(),
            _ => String::new(),
        };
        let stack = stacks.entry(root).or_default();
        match m {
            "pushDebugGroup" | "webgd:pushMarker" => stack.push((label, i)),
            "popDebugGroup" | "webgd:popMarker" => {
                let depth = stack.len().saturating_sub(1);
                if let Some((l, push)) = stack.pop() {
                    close(&mut queues, q, l, depth, push, i);
                }
            }
            _ => {
                let depth = stack.len();
                let t = next_start(q, i);
                queues[q].markers.push(Span {
                    start_ns: t,
                    end_ns: t,
                    label,
                    depth: depth.min(255) as u8,
                    index: i,
                });
            }
        }
    }
    for (root, stack) in stacks {
        let q = roots.get(&root).copied().unwrap_or(0);
        for (depth, (l, push)) in stack.into_iter().enumerate() {
            close(&mut queues, q, l, depth, push, usize::MAX);
        }
    }

    // Counters.
    let counters = counters(cap, &spans, &items, &keys, data, frag_run, &mut notes);
    let total_ns = queues
        .iter()
        .flat_map(|q| {
            q.items
                .iter()
                .chain(q.passes.iter())
                .chain(q.markers.iter())
        })
        .map(|s| s.end_ns)
        .max()
        .unwrap_or(0);
    TraceResult {
        queues,
        counters,
        notes,
        total_ns,
    }
}

#[derive(Default)]
struct Shaders {
    vs: Option<StageStats>,
    fs: Option<StageStats>,
    cs: Option<StageStats>,
}

fn counters(
    cap: &Capture,
    spans: &[ItemSpan],
    items: &[(usize, CmdKind)],
    keys: &[QueueKey],
    data: &TraceData,
    frag_run: Option<&TraceData>,
    notes: &mut Vec<String>,
) -> Vec<CounterTrack> {
    let kinds: HashMap<usize, &CmdKind> = items.iter().map(|(i, k)| (*i, k)).collect();
    // Pipeline per GPU item (pass encoder → last setPipeline).
    let mut cur: HashMap<ObjId, ObjId> = HashMap::new();
    let mut pipeline_at: HashMap<usize, ObjId> = HashMap::new();
    for (i, c) in cap.frame.iter().enumerate() {
        if cap.method(c) == "setPipeline"
            && let Some(p) = c.args.first().and_then(Value::as_obj)
        {
            cur.insert(c.this, p);
        }
        if kinds.contains_key(&i)
            && let Some(p) = cur.get(&c.this)
        {
            pipeline_at.insert(i, *p);
        }
    }
    // Render bundles: pipeline (last set) and vertex invocations recorded into each bundle.
    let mut enc_state: HashMap<ObjId, (Option<ObjId>, f64)> = HashMap::new();
    let mut bundles: HashMap<ObjId, (Option<ObjId>, f64)> = HashMap::new();
    for c in cap.setup.iter().chain(cap.frame.iter()) {
        if cap.class_of(c.this) != "GPURenderBundleEncoder" {
            continue;
        }
        let a = |i: usize, d: f64| c.args.get(i).and_then(Value::as_f64).unwrap_or(d);
        let e = enc_state.entry(c.this).or_default();
        match cap.method(c) {
            "setPipeline" => e.0 = c.args.first().and_then(Value::as_obj),
            "draw" | "drawIndexed" => e.1 += a(0, 0.0) * a(1, 1.0),
            "finish" => {
                if let Some(b) = c.result.as_obj() {
                    bundles.insert(b, *e);
                }
            }
            _ => {}
        }
    }
    let bundle_list = |call: &Call| -> Vec<(Option<ObjId>, f64)> {
        call.args
            .first()
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|b| b.as_obj().and_then(|b| bundles.get(&b).copied()))
            .collect()
    };
    for (i, c) in cap.frame.iter().enumerate() {
        if cap.method(c) == "executeBundles"
            && let Some(p) = bundle_list(c).iter().find_map(|b| b.0)
        {
            pipeline_at.insert(i, p);
        }
    }
    let mut cache: HashMap<(String, Stage), Option<StageStats>> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    let mut wgsl = |module: ObjId,
                    entry: Option<&str>,
                    stage: Stage,
                    failures: &mut Vec<String>|
     -> Option<StageStats> {
        let key = (format!("{}:{}", module.0, entry.unwrap_or("")), stage);
        cache
            .entry(key)
            .or_insert_with(|| {
                let code = creation_desc(cap, module).and_then(|d| get_str(cap, d, "code"))?;
                match wgsl_stats(code, entry, stage) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        failures.push(format!("{} {stage:?}: {e}", obj_label(cap, module)));
                        None
                    }
                }
            })
            .clone()
    };
    let mut glsl_cache: HashMap<(String, Stage), Option<StageStats>> = HashMap::new();
    let mut glsl = |src: &str, stage: Stage, failures: &mut Vec<String>| -> Option<StageStats> {
        if src.is_empty() {
            return None;
        }
        glsl_cache
            .entry((src.to_string(), stage))
            .or_insert_with(|| match glsl_stats(src, stage) {
                Ok(s) => Some(s),
                Err(e) => {
                    failures.push(format!("GLSL {stage:?}: {e}"));
                    None
                }
            })
            .clone()
    };
    let stage_of = |d: &Value, key: &str| -> Option<(ObjId, Option<String>)> {
        let st = cap.dict_get(d, key)?;
        let module = cap.dict_get(st, "module")?.as_obj()?;
        Some((module, get_str(cap, st, "entryPoint").map(str::to_string)))
    };

    let mut occ_s = Vec::new();
    let mut alu_s = Vec::new();
    let mut br_s = Vec::new();
    let mut tex_s = Vec::new();
    let mut frag_gpu = Vec::new();
    let mut frag_gl = Vec::new();
    for s in spans {
        let call = &cap.frame[s.index];
        let kind = kinds
            .get(&s.index)
            .copied()
            .cloned()
            .unwrap_or(CmdKind::Call);
        let method = cap.method(call);
        let a = num_list_args(cap, call);
        let arg = |i: usize, d: f64| a.get(i).copied().flatten().unwrap_or(d);
        let is_gl = matches!(keys[s.queue], QueueKey::Gl(_));
        let mut sh = Shaders::default();
        if is_gl {
            if let Some((vs, fs)) = data.gl_src.get(&s.index) {
                sh.vs = glsl(vs, Stage::Vertex, &mut failures);
                sh.fs = glsl(fs, Stage::Fragment, &mut failures);
            }
        } else if let Some(desc) = pipeline_at
            .get(&s.index)
            .and_then(|p| creation_desc(cap, *p))
        {
            if let Some((m, e)) = stage_of(desc, "vertex") {
                sh.vs = wgsl(m, e.as_deref(), Stage::Vertex, &mut failures);
            }
            if let Some((m, e)) = stage_of(desc, "fragment") {
                sh.fs = wgsl(m, e.as_deref(), Stage::Fragment, &mut failures);
            }
            if let Some((m, e)) = stage_of(desc, "compute") {
                sh.cs = wgsl(m, e.as_deref(), Stage::Compute, &mut failures);
            }
        }
        let indirect = data.indirect.get(&s.index);
        let ind = |i: usize| indirect.and_then(|v| v.get(i)).map(|x| *x as f64);
        let dur = (s.end.saturating_sub(s.start)).max(1) as f64;
        let (occ, alu, br, tex, frags) = match kind {
            CmdKind::Draw => {
                let vinv = if is_gl {
                    match method {
                        "drawArrays" => arg(2, 0.0),
                        "drawElements" => arg(1, 0.0),
                        "drawArraysInstanced" => arg(2, 0.0) * arg(3, 1.0),
                        "drawElementsInstanced" => arg(1, 0.0) * arg(4, 1.0),
                        "drawRangeElements" => arg(3, 0.0),
                        _ => 0.0,
                    }
                } else {
                    match method {
                        "draw" | "drawIndexed" => arg(0, 0.0) * arg(1, 1.0),
                        "drawIndirect" | "drawIndexedIndirect" => {
                            ind(0).unwrap_or(0.0) * ind(1).unwrap_or(1.0)
                        }
                        "executeBundles" => bundle_list(call).iter().map(|b| b.1).sum(),
                        _ => 0.0,
                    }
                };
                let frags = if is_gl {
                    frag_run.and_then(|f| f.frags.get(&s.index)).copied()
                } else {
                    data.occ.get(&s.index).copied()
                };
                let fr = frags.unwrap_or(0.0);
                let v = sh.vs.clone().unwrap_or_default();
                let f = sh.fs.clone().unwrap_or_default();
                let occ = sh
                    .fs
                    .as_ref()
                    .or(sh.vs.as_ref())
                    .map(occupancy)
                    .unwrap_or(0.0);
                (
                    occ,
                    v.alu * vinv + f.alu * fr,
                    v.branches * vinv + f.branches * fr,
                    v.tex_samples * vinv + f.tex_samples * fr,
                    frags,
                )
            }
            CmdKind::Dispatch => {
                let groups = if method == "dispatchWorkgroupsIndirect" {
                    ind(0).unwrap_or(0.0) * ind(1).unwrap_or(1.0) * ind(2).unwrap_or(1.0)
                } else {
                    arg(0, 0.0) * arg(1, 1.0) * arg(2, 1.0)
                };
                let c = sh.cs.clone().unwrap_or_default();
                let wg = c
                    .workgroup_size
                    .map(|w| w.iter().map(|x| *x as f64).product::<f64>())
                    .unwrap_or(1.0);
                let threads = groups * wg;
                let occ = sh.cs.as_ref().map(occupancy).unwrap_or(0.0);
                (
                    occ,
                    c.alu * threads,
                    c.branches * threads,
                    c.tex_samples * threads,
                    None,
                )
            }
            _ => (0.0, 0.0, 0.0, 0.0, None),
        };
        let sample = |value: f64, total: f64| CounterSample {
            start_ns: s.start,
            end_ns: s.end,
            value,
            total,
        };
        occ_s.push(sample(occ * 100.0, occ * 100.0));
        alu_s.push(sample(alu / dur, alu));
        br_s.push(sample(br / dur, br));
        tex_s.push(sample(tex / dur, tex));
        if let Some(f) = frags {
            if is_gl {
                frag_gl.push(sample(f, f))
            } else {
                frag_gpu.push(sample(f, f))
            }
        }
    }
    if !frag_gpu.is_empty() && frag_gpu.iter().all(|s| s.total <= 1.0) {
        notes.push(
            "WebGPU occlusion results are visibility flags on this browser/GPU (0/1), not sample counts: \
             Fragments (post-depth) and fragment-stage estimates use that flag"
                .into(),
        );
    }
    failures.sort();
    failures.dedup();
    for f in failures {
        notes.push(format!("shader analysis n/a: {f}"));
    }
    let track = |name: &str, unit: &str, samples| CounterTrack {
        name: name.into(),
        unit: unit.into(),
        samples,
    };
    let mut out = vec![
        track("Occupancy (est.)", "%", occ_s),
        track("ALU ops/ns (est.)", "ops/ns", alu_s),
        track("Branches/ns (est.)", "branches/ns", br_s),
        track("Texture samples/ns (est.)", "samples/ns", tex_s),
    ];
    if !frag_gpu.is_empty() {
        out.push(track("Fragments (post-depth)", "fragments", frag_gpu));
    }
    if !frag_gl.is_empty() {
        out.push(track(
            "Fragments (rasterized, pre-depth)",
            "fragments",
            frag_gl,
        ));
    }
    out
}

/// Scalar numeric args (None for non-numbers).
fn num_list_args(cap: &Capture, call: &Call) -> Vec<Option<f64>> {
    call.args
        .iter()
        .map(|a| match a {
            Value::Number(_) | Value::Bool(_) => a.as_f64(),
            Value::Typed { .. } | Value::Array(_) => num_list(cap, a).first().copied(),
            _ => None,
        })
        .collect()
}
