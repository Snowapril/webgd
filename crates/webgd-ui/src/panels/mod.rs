//! Dock panels.
mod capture;
mod commands;
mod inspect;
mod log;
mod resources;
mod trace;
pub mod viewer;

use crate::state::{AppState, Tab};

#[derive(Default)]
pub struct Panels {
    pub commands: commands::CommandsUi,
    pub targets: inspect::TargetsUi,
    pub resources: resources::ResourcesUi,
    pub viewer: viewer::ViewerUi,
    pub trace: trace::TraceUi,
}

impl Panels {
    /// Drops all capture-derived UI state (new capture loaded).
    pub fn reset(&mut self) {
        *self = Panels::default();
    }
}

pub fn show(tab: Tab, ui: &mut egui::Ui, st: &mut AppState, p: &mut Panels) {
    match tab {
        Tab::Capture => capture::show(ui, st),
        Tab::Commands => p.commands.show(ui, st),
        Tab::Log => log::show(ui, st),
        Tab::RenderTargets => p.targets.show(ui, st),
        Tab::Pipeline => inspect::show_pipeline(ui, st),
        Tab::Bindings => inspect::show_bindings(ui, st),
        Tab::Resources => p.resources.show(ui, st),
        Tab::Viewer => p.viewer.show(ui, st),
        Tab::Trace => p.trace.show(ui, st),
    }
}

/// Placeholder text for panels that need a loaded capture.
pub(crate) fn no_capture(ui: &mut egui::Ui) {
    ui.weak("No capture loaded. Capture a live target or open a .webgd file in the Capture tab.");
}
