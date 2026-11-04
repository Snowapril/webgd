//! GPU Trace panel: hand-painted timeline with collapsible per-queue row groups (pass bars, marker
//! rows, item bars), estimated counter graphs, a hover crosshair, time-range selection and a range
//! summary table. The latest successful result is shared through `AppState::trace`.
use super::scrubber::action_ordinals;
use crate::replay_queue::get_replayer;
use crate::state::AppState;
use crate::widgets::{
    ACCENT, ERROR, WARN, event_color, fmt_ns, fmt_num, kind_color, marker_color, text_on,
};
use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, pos2, vec2,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use webgd_core::track::commands::{CmdKind, CmdNode};
use webgd_replay::{CounterSample, CounterTrack, QueueTrack, Span, TraceResult};

const LABEL_W: f32 = 200.0;
const RULER_H: f32 = 22.0;
const HEADER_H: f32 = 20.0;
const MARKER_H: f32 = 16.0;
const ITEMS_H: f32 = 20.0;
const COUNTER_H: f32 = 40.0;
/// Label indent of rows inside a group.
const INDENT: f32 = 18.0;
/// Rows of the "top items by duration" table.
const TOP_ITEMS: usize = 10;
/// Narrowest visible window when zooming in.
const MIN_VIEW_NS: f64 = 10.0;
/// Range drags narrower than this many pixels are discarded (treated as a click).
const MIN_RANGE_PX: f32 = 3.0;
/// Major tick spacing target in pixels.
const TICK_PX: f32 = 90.0;

#[derive(Default)]
struct Shared {
    running: bool,
    result: Option<(u64, Result<TraceResult, String>)>,
}

/// Collapsible row group.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Queue(usize),
    Counters,
}

#[derive(Default)]
pub struct TraceUi {
    shared: Rc<RefCell<Shared>>,
    /// Last failed run: (generation, message).
    error: Option<(u64, String)>,
    /// Visible time window in ns.
    view: (f64, f64),
    fit: bool,
    collapsed: HashSet<Group>,
    /// Selected time range in ns (`lo <= hi`).
    range: Option<(f64, f64)>,
    /// Anchor time of an in-progress range drag.
    range_anchor: Option<f64>,
    /// `st.selected` as seen last frame; a change scrolls the timeline to the selected command.
    last_selected: Option<usize>,
    /// `action_ordinals` of the loaded capture, keyed by its generation.
    ordinals: Option<(u64, Vec<Option<usize>>)>,
}

enum Row<'a> {
    /// Group header; its lane shows the queue's pass bars.
    QueueHeader(usize, &'a QueueTrack),
    Markers(&'a QueueTrack, u8),
    Items(&'a QueueTrack),
    CounterHeader,
    /// Counter track and its maximum finite value.
    Counter(&'a CounterTrack, f64),
}

impl<'a> Row<'a> {
    fn height(&self) -> f32 {
        match self {
            Row::QueueHeader(..) | Row::CounterHeader => HEADER_H,
            Row::Markers(..) => MARKER_H,
            Row::Items(_) => ITEMS_H,
            Row::Counter(..) => COUNTER_H,
        }
    }

    fn group(&self) -> Option<Group> {
        match self {
            Row::QueueHeader(i, _) => Some(Group::Queue(*i)),
            Row::CounterHeader => Some(Group::Counters),
            _ => None,
        }
    }

    /// Bars shown in this row's lane.
    fn spans(&self) -> impl Iterator<Item = &'a Span> + use<'a> {
        let (list, depth): (&'a [Span], Option<u8>) = match *self {
            Row::QueueHeader(_, q) => (q.passes.as_slice(), None),
            Row::Markers(q, d) => (q.markers.as_slice(), Some(d)),
            Row::Items(q) => (q.items.as_slice(), None),
            Row::CounterHeader | Row::Counter(..) => (&[][..], None),
        };
        list.iter()
            .filter(move |s| depth.is_none_or(|d| s.depth == d))
    }
}

/// What the pointer is over in the row area.
enum Hit<'a> {
    Span(&'a Span),
    Counter(&'a CounterTrack, &'a CounterSample),
}

/// Maps between time (ns) and screen x over the lane area.
#[derive(Clone, Copy)]
struct Axis {
    t0: f64,
    t1: f64,
    left: f32,
    right: f32,
}

impl Axis {
    fn ns_per_px(&self) -> f64 {
        (self.t1 - self.t0) / (self.right - self.left).max(1.0) as f64
    }

    /// Screen x of `t`, clamped near the lane so far off-screen spans stay finite for painting.
    fn x(&self, t: f64) -> f32 {
        let x = self.left as f64 + (t - self.t0) / self.ns_per_px();
        x.clamp(self.left as f64 - 1e4, self.right as f64 + 1e4) as f32
    }

    fn t(&self, x: f32) -> f64 {
        self.t0 + (x - self.left) as f64 * self.ns_per_px()
    }
}

fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return 1.0;
    }
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    p * if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    }
}

