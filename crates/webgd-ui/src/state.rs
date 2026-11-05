//! Shared app state passed to every panel.
use crate::port::{Inbox, PortClient};
use crate::replay_queue::{Key, ReplayQueue};
use std::collections::HashMap;
use std::rc::Rc;
use webgd_core::edits::{self, CaptureEdits, EditOrigin};
use webgd_core::track::commands::{CmdNode, build_tree};
use webgd_core::track::resources::{ResourceRow, list};
use webgd_core::track::revisions::{self, Revision};
use webgd_core::{Capture, ObjId};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
    Capture,
    Events,
    Log,
    RenderTargets,
    ApiInspector,
    Resources,
    Viewer,
    Geometry,
    Trace,
    Scrubber,
    Editor,
}

/// Top-level activity; each has its own dock layout.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Workspace {
    FrameDebugger,
    GpuTrace,
}

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Capture => "Capture",
            Tab::Events => "Events",
            Tab::Log => "Log",
            Tab::RenderTargets => "Render Targets",
            Tab::ApiInspector => "API Inspector",
            Tab::Resources => "Resources",
            Tab::Viewer => "Resource Viewer",
            Tab::Geometry => "Geometry",
            Tab::Trace => "GPU Trace",
            Tab::Scrubber => "Scrubber",
            Tab::Editor => "Command Editor",
        }
    }
}

/// A decoded capture with the user's edits applied, plus derived data that is constant until the
/// next load or edit.
pub struct Loaded {
    /// Edited capture (equal to `original` while there are no edits); everything else reads this.
    pub cap: Rc<Capture>,
    /// Capture as decoded from `bytes`.
    pub original: Rc<Capture>,
    /// Session-only edits against `original` (never written back to `bytes`).
    pub edits: CaptureEdits,
    /// Origin of every `cap.frame` command.
    pub origin: Vec<EditOrigin>,
    pub bytes: Rc<Vec<u8>>,
    /// Unique per load and per edit; keys every cache derived from the capture.
    pub generation: u64,
    pub source: String,
    pub tree: Vec<CmdNode>,
    pub labels: Vec<String>,
    pub resources: Vec<ResourceRow>,
    /// Revision history per resource (`webgd_core::track::revisions::all`).
    pub revisions: HashMap<ObjId, Vec<Revision>>,
}

impl Loaded {
    pub fn new(cap: Capture, bytes: Vec<u8>, source: String, generation: u64) -> Self {
        let original = Rc::new(cap);
        let origin = (0..original.frame.len())
            .map(EditOrigin::Original)
            .collect();
        Self::derive(
            original.clone(),
            original,
            CaptureEdits::default(),
            origin,
            Rc::new(bytes),
            source,
            generation,
        )
    }

    fn derive(
        cap: Rc<Capture>,
        original: Rc<Capture>,
        edits: CaptureEdits,
        origin: Vec<EditOrigin>,
        bytes: Rc<Vec<u8>>,
        source: String,
        generation: u64,
    ) -> Self {
        let tree = build_tree(&cap);
        let labels = cap
            .frame
            .iter()
            .map(|c| webgd_core::display::call_label(&cap, c))
            .collect();
        let resources = list(&cap);
        let revisions = revisions::all(&cap);
        Loaded {
            cap,
            original,
            edits,
            origin,
            bytes,
            generation,
            source,
            tree,
            labels,
            resources,
            revisions,
        }
    }

    /// Frame index the replay stops at for the current selection.
    pub fn stop(&self, selected: Option<usize>) -> Option<usize> {
        selected.or_else(|| self.cap.frame.len().checked_sub(1))
    }

    /// Origin of edited frame command `index` (`Original(index)` when out of range).
    pub fn origin_of(&self, index: usize) -> EditOrigin {
        self.origin
            .get(index)
            .copied()
            .unwrap_or(EditOrigin::Original(index))
    }

    /// Edited frame index of the command with origin `o` (original commands match whether or not
    /// they are modified).
    pub fn index_of(&self, o: EditOrigin) -> Option<usize> {
        index_of_origin(&self.origin, o)
    }

    /// Number of edited commands (modified + inserted).
    pub fn edit_count(&self) -> usize {
        self.edits.modified.len() + self.edits.inserted.values().map(Vec::len).sum::<usize>()
    }
}

/// Edited frame index of the command with origin `o` in `origin`.
pub fn index_of_origin(origin: &[EditOrigin], o: EditOrigin) -> Option<usize> {
    match o {
        EditOrigin::Original(i) | EditOrigin::Modified(i) => edits::edited_index_of(origin, i),
        ins @ EditOrigin::Inserted { .. } => origin.iter().position(|x| *x == ins),
    }
}

pub struct AppState {
    pub ctx: egui::Context,
    pub loaded: Option<Loaded>,
    pub selected: Option<usize>,
    /// Set when selection changed from outside the Events list (scroll it into view).
    pub reveal_selected: bool,
    /// Highlighted pre-frame setup call (`cap.setup` index). Setup calls are not replay stop points:
    /// selecting one shows its call in the Events list's "Pre-frame setup" group and leaves the replay at frame start.
    pub selected_setup: Option<usize>,
    /// Resource shown in the Resource Viewer.
    pub view: Option<ObjId>,
    pub focus_tab: Option<Tab>,
    pub status: String,
    pub workspace: Workspace,
    /// Latest successful GPU trace for the loaded capture (cleared on load).
    pub trace: Option<Rc<webgd_replay::TraceResult>>,
    pub port: PortClient,
    pub inbox: Inbox,
    pub replay: ReplayQueue,
    next_generation: u64,
}

