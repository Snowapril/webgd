//! Latest-wins replay scheduling. One `spawn_local` loop owns the live `ReplayState`,
//! replays to the requested stop (a frame command or frame start), then runs inspection and
//! resource reads.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use webgd_core::pixels::Image;
use webgd_core::{Capture, ObjId};
use webgd_replay::{Inspection, ProgramInfo, ReplayMode, ReplayState, Replayer};

/// Identifies one replay: capture generation + stop point.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub generation: u64,
    /// Inclusive frame-call index the replay stops at; `None` = frame start (setup + snapshot
    /// applied, no frame calls).
    pub stop: Option<usize>,
}

impl Key {
    /// `#idx` for command stops, `frame start` otherwise.
    pub fn label(&self) -> String {
        match self.stop {
            Some(i) => format!("#{i}"),
            None => "frame start".to_owned(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ViewReq {
    Texture { id: ObjId, mip: u32, layer: u32 },
    Buffer(ObjId),
    Shader(ObjId),
    Program(ObjId),
}

pub enum ViewData {
    Texture(Image),
    Buffer(Vec<u8>),
    ShaderLog(Option<String>),
    Program(Option<ProgramInfo>),
}

pub struct ReplayInfo {
    pub errors: Vec<(usize, String)>,
    pub notes: HashMap<ObjId, Vec<String>>,
}

#[derive(Default)]
pub struct Results {
    /// Latest finished replay (errors + notes) or the replay failure.
    pub replay: Option<(Key, Result<ReplayInfo, String>)>,
    pub inspection: Option<(Key, Inspection)>,
    /// Latest view read, possibly another key than the current request.
    pub view: Option<(Key, ViewReq, Result<ViewData, String>)>,
}

struct Request {
    cap: Rc<Capture>,
    key: Key,
    inspect: Option<usize>,
    view: Option<ViewReq>,
}

pub type ReplayerCell = Rc<RefCell<Option<Rc<Replayer>>>>;

#[derive(Default)]
struct Shared {
    pending: Option<Request>,
    running: bool,
    results: Results,
    held: Option<(Key, ReplayState)>,
    /// Key of the request currently being processed.
    active: Option<Key>,
}

pub struct ReplayQueue {
    ctx: egui::Context,
    shared: Rc<RefCell<Shared>>,
    replayer: ReplayerCell,
    last_submitted: Option<(Key, Option<usize>, Option<ViewReq>)>,
}

/// Shared `Replayer` (one adapter request per UI session).
pub async fn get_replayer(cell: &ReplayerCell) -> Rc<Replayer> {
    if let Some(r) = cell.borrow().clone() {
        return r;
    }
    let r = Rc::new(Replayer::new().await);
    cell.borrow_mut().get_or_insert(r).clone()
}

impl ReplayQueue {
    pub fn new(ctx: egui::Context) -> Self {
        ReplayQueue {
            ctx,
            shared: Rc::default(),
            replayer: Rc::default(),
            last_submitted: None,
        }
    }

    pub fn replayer_cell(&self) -> ReplayerCell {
        self.replayer.clone()
    }

    /// Requests replay to `key.stop` (plus inspection / view read). No-op if identical to the last
    /// request. `inspect` must be `None` for frame-start keys.
    pub fn ensure(
        &mut self,
        cap: &Rc<Capture>,
        key: Key,
        inspect: Option<usize>,
        view: Option<ViewReq>,
    ) {
        let want = (key, inspect, view);
        if self.last_submitted == Some(want) {
            return;
        }
        self.last_submitted = Some(want);
        let mut sh = self.shared.borrow_mut();
        sh.pending = Some(Request {
            cap: cap.clone(),
            key,
            inspect,
            view,
        });
        if !sh.running {
            sh.running = true;
            drop(sh);
            let (shared, cell, ctx) =
                (self.shared.clone(), self.replayer.clone(), self.ctx.clone());
            wasm_bindgen_futures::spawn_local(run(shared, cell, ctx));
        }
    }

    pub fn results<R>(&self, f: impl FnOnce(&Results) -> R) -> R {
        f(&self.shared.borrow().results)
    }

    /// Key currently being replayed/inspected, if any.
    pub fn busy(&self) -> Option<Key> {
        let sh = self.shared.borrow();
        if sh.running {
            sh.active.or(sh.pending.as_ref().map(|p| p.key))
        } else {
            None
        }
    }
}

async fn run(shared: Rc<RefCell<Shared>>, cell: ReplayerCell, ctx: egui::Context) {
    let rep = get_replayer(&cell).await;
    loop {
        let Some(req) = shared.borrow_mut().pending.take() else {
            break;
        };
        shared.borrow_mut().active = Some(req.key);
        let held = shared.borrow_mut().held.take();
        let state = match held {
            Some((k, st)) if k == req.key => st,
            other => {
                if let Some((_, old)) = other {
                    old.dispose();
                }
                let replayed = match req.key.stop {
                    Some(stop) => rep.replay_to(&req.cap, stop, ReplayMode::Inspect).await,
                    None => rep.replay_to_frame_start(&req.cap).await,
                };
                match replayed {
                    Ok(st) => {
                        let info = ReplayInfo {
                            errors: st.errors.clone(),
                            notes: st.notes.clone(),
                        };
                        shared.borrow_mut().results.replay = Some((req.key, Ok(info)));
                        st
                    }
                    Err(e) => {
                        shared.borrow_mut().results.replay = Some((req.key, Err(e.to_string())));
                        ctx.request_repaint();
                        continue;
                    }
                }
            }
        };
        ctx.request_repaint();
        let superseded = |sh: &Rc<RefCell<Shared>>| {
            sh.borrow()
                .pending
                .as_ref()
                .is_some_and(|p| p.key != req.key)
        };
        if let Some(index) = req.inspect
            && !superseded(&shared)
            && shared
                .borrow()
                .results
                .inspection
                .as_ref()
                .is_none_or(|(k, _)| *k != req.key)
        {
            let insp = state.inspect(&req.cap, index).await;
            shared.borrow_mut().results.inspection = Some((req.key, insp));
            ctx.request_repaint();
        }
        if let Some(view) = req.view
            && !superseded(&shared)
            && shared
                .borrow()
                .results
                .view
                .as_ref()
                .is_none_or(|(k, v, _)| *k != req.key || *v != view)
        {
            let data = match view {
                ViewReq::Texture { id, mip, layer } => state
                    .read_texture(&req.cap, id, mip, layer)
                    .await
                    .map(ViewData::Texture)
                    .map_err(|e| e.to_string()),
                ViewReq::Buffer(id) => state
                    .read_buffer(&req.cap, id)
                    .await
                    .map(ViewData::Buffer)
                    .map_err(|e| e.to_string()),
                ViewReq::Shader(id) => Ok(ViewData::ShaderLog(state.shader_info_log(id))),
                ViewReq::Program(id) => Ok(ViewData::Program(state.program_info(id))),
            };
            shared.borrow_mut().results.view = Some((req.key, view, data));
            ctx.request_repaint();
        }
        shared.borrow_mut().held = Some((req.key, state));
    }
    let mut sh = shared.borrow_mut();
    sh.running = false;
    sh.active = None;
    drop(sh);
    ctx.request_repaint();
}