/// Major tick step (ns) and number of minor subdivisions per major step.
fn tick_steps(axis: Axis) -> (f64, i64) {
    let step = nice_step(TICK_PX as f64 * axis.ns_per_px());
    let mantissa = step / 10f64.powf(step.log10().floor());
    let div = if (mantissa - 2.0).abs() < 0.01 { 4 } else { 5 };
    (step, div)
}

/// Calls `f(t, is_major)` for every minor tick inside the axis window.
fn for_each_tick(axis: Axis, mut f: impl FnMut(f64, bool)) {
    let (step, div) = tick_steps(axis);
    let minor = step / div as f64;
    let first = (axis.t0 / minor).floor() as i64;
    let last = ((axis.t1 / minor).ceil() as i64).min(first + 4000);
    for k in first..=last {
        f(k as f64 * minor, k.rem_euclid(div) == 0);
    }
}

/// Tick label with just enough decimals for the step.
fn fmt_tick(t: f64, step: f64) -> String {
    let (scale, unit) = if step >= 1e6 {
        (1e6, "ms")
    } else if step >= 1e3 {
        (1e3, "µs")
    } else {
        (1.0, "ns")
    };
    let t = if t.abs() < step * 1e-6 { 0.0 } else { t };
    let decimals = (-(step / scale).log10().floor()).max(0.0) as usize;
    format!("{:.*} {unit}", decimals, t / scale)
}

/// Precise time readout (crosshair, range bounds).
fn fmt_time(t: f64) -> String {
    let a = t.abs();
    if a >= 1e6 {
        format!("{:.4} ms", t / 1e6)
    } else if a >= 1e3 {
        format!("{:.3} µs", t / 1e3)
    } else {
        format!("{t:.1} ns")
    }
}

fn dur_ns(s: &Span) -> u64 {
    s.end_ns.saturating_sub(s.start_ns)
}

/// Overlap in ns of `[a, b]` with `[lo, hi]`.
fn overlap(a: f64, b: f64, lo: f64, hi: f64) -> f64 {
    (b.min(hi) - a.max(lo)).max(0.0)
}

/// Span intersects the range (zero-width spans: lies inside it).
fn in_range(s: &Span, lo: f64, hi: f64) -> bool {
    let (a, b) = (s.start_ns as f64, s.end_ns as f64);
    if b <= a {
        a >= lo && a <= hi
    } else {
        a < hi && b > lo
    }
}

/// Counter sample covering time `t`.
fn counter_at(c: &CounterTrack, t: f64) -> Option<&CounterSample> {
    c.samples
        .iter()
        .find(|s| (s.start_ns as f64) <= t && t <= s.end_ns as f64)
}

/// (time-weighted average value, total) of a counter over `[lo, hi]`. Totals of partially covered
/// samples are prorated by overlap; zero-width samples inside the range count fully.
fn counter_over(c: &CounterTrack, lo: f64, hi: f64) -> (Option<f64>, f64) {
    let (mut total, mut weighted, mut weight) = (0.0, 0.0, 0.0);
    let (mut point_sum, mut points) = (0.0, 0usize);
    for s in &c.samples {
        if !s.value.is_finite() {
            continue;
        }
        let (a, b) = (s.start_ns as f64, s.end_ns as f64);
        if b <= a {
            if a >= lo && a <= hi {
                total += s.total;
                point_sum += s.value;
                points += 1;
            }
            continue;
        }
        let ov = overlap(a, b, lo, hi);
        if ov <= 0.0 {
            continue;
        }
        total += s.total * ov / (b - a);
        weighted += s.value * ov;
        weight += ov;
    }
    let avg = if weight > 0.0 {
        Some(weighted / weight)
    } else if points > 0 {
        Some(point_sum / points as f64)
    } else {
        None
    };
    (avg, total)
}

/// Time window to bring into view for a selected command: its own bar (item, marker or pass), else
/// the first item recorded after it.
fn focus_span(tr: &TraceResult, sel: usize) -> Option<(f64, f64)> {
    let window = |s: &Span| (s.start_ns as f64, s.end_ns.max(s.start_ns) as f64);
    tr.queues
        .iter()
        .flat_map(|q| q.items.iter().chain(&q.markers).chain(&q.passes))
        .find(|s| s.index == sel)
        .or_else(|| {
            tr.queues
                .iter()
                .flat_map(|q| &q.items)
                .filter(|s| s.index > sel)
                .min_by_key(|s| s.index)
        })
        .map(window)
}

