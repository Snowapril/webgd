//! Inspection panels: Render Targets, Pipeline State, Bindings.
use crate::replay_queue::Key;
use crate::state::AppState;
use crate::widgets::{banner, default_range, load_image, obj_link};
use egui::{RichText, TextureHandle, TextureOptions};
use webgd_core::{Capture, ObjId};
use webgd_replay::{Inspection, StateNode};

/// Runs `f` on the inspection for the selected command, or shows why there is none.
/// Returns the resource the user clicked, if any.
fn with_inspection(
    ui: &mut egui::Ui,
    st: &AppState,
    f: impl FnOnce(&mut egui::Ui, &Capture, Key, &Inspection) -> Option<ObjId>,
) -> Option<ObjId> {
    let Some(l) = &st.loaded else {
        super::no_capture(ui);
        return None;
    };
    let Some(sel) = st.selected else {
        ui.weak("Select a command to inspect it.");
        return None;
    };
    let key = Key {
        generation: l.generation,
        stop: sel,
    };
    let cap = l.cap.clone();
    let out = st.replay.results(|r| match (&r.inspection, &r.replay) {
        (Some((k, insp)), _) if *k == key => Some(f(ui, &cap, key, insp)),
        (_, Some((k, Err(e)))) if *k == key => {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("replay failed: {e}"));
            Some(None)
        }
        _ => None,
    });
    match out {
        Some(clicked) => clicked,
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("replaying to #{sel}…"));
            });
            None
        }
    }
}

#[derive(Default)]
pub struct TargetsUi {
    cache: Option<(Key, Vec<Option<TextureHandle>>)>,
}

impl TargetsUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let cache = &mut self.cache;
        let clicked = with_inspection(ui, st, |ui, cap, key, insp| {
            if cache.as_ref().is_none_or(|(k, _)| *k != key) {
                let handles = insp
                    .targets
                    .iter()
                    .map(|t| {
                        t.image.as_ref().map(|img| {
                            load_image(
                                ui.ctx(),
                                img,
                                default_range(img),
                                [true; 4],
                                TextureOptions::LINEAR,
                            )
                        })
                    })
                    .collect();
                *cache = Some((key, handles));
            }
            let handles = &cache.as_ref().unwrap().1;
            for n in &insp.notes {
                banner(ui, n);
            }
            if insp.targets.is_empty() {
                ui.weak("No render targets bound for this command.");
            }
            let mut clicked = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (t, h) in insp.targets.iter().zip(handles) {
                            ui.vertical(|ui| {
                                ui.set_max_width(264.0);
                                ui.strong(&t.label);
                                if let Some(id) = t.resource
                                    && obj_link(ui, cap, id)
                                {
                                    clicked = Some(id);
                                }
                                if let (Some(h), Some(img)) = (h, &t.image) {
                                    ui.weak(format!(
                                        "{}×{} {:?}",
                                        img.width, img.height, img.format
                                    ));
                                    let s = 256.0 / img.width.max(img.height).max(1) as f32;
                                    let size =
                                        egui::vec2(img.width as f32 * s, img.height as f32 * s);
                                    let resp = ui.add(
                                        egui::Image::new((h.id(), size))
                                            .sense(egui::Sense::click()),
                                    );
                                    if resp.on_hover_text("Open in Resource Viewer").clicked() {
                                        clicked = t.resource;
                                    }
                                }
                                if let Some(n) = &t.note {
                                    banner(ui, n);
                                }
                            });
                        }
                    });
                });
            clicked
        });
        if let Some(id) = clicked {
            st.open_resource(id);
        }
    }
}

fn state_tree(ui: &mut egui::Ui, nodes: &[StateNode], depth: usize) {
    for n in nodes {
        match n {
            StateNode::Leaf { name, value } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{name}:"));
                    ui.label(RichText::new(value).monospace().strong());
                });
            }
            StateNode::Group { name, children } => {
                egui::CollapsingHeader::new(name)
                    .default_open(depth < 2)
                    .show(ui, |ui| state_tree(ui, children, depth + 1));
            }
        }
    }
}

pub fn show_pipeline(ui: &mut egui::Ui, st: &mut AppState) {
    with_inspection(ui, st, |ui, _cap, _key, insp| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if insp.pipeline.is_empty() {
                    ui.weak("No pipeline state for this command.");
                }
                state_tree(ui, &insp.pipeline, 0);
            });
        None
    });
}

pub fn show_bindings(ui: &mut egui::Ui, st: &mut AppState) {
    let clicked = with_inspection(ui, st, |ui, cap, _key, insp| {
        let mut clicked = None;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if insp.bindings.is_empty() {
                    ui.weak("No bindings for this command.");
                }
                let mut groups: Vec<&str> = Vec::new();
                for b in &insp.bindings {
                    if !groups.contains(&b.group.as_str()) {
                        groups.push(&b.group);
                    }
                }
                for g in groups {
                    egui::CollapsingHeader::new(g)
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new(("bindings", g))
                                .striped(true)
                                .num_columns(3)
                                .show(ui, |ui| {
                                    for b in insp.bindings.iter().filter(|b| b.group == g) {
                                        ui.label(RichText::new(&b.slot).monospace());
                                        match b.resource {
                                            Some(id) => {
                                                if obj_link(ui, cap, id) {
                                                    clicked = Some(id);
                                                }
                                            }
                                            None => {
                                                ui.weak("—");
                                            }
                                        }
                                        ui.label(&b.detail);
                                        ui.end_row();
                                    }
                                });
                        });
                }
            });
        clicked
    });
    if let Some(id) = clicked {
        st.open_resource(id);
    }
}
