//! Shared app state passed to every panel.
use crate::port::{Inbox, PortClient};
use crate::replay_queue::ReplayQueue;
use std::rc::Rc;
use webgd_core::track::commands::{CmdNode, build_tree};
use webgd_core::track::resources::{ResourceRow, list};
use webgd_core::{Capture, ObjId};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
    Capture,
    Commands,
    Log,
    RenderTargets,
    Pipeline,
    Bindings,
    Resources,
    Viewer,
    Trace,
}

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Capture => "Capture",
            Tab::Commands => "Commands",
            Tab::Log => "Log",
            Tab::RenderTargets => "Render Targets",
            Tab::Pipeline => "Pipeline State",
            Tab::Bindings => "Bindings",
            Tab::Resources => "Resources",
            Tab::Viewer => "Resource Viewer",
            Tab::Trace => "Trace",
        }
    }
}

/// A decoded capture plus derived, capture-constant data.
pub struct Loaded {
    pub cap: Rc<Capture>,
    pub bytes: Rc<Vec<u8>>,
    /// Unique per load; keys every cache derived from the capture.
    pub generation: u64,
    pub source: String,
    pub tree: Vec<CmdNode>,
    pub labels: Vec<String>,
    pub resources: Vec<ResourceRow>,
}

impl Loaded {
    pub fn new(cap: Capture, bytes: Vec<u8>, source: String, generation: u64) -> Self {
        let tree = build_tree(&cap);
        let labels = cap
            .frame
            .iter()
            .map(|c| webgd_core::display::call_label(&cap, c))
            .collect();
        let resources = list(&cap);
        Loaded {
            cap: Rc::new(cap),
            bytes: Rc::new(bytes),
            generation,
            source,
            tree,
            labels,
            resources,
        }
    }

    /// Frame index the replay stops at for the current selection.
    pub fn stop(&self, selected: Option<usize>) -> Option<usize> {
        selected.or_else(|| self.cap.frame.len().checked_sub(1))
    }
}

pub struct AppState {
    pub ctx: egui::Context,
    pub loaded: Option<Loaded>,
    pub selected: Option<usize>,
    /// Set when selection changed from outside the Commands list (scroll it into view).
    pub reveal_selected: bool,
    /// Resource shown in the Resource Viewer.
    pub view: Option<ObjId>,
    pub focus_tab: Option<Tab>,
    pub status: String,
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
            view: None,
            focus_tab: None,
            status: String::new(),
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
                    self.view = None;
                    self.focus_tab = Some(Tab::Commands);
                    changed = true;
                }
                Err(e) => self.status = format!("failed to decode {}: {e}", inc.source),
            }
        }
        changed
    }

    pub fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.reveal_selected = true;
        self.focus_tab = Some(Tab::Commands);
    }

    pub fn open_resource(&mut self, id: ObjId) {
        self.view = Some(id);
        self.focus_tab = Some(Tab::Viewer);
    }
}