fn span_color(row: &Row, s: &Span, tree: &[CmdNode], ordinals: &[Option<usize>]) -> Color32 {
    match row {
        Row::QueueHeader(..) => kind_color(&CmdKind::PassBegin),
        Row::Markers(..) => marker_color(s.depth as usize, &s.label),
        _ => tree.get(s.index).map_or(Color32::GRAY, |n| {
            let ord = ordinals.get(s.index).copied().flatten().unwrap_or(s.index);
            event_color(&n.kind, ord)
        }),
    }
}

/// Bar extent of a span: (x0, x1, zero_width).
fn span_x(axis: Axis, s: &Span) -> (f32, f32, bool) {
    let x0 = axis.x(s.start_ns as f64);
    let zero = s.end_ns <= s.start_ns;
    let x1 = if zero {
        x0 + 2.0
    } else {
        axis.x(s.end_ns as f64).max(x0 + 1.0)
    };
    (x0, x1, zero)
}

/// Fills the time band `[lo, hi]` within `area` (at least 1 px wide).
fn band(p: &Painter, axis: Axis, lo: f64, hi: f64, area: Rect, fill: Color32) {
    let x0 = axis.x(lo);
    let x1 = axis.x(hi).max(x0 + 1.0);
    let r = Rect::from_min_max(pos2(x0, area.top()), pos2(x1, area.bottom())).intersect(area);
    if r.width() > 0.0 {
        p.rect_filled(r, 0.0, fill);
    }
}

/// Disclosure triangle (open = pointing down).
fn paint_triangle(p: &Painter, c: Pos2, open: bool, col: Color32) {
    let s = 4.0;
    let pts = if open {
        vec![
            pos2(c.x - s, c.y - s * 0.6),
            pos2(c.x + s, c.y - s * 0.6),
            pos2(c.x, c.y + s * 0.8),
        ]
    } else {
        vec![
            pos2(c.x - s * 0.6, c.y - s),
            pos2(c.x + s * 0.8, c.y),
            pos2(c.x - s * 0.6, c.y + s),
        ]
    };
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// Row under screen y, given the rows' top edge.
fn row_at<'r, 'a>(rows: &'r [Row<'a>], top: f32, y: f32) -> Option<&'r Row<'a>> {
    let mut y0 = top;
    for row in rows {
        let y1 = y0 + row.height();
        if y >= y0 && y < y1 {
            return Some(row);
        }
        y0 = y1;
    }
    None
}

