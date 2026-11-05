//! Events panel: dense, virtualized frame event list with hierarchy, kind strip and GPU duration
//! column, a collapsible "Pre-frame setup" group, filtering and keyboard navigation.
use super::scrubber::{is_action, item_durations, trace_key};
use crate::state::{AppState, Loaded, index_of_origin};
use crate::widgets::{ERROR, fmt_ns, kind_color, marker_color, origin_style, text_on};
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Stroke, TextStyle, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use webgd_core::edits::EditOrigin;
use webgd_core::track::commands::{CmdKind, CmdNode};

/// Horizontal indent per hierarchy level.
const INDENT: f32 = 14.0;
/// Text colors on a selected row (accent fill): high contrast for every run, including the
/// receiver and index that are dimmed on other rows.
const SEL_TEXT: Color32 = Color32::from_gray(250);
const SEL_DIM: Color32 = Color32::from_gray(228);
const SEL_ERROR: Color32 = Color32::from_rgb(255, 205, 205);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    SetupHeader,
    Setup(usize),
    Frame(usize),
}

/// Capture-constant data derived once per load (and per edit).
struct Derived {
    generation: u64,
    /// Unedited capture and the origin of every row; carry collapsed groups across edits.
    original: Rc<webgd_core::Capture>,
    origin: Vec<EditOrigin>,
    frame_lower: Vec<String>,
    /// Number of actions in `tree[..k]`.
    act_prefix: Vec<u32>,
    /// Marker/debug-group nesting depth per call (passes excluded); keys `marker_color` like the
    /// scrubber and trace bands.
    marker_depth: Vec<u16>,
    /// Pre-frame setup labels and their lowercase forms; built when the group is first opened.
    setup: Option<(Vec<String>, Vec<String>)>,
}

impl Derived {
    fn new(l: &Loaded) -> Self {
        let mut act_prefix = Vec::with_capacity(l.tree.len() + 1);
        let mut a = 0u32;
        for n in &l.tree {
            act_prefix.push(a);
            if is_action(&n.kind) {
                a += 1;
            }
        }
        act_prefix.push(a);
        let mut marker_depth = Vec::with_capacity(l.tree.len());
        let mut open: Vec<usize> = Vec::new();
        for n in &l.tree {
            while open.last().is_some_and(|&e| e < n.index) {
                open.pop();
            }
            marker_depth.push(open.len() as u16);
            if matches!(n.kind, CmdKind::GroupBegin(_)) && n.span_end > n.index {
                open.push(n.span_end);
            }
        }
        Derived {
            generation: l.generation,
            original: l.original.clone(),
            origin: l.origin.clone(),
            frame_lower: l.labels.iter().map(|s| s.to_lowercase()).collect(),
            act_prefix,
            marker_depth,
            setup: None,
        }
    }

    /// Actions in `begin..=end`.
    fn actions_in(&self, begin: usize, end: usize) -> u32 {
        let end = end.min(self.act_prefix.len() - 2);
        self.act_prefix[end + 1] - self.act_prefix[begin]
    }
}

/// GPU durations from `st.trace`, keyed by (generation, trace identity).
struct Durations {
    key: (u64, usize),
    per_item: HashMap<usize, u64>,
    /// Sum of item durations in `frame[..k]`.
    prefix: Vec<u64>,
}

impl Durations {
    fn span(&self, begin: usize, end: usize) -> u64 {
        let end = end.min(self.prefix.len() - 2);
        self.prefix[end + 1] - self.prefix[begin]
    }
}

#[derive(Default)]
pub struct CommandsUi {
    filter: String,
    collapsed: HashSet<usize>,
    actions_only: bool,
    setup_open: bool,
    last_setup: Option<usize>,
    last_offset: f32,
    last_view_h: f32,
    derived: Option<Derived>,
    durations: Option<Durations>,
}

fn collapsible(n: &CmdNode) -> bool {
    matches!(n.kind, CmdKind::GroupBegin(_) | CmdKind::PassBegin) && n.span_end > n.index
}

impl CommandsUi {
    fn ensure_setup_labels(&mut self, l: &Loaded) {
        if let Some(d) = &mut self.derived
            && d.setup.is_none()
        {
            let labels: Vec<String> = l
                .cap
                .setup
                .iter()
                .map(|c| webgd_core::display::call_label(&l.cap, c))
                .collect();
            let lower = labels.iter().map(|s| s.to_lowercase()).collect();
            d.setup = Some((labels, lower));
        }
    }

