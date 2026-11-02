//! Commands panel: frame call tree with collapsible groups, filter and keyboard navigation.
use crate::state::AppState;
use egui::{Align2, Color32, FontId, Key, Sense, TextStyle, vec2};
use std::collections::HashSet;
use webgd_core::track::commands::{CmdKind, CmdNode};

#[derive(Default)]
pub struct CommandsUi {
    filter: String,
    collapsed: HashSet<usize>,
    last_offset: f32,
    last_view_h: f32,
}

fn collapsible(n: &CmdNode) -> bool {
    matches!(n.kind, CmdKind::GroupBegin(_) | CmdKind::PassBegin) && n.span_end > n.index
}

impl CommandsUi {
    fn visible(&self, tree: &[CmdNode], labels: &[String]) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        if !needle.is_empty() {
            return (0..tree.len())
                .filter(|&i| labels[i].to_lowercase().contains(&needle))
                .collect();
        }
        let mut out = Vec::with_capacity(tree.len());
        let mut i = 0;
        while i < tree.len() {
            out.push(i);
            let n = &tree[i];
            i = if collapsible(n) && self.collapsed.contains(&i) {
                n.span_end + 1
            } else {
                i + 1
            };
        }
        out
    }

    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("label substring")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.horizontal(|ui| {
            ui.weak(format!("{} calls", l.tree.len()));
            if ui.small_button("Collapse all").clicked() {
                self.collapsed = l
                    .tree
                    .iter()
                    .filter(|n| collapsible(n))
                    .map(|n| n.index)
                    .collect();
            }
            if ui.small_button("Expand all").clicked() {
                self.collapsed.clear();
            }
        });
        let err_set = st.replay.results(|r| {
            let mut set = HashSet::new();
            if let Some((k, Ok(info))) = &r.replay
                && k.generation == l.generation
            {
                set.extend(info.errors.iter().map(|(i, _)| *i));
            }
            set
        });
        let visible = self.visible(&l.tree, &l.labels);
        let row_h = ui.text_style_height(&TextStyle::Monospace) + 4.0;
        let row_step = row_h + ui.spacing().item_spacing.y;

        // Keyboard navigation (only when no text field has focus).
        let no_focus = ui.ctx().memory(|m| m.focused().is_none());
        let mut moved = false;
        if no_focus && !visible.is_empty() {
            let (up, down) =
                ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
            if up || down {
                let pos = st
                    .selected
                    .and_then(|s| visible.iter().position(|&v| v >= s));
                let next = match (pos, down) {
                    (None, _) => 0,
                    (Some(p), true) => (p + 1).min(visible.len() - 1),
                    (Some(p), false) => p.saturating_sub(1),
                };
                st.selected = Some(visible[next]);
                moved = true;
            }
        }
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if (moved || st.reveal_selected)
            && let Some(sel) = st.selected
        {
            // Expand collapsed groups containing the selection when revealing from elsewhere.
            if st.reveal_selected {
                let enclosing: Vec<usize> = self
                    .collapsed
                    .iter()
                    .copied()
                    .filter(|&g| g < sel && l.tree[g].span_end >= sel)
                    .collect();
                for g in enclosing {
                    self.collapsed.remove(&g);
                }
            }
            let visible = self.visible(&l.tree, &l.labels);
            if let Some(p) = visible.iter().position(|&v| v == sel) {
                let y = p as f32 * row_step;
                if y < self.last_offset || y + row_step > self.last_offset + self.last_view_h {
                    scroll = scroll.vertical_scroll_offset((y - self.last_view_h * 0.5).max(0.0));
                }
            }
            st.reveal_selected = false;
        }
        let visible = self.visible(&l.tree, &l.labels);
        let font = FontId::monospace(ui.text_style_height(&TextStyle::Monospace) - 1.0);
        let mut toggle = None;
        let mut clicked = None;
        let out = scroll.show_rows(ui, row_h, visible.len(), |ui, range| {
            for &i in &visible[range] {
                let n = &l.tree[i];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let painter = ui.painter_at(rect);
                let selected = st.selected == Some(i);
                if selected {
                    painter.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                } else if resp.hovered() {
                    painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let weak = ui.visuals().weak_text_color();
                painter.text(
                    egui::pos2(rect.left() + 40.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    i.to_string(),
                    font.clone(),
                    weak,
                );
                let indent = rect.left() + 48.0 + n.depth as f32 * 14.0;
                let is_group = collapsible(n);
                if is_group {
                    let open = !self.collapsed.contains(&i);
                    painter.text(
                        egui::pos2(indent, rect.center().y),
                        Align2::LEFT_CENTER,
                        if open { "⏷" } else { "⏵" },
                        font.clone(),
                        ui.visuals().text_color(),
                    );
                }
                let err = err_set.contains(&i);
                let bold = matches!(n.kind, CmdKind::Draw | CmdKind::Dispatch);
                let color = if err {
                    Color32::from_rgb(255, 90, 90)
                } else if bold || selected {
                    ui.visuals().strong_text_color()
                } else if matches!(n.kind, CmdKind::GroupBegin(_)) {
                    Color32::from_rgb(140, 190, 255)
                } else {
                    ui.visuals().text_color()
                };
                // `Class#id.method(args)`: dimmed receiver prefix, then the call in the row color.
                let label = &l.labels[i];
                let (recv, call) = match label.split_once('.') {
                    Some((recv, _)) => label.split_at(recv.len()),
                    None => ("", label.as_str()),
                };
                let mut x = indent + 16.0;
                if let CmdKind::GroupBegin(name) = &n.kind {
                    let r = painter.text(
                        egui::pos2(x, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        font.clone(),
                        color,
                    );
                    x = r.right() + 8.0;
                }
                let r = painter.text(
                    egui::pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    recv,
                    font.clone(),
                    weak,
                );
                let tx = egui::pos2(r.right(), rect.center().y);
                painter.text(tx, Align2::LEFT_CENTER, call, font.clone(), color);
                if bold {
                    // Faux bold: overdraw shifted by a fraction of a pixel.
                    painter.text(
                        tx + vec2(0.7, 0.0),
                        Align2::LEFT_CENTER,
                        call,
                        font.clone(),
                        color,
                    );
                }
                let resp = resp.on_hover_text_at_pointer(&l.labels[i]);
                if resp.clicked() {
                    let on_toggle = resp
                        .interact_pointer_pos()
                        .is_some_and(|p| p.x < indent + 14.0);
                    if is_group && on_toggle {
                        toggle = Some(i);
                    } else {
                        clicked = Some(i);
                    }
                }
                if resp.double_clicked() && is_group {
                    toggle = Some(i);
                }
            }
        });
        self.last_offset = out.state.offset.y;
        self.last_view_h = out.inner_rect.height();
        if let Some(g) = toggle
            && !self.collapsed.remove(&g)
        {
            self.collapsed.insert(g);
        }
        if let Some(i) = clicked {
            st.selected = Some(i);
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }
}