impl TraceUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        let generation = l.generation;
        let cap = l.cap.clone();

        // Collect a finished run; only results for the loaded capture are kept.
        let finished = self.shared.borrow_mut().result.take();
        if let Some((g, res)) = finished
            && g == generation
        {
            match res {
                Ok(tr) => {
                    st.trace = Some(Rc::new(tr));
                    self.error = None;
                    self.fit = true;
                    self.range = None;
                    self.range_anchor = None;
                    // Re-reveal the current selection on the new layout.
                    self.last_selected = None;
                }
                Err(e) => self.error = Some((g, e)),
            }
        }

        let running = self.shared.borrow().running;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Run trace"))
                .clicked()
            {
                self.shared.borrow_mut().running = true;
                let (shared, cell, ctx) = (
                    self.shared.clone(),
                    st.replay.replayer_cell(),
                    st.ctx.clone(),
                );
                let cap = cap.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let rep = get_replayer(&cell).await;
                    let res = webgd_replay::run_trace(&rep, &cap)
                        .await
                        .map_err(|e| e.to_string());
                    let mut sh = shared.borrow_mut();
                    sh.running = false;
                    sh.result = Some((generation, res));
                    drop(sh);
                    ctx.request_repaint();
                });
            }
            if ui
                .add_enabled(
                    !running && st.trace.is_some(),
                    egui::Button::new("Export as text"),
                )
                .clicked()
            {
                crate::app::export_trace_text(st);
            }
            if running {
                ui.spinner();
                ui.label("tracing (5 instrumented replays)…");
            }
            if let Some((lo, hi)) = self.range {
                ui.separator();
                ui.label(format!(
                    "range {} – {} ({})",
                    fmt_time(lo),
                    fmt_time(hi),
                    fmt_ns(hi - lo)
                ));
                if ui.small_button("Zoom to range").clicked() && hi > lo {
                    self.view = (lo, hi);
                }
                if ui.small_button("Clear range").clicked() {
                    self.range = None;
                    self.range_anchor = None;
                }
            }
        });
        ui.weak(
            "ctrl+wheel: zoom · drag: pan · F: fit · drag ruler or shift+drag: select range · \
             Esc: clear range · click bar: select command · click group header: collapse",
        );
        if let Some((g, e)) = &self.error
            && *g == generation
        {
            ui.colored_label(ERROR, format!("trace failed: {e}"));
        }
        let Some(tr) = st.trace.clone() else {
            if !running {
                ui.weak("No trace yet. Run trace replays the frame 5× with GPU timing and keeps the median run.");
            }
            return;
        };
        for n in &tr.notes {
            ui.label(RichText::new(format!("• {n}")).color(WARN));
        }

        let total = tr.total_ns.max(1) as f64;
        if self.fit || self.view.1 <= self.view.0 || !self.view.0.is_finite() {
            self.view = (0.0, total);
            self.fit = false;
        }
        if st.selected != self.last_selected {
            self.last_selected = st.selected;
            if let Some(sel) = st.selected
                && let Some((a, b)) = focus_span(&tr, sel)
            {
                self.reveal(a, b);
            }
        }
        if self.range.is_some()
            && ui.input(|i| i.key_pressed(Key::Escape))
            && ui.ctx().memory(|m| m.focused().is_none())
        {
            self.range = None;
            self.range_anchor = None;
        }

        let selected = st.selected;
        let mut select = None;
        if let Some(range) = self.range {
            // Summary defaults to ~30% of the tab; the timeline keeps ≥ 55% and ≥ 200 px.
            let avail = ui.available_height();
            let max = (avail * 0.45).min(avail - 200.0).max(60.0);
            let min = 140.0f32.min(max);
            egui::Panel::bottom(ui.id().with("trace-range-summary"))
                .resizable(true)
                .default_size((avail * 0.3).clamp(min, max))
                .min_size(min)
                .max_size(max)
                .show(ui, |ui| {
                    if let Some(i) = range_summary(ui, &tr, range, selected) {
                        select = Some(i);
                    }
                });
        }

        let Some(l) = st.loaded.as_ref() else {
            return;
        };
        let (tree, labels) = (&l.tree, &l.labels);
        if self.ordinals.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.ordinals = Some((generation, action_ordinals(tree)));
        }

        // Ruler: fixed above the scrolling rows; drag selects a range, click clears it.
        let (ruler_rect, ruler_resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), RULER_H), Sense::click_and_drag());
        let region_bottom = ui
            .available_rect_before_wrap()
            .bottom()
            .max(ruler_rect.bottom());
        let region = Rect::from_min_max(ruler_rect.min, pos2(ruler_rect.right(), region_bottom));
        let (left, right) = (ruler_rect.left() + LABEL_W, ruler_rect.right());
        let axis_of = |view: (f64, f64)| Axis {
            t0: view.0,
            t1: view.1,
            left,
            right,
        };
        let crosshair = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| ui.rect_contains_pointer(region) && p.x >= left && p.x <= right)
            .map(|p| p.x);

        if ruler_resp.hovered() {
            self.zoom_input(ui, axis_of(self.view), total);
            if crosshair.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            }
        }
        self.range_drag(ui, &ruler_resp, axis_of(self.view), total, true);
        if ruler_resp.clicked() {
            self.range = None;
            self.range_anchor = None;
        }

        let rows = self.rows(&tr);
        let mut toggle = None;
        egui::ScrollArea::vertical()
            .id_salt("trace-rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let height: f32 = rows.iter().map(Row::height).sum();
                let (rect, resp) = ui.allocate_exact_size(
                    vec2(ui.available_width(), height),
                    Sense::click_and_drag(),
                );
                if resp.hovered() {
                    self.zoom_input(ui, axis_of(self.view), total);
                    let dx = ui
                        .ctx()
                        .input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.x));
                    if dx != 0.0 {
                        let d = -dx as f64 * axis_of(self.view).ns_per_px();
                        self.view = (self.view.0 + d, self.view.1 + d);
                    }
                }
                if !self.range_drag(ui, &resp, axis_of(self.view), total, false) && resp.dragged() {
                    let d = -resp.drag_delta().x as f64 * axis_of(self.view).ns_per_px();
                    self.view = (self.view.0 + d, self.view.1 + d);
                }
                let axis = axis_of(self.view);
                let hover = resp.hover_pos();
                let hit = self.paint_rows(ui, rect, &rows, axis, tree, selected, crosshair, hover);

                // Group headers in the label column toggle collapse.
                let header_under = |p: Pos2| {
                    (p.x < left)
                        .then(|| row_at(&rows, rect.top(), p.y).and_then(Row::group))
                        .flatten()
                };
                if let Some(p) = hover
                    && header_under(p).is_some()
                {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if resp.clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    if let Some(g) = header_under(p) {
                        toggle = Some(g);
                    } else if let Some(Hit::Span(s)) = &hit {
                        select = Some(s.index);
                    }
                }
                if let Some(hit) = &hit
                    && !resp.dragged()
                {
                    let t = hover.map(|p| axis.t(p.x));
                    resp.clone()
                        .on_hover_ui_at_pointer(|ui| hit_tooltip(ui, &tr, hit, t, labels));
                }
            });
        if let Some(g) = toggle
            && !self.collapsed.remove(&g)
        {
            self.collapsed.insert(g);
        }
        self.paint_ruler(ui, ruler_rect, axis_of(self.view), crosshair);

        if let Some(i) = select {
            st.select(i);
        }
    }

    fn rows<'a>(&self, tr: &'a TraceResult) -> Vec<Row<'a>> {
        let mut rows = Vec::new();
        for (i, q) in tr.queues.iter().enumerate() {
            rows.push(Row::QueueHeader(i, q));
            if self.collapsed.contains(&Group::Queue(i)) {
                continue;
            }
            if let Some(md) = q.markers.iter().map(|m| m.depth).max() {
                for d in 0..=md {
                    rows.push(Row::Markers(q, d));
                }
            }
            rows.push(Row::Items(q));
        }
        if !tr.counters.is_empty() {
            rows.push(Row::CounterHeader);
            if !self.collapsed.contains(&Group::Counters) {
                for c in &tr.counters {
                    let max = c
                        .samples
                        .iter()
                        .map(|s| s.value)
                        .filter(|v| v.is_finite())
                        .fold(0.0, f64::max);
                    rows.push(Row::Counter(c, max));
                }
            }
        }
        rows
    }

    /// Scrolls (keeping zoom) so `[a, b]` is visible; zooms out when it does not fit.
    fn reveal(&mut self, a: f64, b: f64) {
        let (v0, v1) = self.view;
        if a >= v0 && b <= v1 {
            return;
        }
        let w = v1 - v0;
        let len = b - a;
        if len > w * 0.9 {
            let pad = (len * 0.05).max(MIN_VIEW_NS);
            self.view = (a - pad, b + pad);
        } else {
            let c = (a + b) * 0.5;
            self.view = (c - w * 0.5, c + w * 0.5);
        }
    }

    /// ctrl+wheel zoom around the cursor; F fits the whole trace.
    fn zoom_input(&mut self, ui: &egui::Ui, axis: Axis, total: f64) {
        let (zoom, hover, fit) =
            ui.input(|i| (i.zoom_delta(), i.pointer.hover_pos(), i.key_pressed(Key::F)));
        if fit {
            self.view = (0.0, total);
            return;
        }
        if zoom != 1.0
            && let Some(p) = hover
        {
            let x = p.x.clamp(axis.left, axis.right);
            let anchor = axis.t(x);
            let span = ((self.view.1 - self.view.0) / zoom as f64).clamp(MIN_VIEW_NS, total * 4.0);
            let frac = ((x - axis.left) / (axis.right - axis.left).max(1.0)) as f64;
            let start = anchor - span * frac;
            self.view = (start, start + span);
        }
    }

    /// Range-selection drag on `resp`. `always`: every drag selects (ruler); otherwise only
    /// shift+drag. Returns true while `resp` is driving a range drag.
    fn range_drag(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        axis: Axis,
        total: f64,
        always: bool,
    ) -> bool {
        if resp.drag_started() {
            self.range_anchor = None;
            if always || ui.input(|i| i.modifiers.shift) {
                let origin = ui
                    .input(|i| i.pointer.press_origin())
                    .or(resp.interact_pointer_pos());
                if let Some(p) = origin {
                    self.range_anchor = Some(axis.t(p.x.clamp(axis.left, axis.right)));
                }
            }
        }
        let Some(anchor) = self.range_anchor else {
            return false;
        };
        if !(resp.dragged() || resp.drag_stopped()) {
            return false;
        }
        if let Some(p) = resp.interact_pointer_pos() {
            let t = axis.t(p.x.clamp(axis.left, axis.right));
            self.range = Some((anchor.min(t), anchor.max(t)));
        }
        if resp.drag_stopped() {
            self.range_anchor = None;
            self.range = self.range.and_then(|(lo, hi)| {
                let (lo, hi) = (lo.max(0.0), hi.min(total));
                (hi - lo >= MIN_RANGE_PX as f64 * axis.ns_per_px()).then_some((lo, hi))
            });
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_rows<'t>(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        rows: &[Row<'t>],
        axis: Axis,
        tree: &[CmdNode],
        selected: Option<usize>,
        crosshair: Option<f32>,
        hover: Option<Pos2>,
    ) -> Option<Hit<'t>> {
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let ordinals = self
            .ordinals
            .as_ref()
            .map_or(&[][..], |(_, o)| o.as_slice());
        let sep = Stroke::new(1.0, visuals.extreme_bg_color);
        let font = FontId::proportional(11.0);
        let header_font = FontId::proportional(12.0);
        let text_col = visuals.text_color();
        let strong_col = visuals.strong_text_color();
        let lanes = Rect::from_min_max(pos2(axis.left, rect.top()), rect.max);
        painter.rect_filled(
            Rect::from_min_max(rect.min, pos2(axis.left, rect.bottom())),
            0.0,
            visuals.faint_bg_color,
        );

        // Grid at major ticks.
        let grid = Stroke::new(
            1.0,
            visuals
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .gamma_multiply(0.6),
        );
        let lane_p = painter.with_clip_rect(lanes);
        for_each_tick(axis, |t, major| {
            if major {
                lane_p.vline(axis.x(t), rect.y_range(), grid);
            }
        });
        // Selected-command column across all rows.
        if let Some(sel) = selected {
            for row in rows {
                if let Row::Items(q) = row {
                    for s in q.items.iter().filter(|s| s.index == sel) {
                        let (x0, x1, _) = span_x(axis, s);
                        let r = Rect::from_min_max(
                            pos2(x0, rect.top()),
                            pos2(x1.max(x0 + 2.0), rect.bottom()),
                        );
                        lane_p.rect_filled(r, 0.0, ACCENT.gamma_multiply(0.12));
                    }
                }
            }
        }
        // Range band.
        if let Some((lo, hi)) = self.range {
            band(&lane_p, axis, lo, hi, lanes, ACCENT.gamma_multiply(0.15));
            let edge = Stroke::new(1.0, ACCENT.gamma_multiply(0.8));
            lane_p.vline(axis.x(lo), rect.y_range(), edge);
            lane_p.vline(axis.x(hi), rect.y_range(), edge);
        }

        let mut hit = None;
        let mut y = rect.top();
        for row in rows {
            let h = row.height();
            let row_rect = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
            let label_rect =
                Rect::from_min_max(row_rect.min, pos2(axis.left - 4.0, row_rect.bottom()));
            let lane = Rect::from_min_max(pos2(axis.left, y + 1.0), pos2(axis.right, y + h - 1.0));
            y += h;
            if !ui.is_rect_visible(row_rect) {
                continue;
            }

            // Label column.
            let group = row.group();
            let indent = if group.is_some() { 0.0 } else { INDENT };
            if let Some(g) = group {
                painter.rect_filled(
                    Rect::from_min_max(row_rect.min, pos2(axis.left, row_rect.bottom())),
                    0.0,
                    visuals.widgets.noninteractive.bg_fill,
                );
                paint_triangle(
                    &painter,
                    pos2(rect.left() + 9.0, row_rect.center().y),
                    !self.collapsed.contains(&g),
                    strong_col,
                );
            }
            let (label, label_font, label_col) = match row {
                Row::QueueHeader(_, q) => (
                    format!("{}  ({} items)", q.name, q.items.len()),
                    header_font.clone(),
                    strong_col,
                ),
                Row::Markers(_, d) => (
                    if *d == 0 {
                        "Markers".to_owned()
                    } else {
                        format!("Markers · level {d}")
                    },
                    font.clone(),
                    text_col,
                ),
                Row::Items(_) => ("Items".to_owned(), font.clone(), text_col),
                Row::CounterHeader => (
                    "Counters (est.)".to_owned(),
                    header_font.clone(),
                    strong_col,
                ),
                Row::Counter(c, max) => (
                    format!("{}\nmax {} {}", c.name, fmt_num(*max), c.unit),
                    font.clone(),
                    text_col,
                ),
            };
            painter.with_clip_rect(label_rect).text(
                pos2(rect.left() + 18.0 + indent, row_rect.center().y),
                Align2::LEFT_CENTER,
                label,
                label_font,
                label_col,
            );
            painter.line_segment(
                [row_rect.left_bottom(), row_rect.right_bottom()],
                Stroke::new(
                    1.0,
                    visuals
                        .widgets
                        .noninteractive
                        .bg_stroke
                        .color
                        .gamma_multiply(0.5),
                ),
            );

            let row_p = painter.with_clip_rect(lane);
            match row {
                Row::CounterHeader => {}
                Row::Counter(c, max) => {
                    let scale = if *max > 0.0 {
                        (lane.height() - 4.0) as f64 / max
                    } else {
                        0.0
                    };
                    let fill = ACCENT.gamma_multiply(0.4);
                    let edge = Stroke::new(1.0, ACCENT);
                    for s in &c.samples {
                        if !s.value.is_finite() {
                            continue;
                        }
                        let x0 = axis.x(s.start_ns as f64);
                        let x1 = axis.x(s.end_ns as f64).max(x0 + 1.0);
                        if x1 < lane.left() || x0 > lane.right() {
                            continue;
                        }
                        let top = lane.bottom() - (s.value * scale) as f32;
                        row_p.rect_filled(
                            Rect::from_min_max(pos2(x0, top), pos2(x1, lane.bottom())),
                            0.0,
                            fill,
                        );
                        row_p.hline(x0..=x1, top, edge);
                    }
                    if let Some(p) = hover
                        && row_rect.contains(p)
                        && p.x >= axis.left
                        && let Some(s) = counter_at(c, axis.t(p.x))
                    {
                        hit = Some(Hit::Counter(c, s));
                    }
                }
                _ => {
                    for s in row.spans() {
                        let (x0, x1, zero) = span_x(axis, s);
                        if x1 < lane.left() || x0 > lane.right() {
                            continue;
                        }
                        let r = Rect::from_min_max(pos2(x0, lane.top()), pos2(x1, lane.bottom()));
                        let col = span_color(row, s, tree, ordinals);
                        row_p.rect_filled(r, if zero { 0.0 } else { 2.0 }, col);
                        if let Row::Items(_) = row
                            && let Some(n) = tree.get(s.index)
                        {
                            // Kind strip along the bottom; separator on the bar's left edge.
                            row_p.rect_filled(
                                Rect::from_min_max(pos2(r.left(), r.bottom() - 3.0), r.max),
                                0.0,
                                kind_color(&n.kind),
                            );
                            if !zero && r.width() > 2.0 {
                                row_p.vline(r.left(), r.y_range(), sep);
                            }
                        }
                        if selected == Some(s.index) {
                            row_p.rect_stroke(
                                r,
                                2.0,
                                Stroke::new(2.0, Color32::WHITE),
                                StrokeKind::Inside,
                            );
                        }
                        // Labels only inside bars wide enough to hold them; the tooltip has the full text.
                        let visible = r.intersect(lane);
                        if !zero && visible.width() > 6.0 {
                            let fg = text_on(col);
                            let galley = painter.layout_no_wrap(s.label.clone(), font.clone(), fg);
                            if galley.size().x + 6.0 <= visible.width() {
                                row_p.with_clip_rect(visible).galley(
                                    pos2(
                                        visible.left() + 3.0,
                                        r.center().y - galley.size().y / 2.0,
                                    ),
                                    galley,
                                    fg,
                                );
                            }
                        }
                        if let Some(p) = hover
                            && r.expand2(vec2(1.0, 0.0)).contains(p)
                        {
                            hit = Some(Hit::Span(s));
                        }
                    }
                }
            }
        }

        if let Some(x) = crosshair {
            lane_p.vline(
                x,
                rect.y_range(),
                Stroke::new(1.0, strong_col.gamma_multiply(0.6)),
            );
        }
        hit
    }

    fn paint_ruler(&self, ui: &egui::Ui, rect: Rect, axis: Axis, crosshair: Option<f32>) {
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let font = FontId::proportional(11.0);
        let text_col = visuals.text_color();
        let strong_col = visuals.strong_text_color();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
        painter.rect_filled(
            Rect::from_min_max(rect.min, pos2(axis.left, rect.bottom())),
            0.0,
            visuals.faint_bg_color,
        );
        painter.text(
            pos2(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            "time",
            font.clone(),
            visuals.weak_text_color(),
        );
        let lane = Rect::from_min_max(pos2(axis.left, rect.top()), rect.max);
        let lane_p = painter.with_clip_rect(lane);
        if let Some((lo, hi)) = self.range {
            band(&lane_p, axis, lo, hi, lane, ACCENT.gamma_multiply(0.35));
            lane_p.vline(axis.x(lo), rect.y_range(), Stroke::new(1.0, ACCENT));
            lane_p.vline(axis.x(hi), rect.y_range(), Stroke::new(1.0, ACCENT));
        }
        let (step, _) = tick_steps(axis);
        let tick = Stroke::new(1.0, text_col.gamma_multiply(0.7));
        for_each_tick(axis, |t, major| {
            let x = axis.x(t);
            if major {
                lane_p.vline(x, (rect.bottom() - 8.0)..=rect.bottom(), tick);
                lane_p.text(
                    pos2(x + 3.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    fmt_tick(t, step),
                    font.clone(),
                    text_col,
                );
            } else {
                lane_p.vline(x, (rect.bottom() - 4.0)..=rect.bottom(), tick);
            }
        });
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, text_col.gamma_multiply(0.5)),
        );

        if let Some(x) = crosshair {
            lane_p.vline(x, rect.y_range(), Stroke::new(1.0, strong_col));
            let galley = painter.layout_no_wrap(fmt_time(axis.t(x)), font, strong_col);
            let size = galley.size() + vec2(8.0, 4.0);
            let left =
                (x - size.x * 0.5).clamp(lane.left(), (lane.right() - size.x).max(lane.left()));
            let r = Rect::from_min_size(pos2(left, rect.top() + 1.0), size);
            painter.rect_filled(r, 3.0, visuals.widgets.active.bg_fill);
            painter.rect_stroke(r, 3.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
            painter.galley(r.min + vec2(4.0, 2.0), galley, strong_col);
        }
    }
}