    /// Rows in display order. `pin` keeps the selected frame call visible under `Actions only`.
    fn visible(&self, l: &Loaded, pin: Option<usize>) -> Vec<Row> {
        let Some(d) = &self.derived else {
            return Vec::new();
        };
        let needle = self.filter.to_lowercase();
        let tree = &l.tree;
        let mut out = Vec::with_capacity(tree.len() + 1);
        if !l.cap.setup.is_empty() {
            out.push(Row::SetupHeader);
            if self.setup_open
                && let Some((_, lower)) = &d.setup
            {
                out.extend(
                    (0..lower.len())
                        .filter(|&k| needle.is_empty() || lower[k].contains(&needle))
                        .map(Row::Setup),
                );
            }
        }
        if !needle.is_empty() {
            out.extend(
                (0..tree.len())
                    .filter(|&i| {
                        (!self.actions_only || is_action(&tree[i].kind))
                            && d.frame_lower[i].contains(&needle)
                    })
                    .map(Row::Frame),
            );
            return out;
        }
        let mut i = 0;
        while i < tree.len() {
            let n = &tree[i];
            let group = collapsible(n);
            let end = n.span_end.max(i);
            if self.actions_only {
                let pinned = pin.is_some_and(|p| p >= i && p <= end);
                if group && !pinned && d.actions_in(i, end) == 0 {
                    i = end + 1;
                    continue;
                }
                if !group && !is_action(&n.kind) && pin != Some(i) {
                    i += 1;
                    continue;
                }
            }
            out.push(Row::Frame(i));
            i = if group && self.collapsed.contains(&i) {
                end + 1
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
        if self
            .derived
            .as_ref()
            .is_none_or(|d| d.generation != l.generation)
        {
            let mut d = Derived::new(l);
            match self.derived.take() {
                // Same capture with different edits: keep groups collapsed by command identity
                // and reuse the (unedited) pre-frame setup labels.
                Some(old) if Rc::ptr_eq(&old.original, &l.original) => {
                    self.collapsed = self
                        .collapsed
                        .iter()
                        .filter_map(|&g| {
                            let o = *old.origin.get(g)?;
                            index_of_origin(&l.origin, o)
                        })
                        .collect();
                    d.setup = old.setup;
                }
                _ => {
                    self.collapsed.clear();
                    self.setup_open = false;
                    self.last_setup = None;
                }
            }
            self.derived = Some(d);
        }
        let tkey = (l.generation, trace_key(st));
        match st.trace.as_deref() {
            None => self.durations = None,
            Some(t) => {
                if self.durations.as_ref().is_none_or(|d| d.key != tkey) {
                    let per_item = item_durations(t);
                    let mut prefix = Vec::with_capacity(l.tree.len() + 1);
                    let mut acc = 0u64;
                    for k in 0..l.tree.len() {
                        prefix.push(acc);
                        acc += per_item.get(&k).copied().unwrap_or(0);
                    }
                    prefix.push(acc);
                    self.durations = Some(Durations {
                        key: tkey,
                        per_item,
                        prefix,
                    });
                }
            }
        }

        let errors: HashMap<usize, String> = st.replay.results(|r| {
            let mut m: HashMap<usize, String> = HashMap::new();
            if let Some((k, Ok(info))) = &r.replay
                && k.generation == l.generation
            {
                for (i, msg) in &info.errors {
                    let e = m.entry(*i).or_default();
                    if !e.is_empty() {
                        e.push('\n');
                    }
                    e.push_str(msg);
                }
            }
            m
        });
        let n_actions = self
            .derived
            .as_ref()
            .map_or(0, |d| d.act_prefix.last().copied().unwrap_or(0));

        // Toolbar.
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("event label substring")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.actions_only, "Actions only")
                .on_hover_text("Show only draws, dispatches, clears, copies and submits, plus the groups containing them");
            if ui.small_button("Collapse all").clicked() {
                self.collapsed = l
                    .tree
                    .iter()
                    .filter(|n| collapsible(n))
                    .map(|n| n.index)
                    .collect();
                self.setup_open = false;
            }
            if ui.small_button("Expand all").clicked() {
                self.collapsed.clear();
            }
            ui.separator();
            ui.weak(format!("{} events · {n_actions} actions", l.tree.len()));
            let frame_errors = errors.keys().filter(|&&i| i < l.tree.len()).count();
            if frame_errors > 0 {
                ui.colored_label(ERROR, format!("{frame_errors} with errors"));
            }
        });

