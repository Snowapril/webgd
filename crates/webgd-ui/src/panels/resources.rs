//! Resources panel: every GPU resource with kind / text / written-in-frame filters and
//! per-resource revision counts.
use crate::state::AppState;
use egui::{Rect, RichText, Sense, TextStyle, vec2};
use webgd_core::track::resources::{Origin, RESOURCE_CLASSES};
use webgd_core::track::revisions::{Access, Revision};
use webgd_core::{ObjId, Section};

#[derive(Default)]
pub struct ResourcesUi {
    kind: Option<&'static str>,
    filter: String,
    /// Only resources with at least one Write event in the frame section.
    written_in_frame: bool,
}

const COLS: [(&str, f32); 7] = [
    ("ID", 52.0),
    ("Type", 150.0),
    ("Name/label", 150.0),
    ("Summary", 230.0),
    ("Origin", 80.0),
    ("Revisions", 66.0),
    ("Last write", 90.0),
];

/// Write-event statistics of one resource history.
struct WriteStats<'a> {
    count: usize,
    last: Option<&'a Revision>,
    in_frame: bool,
}

fn write_stats(history: &[Revision]) -> WriteStats<'_> {
    let mut s = WriteStats {
        count: 0,
        last: None,
        in_frame: false,
    };
    for r in history.iter().filter(|r| r.access == Access::Write) {
        s.count += 1;
        s.last = Some(r);
        s.in_frame |= r.section == Section::Frame;
    }
    s
}

/// Deferred click, applied after the table borrow of `st.loaded` ends.
enum Action {
    Open(ObjId),
    OpenAndSelect(ObjId, Revision),
    Select(usize),
    SelectRevision(Revision),
}

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
            ui.toggle_value(&mut self.written_in_frame, "Written in frame")
                .on_hover_text("Only resources modified by a frame command");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter")
                    .desired_width(f32::INFINITY),
            );
        });
        let needle = self.filter.to_lowercase();
        let empty: &[Revision] = &[];
        let history = |id: ObjId| l.revisions.get(&id).map(Vec::as_slice).unwrap_or(empty);
        let rows: Vec<usize> = l
            .resources
            .iter()
            .enumerate()
            .filter(|(_, r)| self.kind.is_none_or(|k| r.class == k))
            .filter(|(_, r)| !self.written_in_frame || write_stats(history(r.id)).in_frame)
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
        let mut action = None;
        let total_w: f32 =
            COLS.iter().map(|c| c.1).sum::<f32>() + ui.spacing().item_spacing.x * COLS.len() as f32;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            let row_h = ui.text_style_height(&TextStyle::Body) + 2.0;
            ui.horizontal(|ui| {
                for (name, w) in COLS {
                    cell(ui, w, row_h, egui::Label::new(RichText::new(name).strong()));
                }
            });
            ui.separator();
            let spacing_y = ui.spacing().item_spacing.y;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    let first = range.start;
                    for (n, &ri) in rows[range].iter().enumerate() {
                        let r = &l.resources[ri];
                        let ws = write_stats(history(r.id));
                        let selected = st.view == Some(r.id);
                        let bg =
                            Rect::from_min_size(ui.cursor().min, vec2(total_w, row_h + spacing_y));
                        // Registered before the cells so their links stay clickable on top.
                        let resp = ui.interact(bg, ui.id().with(("res-row", r.id)), Sense::click());
                        if selected {
                            ui.painter().rect_filled(
                                bg,
                                0.0,
                                ui.visuals().selection.bg_fill.gamma_multiply(0.45),
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(
                                bg,
                                0.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        } else if (first + n) % 2 == 1 {
                            ui.painter()
                                .rect_filled(bg, 0.0, ui.visuals().faint_bg_color);
                        }
                        ui.horizontal(|ui| {
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
                            cell(ui, COLS[2].1, row_h, label(RichText::new(&r.label)));
                            cell(
                                ui,
                                COLS[3].1,
                                row_h,
                                label(RichText::new(&r.summary).monospace()),
                            );
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
                                    if cell(ui, COLS[4].1, row_h, egui::Link::new(format!("#{i}")))
                                        .on_hover_text(format!("created at command #{i}"))
                                        .clicked()
                                    {
                                        action = Some(Action::Select(i));
                                    }
                                }
                            }
                            let count = RichText::new(ws.count.to_string()).monospace();
                            cell(
                                ui,
                                COLS[5].1,
                                row_h,
                                label(if ws.count == 0 { count.weak() } else { count }),
                            );
                            match ws.last {
                                Some(w) if w.section == Section::Frame => {
                                    if cell(
                                        ui,
                                        COLS[6].1,
                                        row_h,
                                        egui::Link::new(format!("#{}", w.index)),
                                    )
                                    .on_hover_text(&w.detail)
                                    .clicked()
                                    {
                                        action = Some(Action::Select(w.index));
                                    }
                                }
                                Some(w) => {
                                    if cell(
                                        ui,
                                        COLS[6].1,
                                        row_h,
                                        egui::Link::new(
                                            RichText::new(format!("pre-frame #{}", w.index)).weak(),
                                        ),
                                    )
                                    .on_hover_text(&w.detail)
                                    .clicked()
                                    {
                                        action = Some(Action::SelectRevision(w.clone()));
                                    }
                                }
                                None => {
                                    cell(ui, COLS[6].1, row_h, label(RichText::new("—").weak()));
                                }
                            }
                        });
                        if resp.double_clicked() {
                            action = Some(match ws.last {
                                Some(w) => Action::OpenAndSelect(r.id, w.clone()),
                                None => Action::Open(r.id),
                            });
                        } else if resp.clicked() && action.is_none() {
                            action = Some(Action::Open(r.id));
                        }
                    }
                });
        });
        match action {
            Some(Action::Open(id)) => st.open_resource(id),
            Some(Action::OpenAndSelect(id, rev)) => {
                st.select_revision(&rev);
                st.open_resource(id);
            }
            Some(Action::Select(i)) => st.select(i),
            Some(Action::SelectRevision(rev)) => st.select_revision(&rev),
            None => {}
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