fn hit_tooltip(ui: &mut egui::Ui, tr: &TraceResult, hit: &Hit, t: Option<f64>, labels: &[String]) {
    match hit {
        Hit::Span(s) => {
            ui.strong(&s.label);
            if let Some(l) = labels.get(s.index)
                && *l != s.label
            {
                ui.weak(l);
            }
            ui.label(format!("command #{}", s.index));
            ui.label(format!("duration {:.3} µs", dur_ns(s) as f64 / 1e3));
            ui.label(format!("start {}", fmt_time(s.start_ns as f64)));
            let t_at = t
                .unwrap_or(s.start_ns as f64)
                .clamp(s.start_ns as f64, s.end_ns.max(s.start_ns) as f64);
            for c in &tr.counters {
                if let Some(cs) = counter_at(c, t_at) {
                    ui.label(format!(
                        "{}: {} {} (total {})",
                        c.name,
                        fmt_num(cs.value),
                        c.unit,
                        fmt_num(cs.total)
                    ));
                }
            }
        }
        Hit::Counter(c, cs) => {
            ui.strong(&c.name);
            ui.label(format!("{} {}", fmt_num(cs.value), c.unit));
            ui.label(format!("total {}", fmt_num(cs.total)));
            ui.label(format!(
                "{} – {}",
                fmt_time(cs.start_ns as f64),
                fmt_time(cs.end_ns as f64)
            ));
            if let Some(item) = tr
                .queues
                .iter()
                .flat_map(|q| &q.items)
                .find(|i| i.start_ns == cs.start_ns && i.end_ns == cs.end_ns)
            {
                ui.label(format!("item #{}: {}", item.index, item.label));
            }
        }
    }
}