        // Reveal requests (selection made elsewhere, or a newly selected setup call).
        let setup_changed = st.selected_setup != self.last_setup;
        self.last_setup = st.selected_setup;
        let mut target = None;
        if let Some(k) = st.selected_setup {
            if st.reveal_selected || setup_changed {
                self.setup_open = true;
                target = Some(Row::Setup(k));
            }
        } else if st.reveal_selected
            && let Some(sel) = st.selected
        {
            self.collapsed
                .retain(|&g| !(g < sel && l.tree.get(g).is_some_and(|n| n.span_end >= sel)));
            target = Some(Row::Frame(sel));
        }
        st.reveal_selected = false;
        if self.setup_open {
            self.ensure_setup_labels(l);
        }
        let visible = self.visible(l, st.selected);

        let row_h = ui.text_style_height(&TextStyle::Monospace) + 4.0;
        let row_step = row_h + ui.spacing().item_spacing.y;

        // Keyboard navigation (only when no text field has focus).
        let mut pick: Option<Row> = None;
        let no_focus = ui.ctx().memory(|m| m.focused().is_none());
        if no_focus && !visible.is_empty() {
            let (up, down) =
                ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
            if up || down {
                let cur = match (st.selected_setup, st.selected) {
                    (Some(k), _) => visible.iter().position(|r| *r == Row::Setup(k)),
                    (None, Some(s)) => visible
                        .iter()
                        .position(|r| matches!(r, Row::Frame(i) if *i >= s)),
                    _ => None,
                };
                let selectable = |r: &Row| *r != Row::SetupHeader;
                pick = match cur {
                    None => visible.iter().copied().find(selectable),
                    Some(p) if down => visible[p + 1..].iter().copied().find(selectable),
                    Some(p) => visible[..p].iter().rev().copied().find(selectable),
                };
            }
        }

        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(t) = target
            && let Some(p) = visible.iter().position(|&r| r == t)
        {
            let y = p as f32 * row_step;
            if y < self.last_offset || y + row_step > self.last_offset + self.last_view_h {
                scroll = scroll.vertical_scroll_offset((y - self.last_view_h * 0.5).max(0.0));
            }
        }

        // Column geometry (offsets from the row's left edge).
        let font = FontId::monospace(ui.text_style_height(&TextStyle::Monospace) - 1.0);
        let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0'));
        let digits = l
            .tree
            .len()
            .max(l.cap.setup.len())
            .max(10)
            .to_string()
            .len();
        let num_r = cw * digits as f32 + 6.0;
        let strip_x = num_r + 5.0;
        let label_x = strip_x + 9.0;
        let dur_w = if self.durations.is_some() {
            cw * 11.0 + 10.0
        } else {
            0.0
        };

