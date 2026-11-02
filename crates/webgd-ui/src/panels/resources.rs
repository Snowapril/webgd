//! Resources panel: every GPU resource with kind + text filters.
use crate::state::AppState;
use egui::{RichText, Sense, TextStyle};
use webgd_core::track::resources::{Origin, RESOURCE_CLASSES};

#[derive(Default)]
pub struct ResourcesUi {
    kind: Option<&'static str>,
    filter: String,
}

const COLS: [(&str, f32); 5] = [
    ("id", 48.0),
    ("class", 150.0),
    ("summary", 220.0),
    ("label", 150.0),
    ("origin", 90.0),
];

impl ResourcesUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("res-kind")
                .selected_text(self.kind.unwrap_or("All kinds"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.kind, None, "All kinds");
                    for c in RESOURCE_CLASSES {
                        if l.resources.iter().any(|r| r.class == *c) {
                            ui.selectable_value(&mut self.kind, Some(*c), *c);
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter")
                    .desired_width(f32::INFINITY),
            );
        });
        let needle = self.filter.to_lowercase();
        let rows: Vec<usize> = l
            .resources
            .iter()
            .enumerate()
            .filter(|(_, r)| self.kind.is_none_or(|k| r.class == k))
            .filter(|(_, r)| {
                needle.is_empty()
                    || format!("#{} {} {} {}", r.id.0, r.class, r.label, r.summary)
                        .to_lowercase()
                        .contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        ui.weak(format!("{} of {} resources", rows.len(), l.resources.len()));
        ui.separator();
        let mut open = None;
        let mut select = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (name, w) in COLS {
                    cell(ui, w, 16.0, egui::Label::new(RichText::new(name).strong()));
                }
            });
            let row_h = ui.text_style_height(&TextStyle::Body) + 4.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for &ri in &rows[range] {
                        let r = &l.resources[ri];
                        let selected = st.view == Some(r.id);
                        let resp = ui
                            .horizontal(|ui| {
                                ui.set_height(row_h);
                                let label =
                                    |t: RichText| egui::Label::new(t).truncate().selectable(false);
                                cell(
                                    ui,
                                    COLS[0].1,
                                    row_h,
                                    label(RichText::new(format!("#{}", r.id.0)).monospace()),
                                );
                                cell(ui, COLS[1].1, row_h, label(RichText::new(&r.class)));
                                cell(
                                    ui,
                                    COLS[2].1,
                                    row_h,
                                    label(RichText::new(&r.summary).monospace()),
                                );
                                cell(ui, COLS[3].1, row_h, label(RichText::new(&r.label)));
                                match r.origin {
                                    Origin::PreFrame => {
                                        cell(
                                            ui,
                                            COLS[4].1,
                                            row_h,
                                            label(RichText::new("pre-frame").weak()),
                                        );
                                    }
                                    Origin::Frame(i) => {
                                        if cell(
                                            ui,
                                            COLS[4].1,
                                            row_h,
                                            egui::Link::new(format!("frame #{i}")),
                                        )
                                        .clicked()
                                        {
                                            select = Some(i);
                                        }
                                    }
                                }
                            })
                            .response;
                        let resp = resp.interact(Sense::click());
                        if selected {
                            ui.painter().rect_stroke(
                                resp.rect,
                                2.0,
                                ui.visuals().selection.stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        if resp.clicked() {
                            open = Some(r.id);
                        }
                    }
                });
        });
        if let Some(i) = select {
            st.select(i);
        }
        if let Some(id) = open {
            st.open_resource(id);
        }
    }
}

/// Fixed-width, left-aligned table cell.
fn cell(ui: &mut egui::Ui, w: f32, h: f32, widget: impl egui::Widget) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(w);
            ui.add(widget)
        },
    )
    .inner
}
