//! Scrubber: one horizontal strip over the frame's actions (draws, dispatches, clears, copies,
//! submits) with marker / debug-group bands above it. Widths follow GPU duration when a trace is
//! available, otherwise every action gets the same width.
use crate::state::{AppState, Loaded};
use crate::widgets::{ACCENT, event_color, fmt_ns, kind_color, marker_color, origin_style};
use egui::{Align2, FontId, Key, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use std::collections::HashMap;
use std::rc::Rc;
use webgd_core::track::commands::{CmdKind, CmdNode};
use webgd_replay::TraceResult;

/// Band rows drawn above the action lane; deeper groups are not drawn.
const MAX_BAND_ROWS: usize = 6;
const BAND_H: f32 = 14.0;
/// Horizontal padding inside the strip.
const PAD: f32 = 4.0;

/// Commands that do GPU work or move data: the scrubber's stops.
pub(crate) fn is_action(k: &CmdKind) -> bool {
    matches!(
        k,
        CmdKind::Draw | CmdKind::Dispatch | CmdKind::Clear | CmdKind::Copy | CmdKind::Submit
    )
}

/// Action ordinal per frame index (position among the frame's actions, in frame order; `None`
/// for non-actions). Feeds `event_color` so the scrubber and trace color an action identically.
pub(crate) fn action_ordinals(tree: &[CmdNode]) -> Vec<Option<usize>> {
    let mut next = 0;
    tree.iter()
        .map(|c| {
            is_action(&c.kind).then(|| {
                next += 1;
                next - 1
            })
        })
        .collect()
}

/// GPU duration (ns) per frame index from the trace items of every queue.
pub(crate) fn item_durations(t: &TraceResult) -> HashMap<usize, u64> {
    let mut m = HashMap::new();
    for q in &t.queues {
        for s in &q.items {
            let d = s.end_ns.saturating_sub(s.start_ns);
            let e = m.entry(s.index).or_insert(0u64);
            *e = (*e).max(d);
        }
    }
    m
}

/// Identity of the trace currently in `st.trace` (0 when none); keys duration caches.
pub(crate) fn trace_key(st: &AppState) -> usize {
    st.trace
        .as_ref()
        .map_or(0, |t| Rc::as_ptr(t) as *const u8 as usize)
}

struct Band {
    begin: usize,
    end: usize,
    row: usize,
    name: String,
    actions: usize,
    dur_ns: u64,
}

/// Capture/trace-constant geometry, in fractions of the strip width.
struct Layout {
    key: (u64, usize),
    proportional: bool,
    /// Frame indices drawn as blocks, ascending. A block's position here is its action ordinal
    /// (same as `action_ordinals`; every call counts when the frame has no actions).
    slots: Vec<usize>,
    /// Start fraction per frame index; `start[n]` = 1.0.
    start: Vec<f64>,
    bands: Vec<Band>,
    rows: usize,
    durations: HashMap<usize, u64>,
    total_ns: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Band(usize),
    Event(usize),
}

impl Layout {
    fn build(l: &Loaded, trace: Option<&TraceResult>, key: (u64, usize)) -> Layout {
        let tree = &l.tree;
        let n = tree.len();
        let durations = trace.map(item_durations).unwrap_or_default();
        let actions: Vec<usize> = tree
            .iter()
            .filter(|c| is_action(&c.kind))
            .map(|c| c.index)
            .collect();
        // A frame without actions still gets a usable strip: every call is a stop.
        let slots = if actions.is_empty() {
            (0..n).collect()
        } else {
            actions
        };
        let measured: Vec<u64> = slots
            .iter()
            .filter_map(|i| durations.get(i).copied())
            .filter(|&d| d > 0)
            .collect();
        let proportional = !measured.is_empty();
        let mut weight = vec![0.0f64; n];
        if proportional {
            let mean = measured.iter().sum::<u64>() as f64 / measured.len() as f64;
            // Keeps unmeasured actions (copies, submits) visible as thin ticks.
            let floor = mean * 0.02;
            for &i in &slots {
                weight[i] = durations.get(&i).copied().unwrap_or(0) as f64 + floor;
            }
        } else {
            for &i in &slots {
                weight[i] = 1.0;
            }
        }
        let total: f64 = weight.iter().sum();
        let mut start = Vec::with_capacity(n + 1);
        let mut acc = 0.0;
        for w in &weight {
            start.push(if total > 0.0 { acc / total } else { 0.0 });
            acc += w;
        }
        start.push(1.0);

        let mut act_prefix = Vec::with_capacity(n + 1);
        let mut dur_prefix = Vec::with_capacity(n + 1);
        let (mut a, mut d) = (0usize, 0u64);
        for c in tree {
            act_prefix.push(a);
            dur_prefix.push(d);
            if is_action(&c.kind) {
                a += 1;
            }
            d += durations.get(&c.index).copied().unwrap_or(0);
        }
        act_prefix.push(a);
        dur_prefix.push(d);

        let mut bands = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        for c in tree {
            while open.last().is_some_and(|&e| e < c.index) {
                open.pop();
            }
            if let CmdKind::GroupBegin(name) = &c.kind {
                let end = c.span_end.min(n - 1);
                bands.push(Band {
                    begin: c.index,
                    end,
                    row: open.len(),
                    name: name.clone(),
                    actions: act_prefix[end + 1] - act_prefix[c.index],
                    dur_ns: dur_prefix[end + 1] - dur_prefix[c.index],
                });
                if end > c.index {
                    open.push(end);
                }
            }
        }
        let rows = bands
            .iter()
            .map(|b| b.row + 1)
            .max()
            .unwrap_or(0)
            .min(MAX_BAND_ROWS);
        Layout {
            key,
            proportional,
            slots,
            start,
            bands,
            rows,
            total_ns: d,
            durations,
        }
    }

    /// Slot nearest to `frac`, preferring the one containing it.
    fn nearest(&self, frac: f64) -> Option<usize> {
        let k = self.slots.partition_point(|&i| self.start[i] <= frac);
        if k > 0 {
            let i = self.slots[k - 1];
            if frac < self.start[i + 1] {
                return Some(i);
            }
        }
        let center = |i: usize| (self.start[i] + self.start[i + 1]) * 0.5;
        [k.checked_sub(1), Some(k)]
            .into_iter()
            .flatten()
            .filter_map(|k| self.slots.get(k).copied())
            .min_by(|&a, &b| {
                (center(a) - frac)
                    .abs()
                    .total_cmp(&(center(b) - frac).abs())
            })
    }

    fn band_at(&self, row: usize, frac: f64, tol: f64) -> Option<usize> {
        self.bands.iter().position(|b| {
            b.row == row && self.start[b.begin] - tol <= frac && frac <= self.start[b.end + 1] + tol
        })
    }
}

#[derive(Default)]
pub struct ScrubberUi {
    layout: Option<Layout>,
}

impl ScrubberUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        let n = l.tree.len();
        if n == 0 {
            ui.weak("The captured frame has no calls.");
            return;
        }
        let key = (l.generation, trace_key(st));
        if self.layout.as_ref().is_some_and(|x| x.key != key) {
            self.layout = None;
        }
        let lay = self
            .layout
            .get_or_insert_with(|| Layout::build(l, st.trace.as_deref(), key));

        ui.horizontal(|ui| {
            let what = if lay.slots.len() == n {
                format!("{n} calls")
            } else {
                format!("{} actions", lay.slots.len())
            };
            if lay.proportional {
                ui.weak(format!(
                    "{what} · GPU {} · width: GPU duration",
                    fmt_ns(lay.total_ns as f64)
                ));
            } else {
                ui.weak(format!("{what} · width: uniform"));
            }
            match st.selected.filter(|&s| s < n) {
                Some(s) => {
                    ui.separator();
                    ui.monospace(format!("#{s}"));
                    ui.add(egui::Label::new(&l.labels[s]).truncate());
                }
                None if st.selected_setup.is_some() => {
                    ui.separator();
                    ui.weak("frame start");
                }
                None => {}
            }
        });

        let rows = lay.rows;
        let avail = ui.available_size();
        let lane_h = (avail.y - rows as f32 * BAND_H - 2.0).clamp(22.0, 64.0);
        let size = vec2(avail.x.max(64.0), rows as f32 * BAND_H + lane_h);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let vis = ui.visuals();
        painter.rect_filled(rect, 2.0, vis.extreme_bg_color);
        let x0 = rect.left() + PAD;
        let width = (rect.width() - 2.0 * PAD).max(1.0);
        let fx = |f: f64| x0 + f as f32 * width;
        let to_frac = |x: f32| ((x - x0) / width).clamp(0.0, 1.0) as f64;
        let lane = Rect::from_min_max(
            pos2(rect.left(), rect.top() + rows as f32 * BAND_H),
            rect.max,
        );
        let band_rect = |b: &Band| {
            let top = rect.top() + b.row as f32 * BAND_H;
            let a = fx(lay.start[b.begin]);
            let e = fx(lay.start[b.end + 1]).max(a + 2.0);
            Rect::from_min_max(pos2(a, top + 1.0), pos2(e, top + BAND_H - 1.0))
        };
        let block_rect = |i: usize| {
            let (mut a, mut e) = (fx(lay.start[i]), fx(lay.start[i + 1]));
            if e - a > 3.0 {
                // Hairline gap between neighbouring blocks.
                a += 0.5;
                e -= 0.5;
            } else if e - a < 1.0 {
                e = a + 1.0;
            }
            Rect::from_min_max(pos2(a, lane.top() + 3.0), pos2(e, lane.bottom() - 3.0))
        };

        // Hit test (hover, click, drag).
        let hit_at = |p: egui::Pos2, allow_band: bool| -> Option<Hit> {
            let frac = to_frac(p.x);
            if allow_band && p.y < lane.top() && rows > 0 {
                let row = (((p.y - rect.top()) / BAND_H).max(0.0) as usize).min(rows - 1);
                let tol = 3.0 / width as f64;
                if let Some(b) = lay.band_at(row, frac, tol) {
                    return Some(Hit::Band(b));
                }
            }
            lay.nearest(frac).map(Hit::Event)
        };
        let hover = resp.hover_pos().and_then(|p| hit_at(p, true));

        // Marker bands.
        let band_font = FontId::proportional(10.0);
        for (bi, b) in lay.bands.iter().enumerate() {
            if b.row >= rows {
                continue;
            }
            let r = band_rect(b);
            let color = marker_color(b.row, &b.name);
            painter.rect_filled(r, 2.0, color.gamma_multiply(0.55));
            let stroke = if hover == Some(Hit::Band(bi)) {
                Stroke::new(1.5, vis.strong_text_color())
            } else {
                Stroke::new(1.0, color)
            };
            painter.rect_stroke(r, 2.0, stroke, StrokeKind::Inside);
            if r.width() > 24.0 {
                painter.with_clip_rect(r.shrink(1.0).intersect(rect)).text(
                    pos2(r.left() + 3.0, r.center().y),
                    Align2::LEFT_CENTER,
                    &b.name,
                    band_font.clone(),
                    vis.strong_text_color(),
                );
            }
        }

        // Action lane: per-action fill, kind strip along the bottom, dark separators between blocks.
        let sep = Stroke::new(1.0, vis.extreme_bg_color);
        for (ord, &i) in lay.slots.iter().enumerate() {
            let r = block_rect(i);
            if r.right() < rect.left() || r.left() > rect.right() {
                continue;
            }
            let kind = &l.tree[i].kind;
            painter.rect_filled(r, 1.0, event_color(kind, ord));
            painter.rect_filled(
                Rect::from_min_max(pos2(r.left(), r.bottom() - 3.0), r.max),
                0.0,
                kind_color(kind),
            );
            if ord > 0 && r.width() > 2.0 {
                painter.vline(fx(lay.start[i]), r.y_range(), sep);
            }
        }

        // Edit markers: a triangle above each edited action / insertion point, in the edit colour.
        for (i, &o) in l.origin.iter().enumerate() {
            let Some((color, _, _)) = origin_style(o) else {
                continue;
            };
            let x = if lay.start[i + 1] > lay.start[i] {
                block_rect(i).center().x
            } else {
                fx(lay.start[i])
            };
            let top = lane.top();
            painter.add(egui::Shape::convex_polygon(
                vec![pos2(x - 4.0, top), pos2(x + 4.0, top), pos2(x, top + 6.0)],
                color,
                Stroke::new(1.0, vis.extreme_bg_color),
            ));
        }
        if let Some(Hit::Event(i)) = hover {
            painter.rect_stroke(
                block_rect(i).expand(1.0),
                1.0,
                Stroke::new(1.5, vis.strong_text_color()),
                StrokeKind::Outside,
            );
        }

        // Current selection.
        if let Some(sel) = st.selected.filter(|&s| s < n) {
            let r = block_rect(sel);
            let x = if lay.start[sel + 1] > lay.start[sel] {
                r.center().x
            } else {
                fx(lay.start[sel])
            };
            if lay.start[sel + 1] > lay.start[sel] {
                painter.rect_stroke(
                    r.expand(1.5),
                    1.0,
                    Stroke::new(1.5, ACCENT),
                    StrokeKind::Outside,
                );
            }
            painter.vline(x, rect.y_range(), Stroke::new(2.0, ACCENT));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 4.0, rect.top()),
                    pos2(x + 4.0, rect.top()),
                    pos2(x, rect.top() + 5.0),
                ],
                ACCENT,
                Stroke::NONE,
            ));
        } else if st.selected_setup.is_some() {
            painter.vline(fx(0.0), rect.y_range(), Stroke::new(2.0, ACCENT));
        }

        // Selection input.
        let mut pick = None;
        if (resp.clicked() || resp.dragged())
            && let Some(p) = resp.interact_pointer_pos()
        {
            pick = match hit_at(p, resp.clicked()) {
                Some(Hit::Band(b)) => Some(lay.bands[b].begin),
                Some(Hit::Event(i)) => Some(i),
                None => None,
            };
        }
        let no_focus = ui.ctx().memory(|m| m.focused().is_none());
        if no_focus && (resp.hovered() || resp.dragged()) {
            let (left, right) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                )
            });
            let cur = st.selected.filter(|&s| s < n);
            if right {
                let k = cur.map_or(0, |c| lay.slots.partition_point(|&i| i <= c));
                pick = lay.slots.get(k).copied().or(pick);
            } else if left {
                pick = match cur {
                    Some(c) => {
                        let k = lay.slots.partition_point(|&i| i < c);
                        k.checked_sub(1).map(|k| lay.slots[k]).or(pick)
                    }
                    None => lay.slots.last().copied(),
                };
            }
        }

        if let Some(h) = hover {
            resp.on_hover_ui_at_pointer(|ui| match h {
                Hit::Event(i) => {
                    ui.monospace(format!("#{i}  {}", l.labels[i]));
                    let kind = &l.tree[i].kind;
                    match lay.durations.get(&i) {
                        Some(&d) => ui.weak(format!("{kind:?} · GPU {}", fmt_ns(d as f64))),
                        None => ui.weak(format!("{kind:?}")),
                    };
                    if let Some((c, _, word)) = origin_style(l.origin_of(i)) {
                        ui.colored_label(c, word);
                    }
                }
                Hit::Band(b) => {
                    let b = &lay.bands[b];
                    ui.strong(&b.name);
                    ui.weak(format!("#{}–#{} · {} actions", b.begin, b.end, b.actions));
                    if b.dur_ns > 0 {
                        ui.weak(format!("GPU {}", fmt_ns(b.dur_ns as f64)));
                    }
                }
            });
        }

        // Selection from the scrubber does not change the focused tab, so the strip stays visible
        // while dragging; the Events list scrolls to the new selection.
        if let Some(i) = pick
            && (st.selected != Some(i) || st.selected_setup.is_some())
        {
            st.selected = Some(i);
            st.selected_setup = None;
            st.reveal_selected = true;
        }
    }
}
