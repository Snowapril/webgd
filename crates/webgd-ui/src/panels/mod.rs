//! Dock panels.
mod capture;
mod commands;
mod inspect;
mod log;
mod resources;
mod scrubber;
mod trace;
pub mod viewer;

use crate::state::{AppState, Tab};

#[derive(Default)]
pub struct Panels {
    pub events: commands::CommandsUi,
    pub scrubber: scrubber::ScrubberUi,
    pub targets: inspect::TargetsUi,
    pub inspector: inspect::InspectorUi,
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
        Tab::Events => p.events.show(ui, st),
        Tab::Log => log::show(ui, st),
        Tab::RenderTargets => p.targets.show(ui, st),
        Tab::ApiInspector => p.inspector.show(ui, st),
        Tab::Resources => p.resources.show(ui, st),
        Tab::Viewer => p.viewer.show(ui, st),
        Tab::Trace => p.trace.show(ui, st),
        Tab::Scrubber => p.scrubber.show(ui, st),
    }
}

/// Placeholder text for panels that need a loaded capture.
pub(crate) fn no_capture(ui: &mut egui::Ui) {
    ui.weak("No capture loaded. Use File > Open .webgd… or capture a live target from the Capture menu.");
}