        // Column header.
        {
            let (hrect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            let p = ui.painter_at(hrect);
            let vis = ui.visuals();
            p.rect_filled(hrect, 0.0, vis.faint_bg_color);
            p.hline(
                hrect.x_range(),
                hrect.bottom() - 0.5,
                vis.widgets.noninteractive.bg_stroke,
            );
            let c = vis.strong_text_color();
            let y = hrect.center().y;
            p.text(
                pos2(hrect.left() + num_r, y),
                Align2::RIGHT_CENTER,
                "#",
                font.clone(),
                c,
            );
            p.text(
                pos2(hrect.left() + label_x, y),
                Align2::LEFT_CENTER,
                "Event",
                font.clone(),
                c,
            );
            if dur_w > 0.0 {
                p.text(
                    pos2(hrect.right() - 8.0, y),
                    Align2::RIGHT_CENTER,
                    "GPU Duration",
                    font.clone(),
                    c,
                );
            }
        }

        let mut toggle_group = None;
        let mut toggle_setup = false;
        let derived = self.derived.as_ref();
        let durations = self.durations.as_ref();
        let collapsed = &self.collapsed;
        let setup_open = self.setup_open;
        let setup_n = l.cap.setup.len();
        let out = scroll.show_rows(ui, row_h, visible.len(), |ui, range| {
            for &row in &visible[range] {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let painter = ui.painter_at(rect);
                let vis = ui.visuals();
                let y = rect.center().y;
                let selected = match row {
                    Row::Frame(i) => st.selected == Some(i) && st.selected_setup.is_none(),
                    Row::Setup(k) => st.selected_setup == Some(k),
                    Row::SetupHeader => false,
                };
                let (weak, normal, strong) = if selected {
                    (SEL_DIM, SEL_TEXT, SEL_TEXT)
                } else {
                    (
                        vis.weak_text_color(),
                        vis.text_color(),
                        vis.strong_text_color(),
                    )
                };
                let err = match row {
                    Row::Frame(i) => errors.get(&i),
                    _ => None,
                };
                // Edited frame commands: (colour, badge letter).
                let edit = match row {
                    Row::Frame(i) => origin_style(l.origin_of(i)).map(|(c, b, _)| (c, b)),
                    _ => None,
                };
                if selected {
                    painter.rect_filled(rect, 2.0, vis.selection.bg_fill);
                } else if err.is_some() {
                    painter.rect_filled(rect, 2.0, ERROR.gamma_multiply(0.12));
                } else if resp.hovered() {
                    painter.rect_filled(rect, 2.0, vis.widgets.hovered.weak_bg_fill);
                } else if let Some((c, _)) = edit {
                    painter.rect_filled(rect, 2.0, c.gamma_multiply(0.10));
                }
                if let Some((c, _)) = edit {
                    painter.rect_filled(
                        Rect::from_min_max(rect.left_top(), pos2(rect.left() + 3.0, rect.bottom())),
                        1.0,
                        c,
                    );
                }

                // `#` column and kind strip.
                let (num, strip, depth, open) = match row {
                    Row::SetupHeader => (None, None, 0usize, Some(setup_open)),
                    Row::Setup(k) => (Some(k), Some(kind_color(&CmdKind::Call)), 1, None),
                    Row::Frame(i) => {
                        let n = &l.tree[i];
                        let open = collapsible(n).then(|| !collapsed.contains(&i));
                        (Some(i), Some(kind_color(&n.kind)), n.depth as usize, open)
                    }
                };
                if let Some(num) = num {
                    painter.text(
                        pos2(rect.left() + num_r, y),
                        Align2::RIGHT_CENTER,
                        num.to_string(),
                        font.clone(),
                        weak,
                    );
                }
                if let Some(c) = strip {
                    let x = rect.left() + strip_x;
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(x, rect.top() + 1.0),
                            pos2(x + 4.0, rect.bottom() - 1.0),
                        ),
                        1.0,
                        c,
                    );
                }

                // Event column.
                let label_rect = Rect::from_min_max(
                    pos2(rect.left() + label_x, rect.top()),
                    pos2(rect.right() - dur_w, rect.bottom()),
                );
                let lp = painter.with_clip_rect(label_rect);
                let indent = label_rect.left() + depth as f32 * INDENT;
                if let Some(open) = open {
                    lp.text(
                        pos2(indent, y),
                        Align2::LEFT_CENTER,
                        if open { "⏷" } else { "⏵" },
                        font.clone(),
                        normal,
                    );
                }
                let mut x = indent + 16.0;
                if let Some((c, badge)) = edit {
                    let r = Rect::from_min_size(
                        pos2(x, rect.top() + 2.0),
                        vec2(cw + 6.0, rect.height() - 4.0),
                    );
                    lp.rect_filled(r, 2.0, c);
                    lp.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        badge,
                        font.clone(),
                        text_on(c),
                    );
                    x = r.right() + 5.0;
                }
                match row {
                    Row::SetupHeader => {
                        lp.text(
                            pos2(x, y),
                            Align2::LEFT_CENTER,
                            format!("Pre-frame setup ({setup_n} calls)"),
                            font.clone(),
                            vis.strong_text_color(),
                        );
                    }
                    Row::Setup(k) | Row::Frame(k) => {
                        let (label, kind) = match row {
                            Row::Frame(_) => (l.labels[k].as_str(), Some(&l.tree[k].kind)),
                            _ => (
                                derived
                                    .and_then(|d| d.setup.as_ref())
                                    .map_or("", |(labels, _)| labels[k].as_str()),
                                None,
                            ),
                        };
                        let bold = matches!(kind, Some(CmdKind::Draw | CmdKind::Dispatch));
                        let color = match (err.is_some(), selected) {
                            (true, true) => SEL_ERROR,
                            (true, false) => ERROR,
                            _ if bold => strong,
                            _ => normal,
                        };
                        if let Some(CmdKind::GroupBegin(name)) = kind {
                            let r = lp.text(
                                pos2(x, y),
                                Align2::LEFT_CENTER,
                                name,
                                font.clone(),
                                if selected {
                                    SEL_TEXT
                                } else {
                                    marker_color(
                                        derived.map_or(0, |d| d.marker_depth[k] as usize),
                                        name,
                                    )
                                },
                            );
                            x = r.right() + 8.0;
                        }
                        // `Class#id.method(args)`: dimmed receiver, then the call in the row color.
                        let (recv, call) = match label.split_once('.') {
                            Some((recv, _)) => label.split_at(recv.len()),
                            None => ("", label),
                        };
                        let r = lp.text(pos2(x, y), Align2::LEFT_CENTER, recv, font.clone(), weak);
                        let tx = pos2(r.right(), y);
                        lp.text(tx, Align2::LEFT_CENTER, call, font.clone(), color);
                        if bold {
                            // Faux bold: overdraw shifted by a fraction of a pixel.
                            lp.text(
                                tx + vec2(0.7, 0.0),
                                Align2::LEFT_CENTER,
                                call,
                                font.clone(),
                                color,
                            );
                        }
                    }
                }

                // GPU duration column: item time for actions, summed time for groups.
                if let (Row::Frame(i), Some(d)) = (row, durations) {
                    let n = &l.tree[i];
                    let (ns, c) = if collapsible(n) {
                        (d.span(i, n.span_end), weak)
                    } else {
                        (d.per_item.get(&i).copied().unwrap_or(0), normal)
                    };
                    let has = collapsible(n) || d.per_item.contains_key(&i);
                    if has && ns > 0 {
                        painter.text(
                            pos2(rect.right() - 8.0, y),
                            Align2::RIGHT_CENTER,
                            fmt_ns(ns as f64),
                            font.clone(),
                            c,
                        );
                    }
                }
                if dur_w > 0.0 {
                    let x = rect.right() - dur_w + 2.0;
                    painter.vline(x, rect.y_range(), Stroke::new(1.0, vis.faint_bg_color));
                }

                let resp = match row {
                    Row::SetupHeader => resp,
                    Row::Setup(k) => resp.on_hover_ui_at_pointer(|ui| {
                        if let Some((labels, _)) = derived.and_then(|d| d.setup.as_ref()) {
                            ui.monospace(&labels[k]);
                        }
                        ui.weak("pre-frame setup call (replay shows the frame-start state)");
                    }),
                    Row::Frame(i) => resp.on_hover_ui_at_pointer(|ui| {
                        match l.origin_of(i) {
                            EditOrigin::Original(_) => {
                                ui.monospace(&l.labels[i]);
                            }
                            EditOrigin::Modified(o) => {
                                if let Some(c) = l.original.frame.get(o) {
                                    ui.weak("original:");
                                    ui.monospace(webgd_core::display::call_label(&l.original, c));
                                }
                                ui.colored_label(crate::widgets::EDIT_MODIFIED, "edited:");
                                ui.monospace(&l.labels[i]);
                            }
                            EditOrigin::Inserted { .. } => {
                                ui.colored_label(crate::widgets::EDIT_INSERTED, "inserted:");
                                ui.monospace(&l.labels[i]);
                            }
                        }
                        if let Some(e) = err {
                            ui.colored_label(ERROR, format!("replay error: {e}"));
                        }
                    }),
                };
                if resp.clicked() {
                    let on_toggle = resp
                        .interact_pointer_pos()
                        .is_some_and(|p| p.x < indent + INDENT);
                    match row {
                        Row::SetupHeader => toggle_setup = true,
                        Row::Frame(i) if open.is_some() && on_toggle => toggle_group = Some(i),
                        r => pick = Some(r),
                    }
                }
                if resp.double_clicked()
                    && let Row::Frame(i) = row
                    && open.is_some()
                {
                    toggle_group = Some(i);
                }
            }
        });
        self.last_offset = out.state.offset.y;
        self.last_view_h = out.inner_rect.height();
        if let Some(g) = toggle_group
            && !self.collapsed.remove(&g)
        {
            self.collapsed.insert(g);
        }
        if toggle_setup {
            self.setup_open = !self.setup_open;
        }
        if let Some(r) = pick {
            match r {
                Row::Frame(i) => st.select(i),
                Row::Setup(k) => st.select_setup(k),
                Row::SetupHeader => {}
            }
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            ui.ctx().request_repaint();
        }
    }
}