impl AppState {
    pub fn new(ctx: egui::Context) -> Self {
        let inbox = Inbox::default();
        let port = PortClient::new(ctx.clone(), inbox.clone());
        AppState {
            replay: ReplayQueue::new(ctx.clone()),
            ctx,
            loaded: None,
            selected: None,
            reveal_selected: false,
            selected_setup: None,
            view: None,
            focus_tab: None,
            status: String::new(),
            workspace: Workspace::FrameDebugger,
            trace: None,
            port,
            inbox,
            next_generation: 1,
        }
    }

    /// Decodes captures delivered by the port or file picker.
    pub fn drain_inbox(&mut self) -> bool {
        let incoming: Vec<_> = self.inbox.borrow_mut().drain(..).collect();
        let mut changed = false;
        for inc in incoming {
            match webgd_core::wire::decode(&inc.bytes) {
                Ok(cap) => {
                    let generation = self.next_generation;
                    self.next_generation += 1;
                    self.status = format!(
                        "loaded {} ({} bytes): {} setup / {} snapshot / {} frame calls",
                        inc.source,
                        inc.bytes.len(),
                        cap.setup.len(),
                        cap.snapshot.len(),
                        cap.frame.len()
                    );
                    self.loaded = Some(Loaded::new(cap, inc.bytes, inc.source, generation));
                    self.selected = None;
                    self.selected_setup = None;
                    self.view = None;
                    self.trace = None;
                    self.focus_tab = Some(Tab::Events);
                    changed = true;
                }
                Err(e) => self.status = format!("failed to decode {}: {e}", inc.source),
            }
        }
        changed
    }

    pub fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.selected_setup = None;
        self.reveal_selected = true;
        self.focus_tab = Some(Tab::Events);
    }

    /// Selects a pre-frame setup call. Setup calls are not replay stop points; the replay shows the
    /// frame-start state (setup + snapshot applied, no frame calls), which reflects every setup call.
    pub fn select_setup(&mut self, index: usize) {
        self.selected_setup = Some(index);
        self.selected = None;
        self.reveal_selected = true;
        self.focus_tab = Some(Tab::Events);
    }

    /// Selects the API command a revision refers to (frame or pre-frame setup call).
    pub fn select_revision(&mut self, rev: &Revision) {
        match rev.section {
            webgd_core::Section::Frame => self.select(rev.index),
            _ => self.select_setup(rev.index),
        }
    }

    /// Replay stop for the current selection: the selected frame call, frame start while a
    /// pre-frame setup call is selected (or the frame is empty), else the last frame call.
    pub fn replay_key(&self) -> Option<Key> {
        let l = self.loaded.as_ref()?;
        let stop = if self.selected.is_none() && self.selected_setup.is_some() {
            None
        } else {
            l.stop(self.selected)
        };
        Some(Key {
            generation: l.generation,
            stop,
        })
    }

    pub fn open_resource(&mut self, id: ObjId) {
        self.view = Some(id);
        self.focus_tab = Some(Tab::Viewer);
    }

    /// Replaces the edits of the loaded capture: rebuilds the edited capture and its derived data
    /// under a new generation (so every replay/geometry/trace cache refreshes), drops the GPU
    /// trace, and keeps the same command selected. Panel state is kept.
    pub fn apply_edits(&mut self, edits: CaptureEdits) {
        let Some(old) = self.loaded.take() else {
            return;
        };
        let generation = self.next_generation;
        self.next_generation += 1;
        let sel_origin = self.selected.map(|i| old.origin_of(i));
        let Loaded {
            original,
            bytes,
            source,
            ..
        } = old;
        let (cap, origin) = if edits.is_empty() {
            let origin = (0..original.frame.len())
                .map(EditOrigin::Original)
                .collect();
            (original.clone(), origin)
        } else {
            let e = edits::apply(&original, &edits);
            (Rc::new(e.cap), e.origin)
        };
        let new = Loaded::derive(cap, original, edits, origin, bytes, source, generation);
        if let Some(o) = sel_origin {
            let index = new.index_of(o).or_else(|| match o {
                // A deleted inserted command: select what now sits at its insertion point.
                EditOrigin::Inserted { before, .. } => edits::edited_index_of(&new.origin, before)
                    .or_else(|| new.cap.frame.len().checked_sub(1)),
                _ => None,
            });
            if index != self.selected {
                self.reveal_selected = true;
            }
            self.selected = index;
        }
        self.trace = None;
        self.loaded = Some(new);
    }

    /// Selects the frame command with origin `o` (if present) without changing the focused tab.
    pub fn select_origin(&mut self, o: EditOrigin) {
        if let Some(i) = self.loaded.as_ref().and_then(|l| l.index_of(o)) {
            self.selected = Some(i);
            self.selected_setup = None;
            self.reveal_selected = true;
        }
    }
}
