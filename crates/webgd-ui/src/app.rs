//! eframe app: dock layout + per-frame replay request.
use crate::panels::{self, Panels};
use crate::replay_queue::{Key, ViewReq};
use crate::state::{AppState, Tab};
use egui_dock::{DockArea, DockState, NodeIndex, Style};

pub struct App {
    dock: DockState<Tab>,
    state: AppState,
    panels: Panels,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut dock = DockState::new(vec![Tab::RenderTargets, Tab::Viewer, Tab::Trace]);
        let surface = dock.main_surface_mut();
        let [center, _left] =
            surface.split_left(NodeIndex::root(), 0.27, vec![Tab::Capture, Tab::Commands]);
        let [center, _right] = surface.split_right(
            center,
            0.55,
            vec![Tab::Pipeline, Tab::Bindings, Tab::Resources],
        );
        surface.split_below(center, 0.75, vec![Tab::Log]);
        App {
            dock,
            state: AppState::new(cc.egui_ctx.clone()),
            panels: Panels::default(),
        }
    }

    /// Submits the replay needed by the current selection / viewer (latest wins).
    fn schedule_replay(&mut self) {
        let Some(l) = &self.state.loaded else { return };
        let Some(stop) = l.stop(self.state.selected) else {
            return;
        };
        let key = Key {
            generation: l.generation,
            stop,
        };
        let view: Option<ViewReq> = self.panels.viewer.wanted_read(l, self.state.view);
        let cap = l.cap.clone();
        self.state
            .replay
            .ensure(&cap, key, self.state.selected, view);
    }
}

struct Viewer<'a> {
    state: &'a mut AppState,
    panels: &'a mut Panels,
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }
    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        panels::show(*tab, ui, self.state, self.panels);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.state.drain_inbox() {
            self.panels.reset();
        }
        self.schedule_replay();
        DockArea::new(&mut self.dock)
            .style(Style::from_egui(ui.style().as_ref()))
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_inside(
                ui,
                &mut Viewer {
                    state: &mut self.state,
                    panels: &mut self.panels,
                },
            );
        if let Some(tab) = self.state.focus_tab.take()
            && let Some(path) = self.dock.find_tab(&tab)
        {
            let _ = self.dock.set_active_tab(path);
        }
        // Selection/view changes made by panels this frame: request their replay right away.
        self.schedule_replay();
    }
}