/// Bottom table for the selected range; returns a command index clicked in the top-items list.
fn range_summary(
    ui: &mut egui::Ui,
    tr: &TraceResult,
    (lo, hi): (f64, f64),
    selected: Option<usize>,
) -> Option<usize> {
    let mut items: Vec<&Span> = tr
        .queues
        .iter()
        .flat_map(|q| &q.items)
        .filter(|s| in_range(s, lo, hi))
        .collect();
    let item_time: f64 = items
        .iter()
        .map(|s| overlap(s.start_ns as f64, s.end_ns as f64, lo, hi))
        .sum();
    ui.horizontal(|ui| {
        ui.strong("Range summary");
        ui.label(format!("{} – {}", fmt_time(lo), fmt_time(hi)));
        ui.separator();
        ui.label(format!("duration {}", fmt_ns(hi - lo)));
        ui.separator();
        ui.label(format!("{} items", items.len()));
        ui.separator();
        ui.label(format!("item time {}", fmt_ns(item_time)));
    });
    ui.separator();
    items.sort_by(|a, b| dur_ns(b).cmp(&dur_ns(a)).then(a.index.cmp(&b.index)));
    let mut pick = None;
    ui.columns(2, |cols| {
        egui::ScrollArea::vertical()
            .id_salt("trace-range-counters")
            .auto_shrink([false, false])
            .show(&mut cols[0], |ui| {
                egui::Grid::new("trace-range-counters-grid")
                    .striped(true)
                    .num_columns(3)
                    .show(ui, |ui| {
                        ui.strong("Counter");
                        ui.strong("Average");
                        ui.strong("Total");
                        ui.end_row();
                        for c in &tr.counters {
                            let (avg, total) = counter_over(c, lo, hi);
                            ui.label(c.name.as_str());
                            ui.label(avg.map_or_else(
                                || "n/a".to_owned(),
                                |a| format!("{} {}", fmt_num(a), c.unit),
                            ));
                            // Percent tracks have no meaningful sum.
                            ui.label(if c.unit == "%" {
                                "—".to_owned()
                            } else {
                                let unit = c.unit.strip_suffix("/ns").unwrap_or(&c.unit);
                                format!("{} {unit}", fmt_num(total))
                            });
                            ui.end_row();
                        }
                    });
            });
        egui::ScrollArea::vertical()
            .id_salt("trace-range-top-items")
            .auto_shrink([false, false])
            .show(&mut cols[1], |ui| {
                ui.strong(format!("Top {TOP_ITEMS} items by duration"));
                if items.is_empty() {
                    ui.weak("No items in range.");
                    return;
                }
                egui::Grid::new("trace-range-top-items-grid")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        ui.strong("#");
                        ui.strong("Command");
                        ui.strong("Duration");
                        ui.strong("Start");
                        ui.end_row();
                        for s in items.iter().take(TOP_ITEMS) {
                            let sel = selected == Some(s.index);
                            if ui.selectable_label(sel, format!("#{}", s.index)).clicked() {
                                pick = Some(s.index);
                            }
                            if ui.selectable_label(sel, s.label.as_str()).clicked() {
                                pick = Some(s.index);
                            }
                            ui.label(fmt_ns(dur_ns(s) as f64));
                            ui.label(fmt_time(s.start_ns as f64));
                            ui.end_row();
                        }
                    });
            });
    });
    pick
}
