//! Trace panel: hand-painted GPU timeline (queues, markers, items) + estimated counter rows.
use crate::replay_queue::get_replayer;
use crate::state::AppState;
use crate::widgets::{fmt_ns, fmt_num};
use egui::{Align2, Color32, FontId, Key, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use std::cell::RefCell;
use std::rc::Rc;
use webgd_replay::{CounterTrack, QueueTrack, Span, TraceResult};

const LABEL_W: f32 = 200.0;
const AXIS_H: f32 = 20.0;
const PASS_H: f32 = 20.0;
const MARKER_H: f32 = 16.0;
const ITEMS_H: f32 = 20.0;
const COUNTER_H: f32 = 40.0;

#[derive(Default)]
struct Shared {
    running: bool,
    result: Option<(u64, Result<TraceResult, String>)>,
}

#[derive(Default)]
pub struct TraceUi {
    shared: Rc<RefCell<Shared>>,
    /// Visible time window in ns.
    view: (f64, f64),
    fit: bool,
}

enum Row<'a> {
    Passes(&'a QueueTrack),
    Markers(&'a QueueTrack, u8),
    Items(&'a QueueTrack),
    Counter(&'a CounterTrack, f64),
}

impl Row<'_> {
    fn height(&self) -> f32 {
        match self {
            Row::Passes(_) => PASS_H,
            Row::Markers(..) => MARKER_H,
            Row::Items(_) => ITEMS_H,
            Row::Counter(..) => COUNTER_H,
        }
    }
    fn spans(&self) -> Vec<&Span> {
        match self {
            Row::Passes(q) => q.passes.iter().collect(),
            Row::Markers(q, d) => q.markers.iter().filter(|s| s.depth == *d).collect(),
            Row::Items(q) => q.items.iter().collect(),
            Row::Counter(..) => Vec::new(),
        }
    }
}

fn color_for(label: &str) -> Color32 {
    let h = label
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.65, 1.0).into()
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

/// Counter value at time `t` (sample covering it).
fn counter_at(c: &CounterTrack, t: f64) -> Option<&webgd_replay::CounterSample> {
    c.samples
        .iter()
        .find(|s| (s.start_ns as f64) <= t && t <= s.end_ns as f64)
}

impl TraceUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        let generation = l.generation;
        let running = self.shared.borrow().running;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Run trace"))
                .clicked()
            {
                self.shared.borrow_mut().running = true;
                let (shared, cell, cap, ctx) = (
                    self.shared.clone(),
                    st.replay.replayer_cell(),
                    l.cap.clone(),
                    st.ctx.clone(),
                );
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
                self.fit = true;
            }
            if running {
                ui.spinner();
                ui.label("tracing (5 instrumented replays)…");
            }
            ui.weak("ctrl+wheel: zoom · drag: pan · F: fit · click: select command");
        });
        let shared = self.shared.clone();
        let sh = shared.borrow();
        let Some((g, res)) = &sh.result else {
            ui.weak("No trace yet.");
            return;
        };
        if *g != generation {
            ui.weak("No trace for this capture yet.");
            return;
        }
        let tr = match res {
            Ok(tr) => tr,
            Err(e) => {
                ui.colored_label(Color32::LIGHT_RED, format!("trace failed: {e}"));
                return;
            }
        };
        for n in &tr.notes {
            ui.label(RichText::new(format!("• {n}")).color(crate::widgets::WARN));
        }
        let mut rows: Vec<Row> = Vec::new();
        for q in &tr.queues {
            rows.push(Row::Passes(q));
            let max_depth = q.markers.iter().map(|m| m.depth).max();
            if let Some(md) = max_depth {
                for d in 0..=md {
                    rows.push(Row::Markers(q, d));
                }
            }
            rows.push(Row::Items(q));
        }
        for c in &tr.counters {
            let max = c
                .samples
                .iter()
                .map(|s| s.value)
                .filter(|v| v.is_finite())
                .fold(0.0, f64::max);
            rows.push(Row::Counter(c, max));
        }
        let total = tr.total_ns.max(1) as f64;
        if self.fit || self.view.1 <= self.view.0 {
            self.view = (0.0, total);
            self.fit = false;
        }
        let selected = st.selected;
        let mut select = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let height = AXIS_H + rows.iter().map(Row::height).sum::<f32>();
                let (rect, resp) = ui.allocate_exact_size(
                    vec2(ui.available_width(), height),
                    Sense::click_and_drag(),
                );
                let tl = Rect::from_min_max(pos2(rect.left() + LABEL_W, rect.top()), rect.max);
                let (t0, t1) = self.view;
                let ns_per_px = (t1 - t0) / tl.width().max(1.0) as f64;
                let to_x = |ns: f64| tl.left() + ((ns - t0) / ns_per_px) as f32;
                let to_t = |x: f32| t0 + (x - tl.left()) as f64 * ns_per_px;

                // Interaction: zoom around cursor, drag pan, F fit.
                if resp.hovered() {
                    let (zoom, hover, fit) = ui
                        .input(|i| (i.zoom_delta(), i.pointer.hover_pos(), i.key_pressed(Key::F)));
                    if fit {
                        self.view = (0.0, total);
                    } else if zoom != 1.0
                        && let Some(p) = hover
                    {
                        let anchor = to_t(p.x.max(tl.left()));
                        let span = ((t1 - t0) / zoom as f64).clamp(10.0, total * 4.0);
                        let frac = ((p.x.max(tl.left()) - tl.left()) / tl.width().max(1.0)) as f64;
                        self.view = (anchor - span * frac, anchor - span * frac + span);
                    }
                }
                if resp.dragged() {
                    let dt = -resp.drag_delta().x as f64 * ns_per_px;
                    self.view = (self.view.0 + dt, self.view.1 + dt);
                }

                let painter = ui.painter_at(rect);
                let font = FontId::proportional(11.0);
                let text_col = ui.visuals().text_color();
                painter.rect_filled(
                    Rect::from_min_max(rect.min, pos2(tl.left(), rect.bottom())),
                    0.0,
                    ui.visuals().faint_bg_color,
                );
                // Axis + grid.
                let step = nice_step(80.0 * ns_per_px);
                let mut t = (t0 / step).floor() * step;
                while t <= t1 {
                    let x = to_x(t);
                    if x >= tl.left() {
                        painter.line_segment(
                            [pos2(x, rect.top() + AXIS_H - 5.0), pos2(x, rect.bottom())],
                            Stroke::new(1.0, Color32::from_gray(50)),
                        );
                        painter.text(
                            pos2(x + 2.0, rect.top() + 2.0),
                            Align2::LEFT_TOP,
                            fmt_ns(t),
                            font.clone(),
                            text_col,
                        );
                    }
                    t += step;
                }
                painter.line_segment(
                    [
                        pos2(rect.left(), rect.top() + AXIS_H),
                        pos2(rect.right(), rect.top() + AXIS_H),
                    ],
                    Stroke::new(1.0, Color32::GRAY),
                );

                let hover = resp.hover_pos();
                let mut hovered: Option<(&Span, Option<&CounterTrack>)> = None;
                let mut y = rect.top() + AXIS_H;
                for row in &rows {
                    let h = row.height();
                    let row_rect = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
                    let lane =
                        Rect::from_min_max(pos2(tl.left(), y + 1.0), pos2(tl.right(), y + h - 1.0));
                    let label = match row {
                        Row::Passes(q) => q.name.clone(),
                        Row::Markers(_, d) => format!("  markers [{d}]"),
                        Row::Items(_) => "  Items".to_owned(),
                        Row::Counter(c, max) => {
                            format!("{}\nmax {} {}", c.name, fmt_num(*max), c.unit)
                        }
                    };
                    painter
                        .with_clip_rect(Rect::from_min_max(
                            row_rect.min,
                            pos2(tl.left() - 4.0, row_rect.bottom()),
                        ))
                        .text(
                            pos2(rect.left() + 4.0, row_rect.center().y),
                            Align2::LEFT_CENTER,
                            label,
                            font.clone(),
                            text_col,
                        );
                    painter.line_segment(
                        [row_rect.left_bottom(), row_rect.right_bottom()],
                        Stroke::new(1.0, Color32::from_gray(40)),
                    );
                    let lane_p = painter.with_clip_rect(lane);
                    match row {
                        Row::Counter(c, max) => {
                            let scale = if *max > 0.0 {
                                (h - 6.0) as f64 / max
                            } else {
                                0.0
                            };
                            let fill = color_for(&c.name).gamma_multiply(0.8);
                            for s in &c.samples {
                                let x0 = to_x(s.start_ns as f64);
                                let x1 = to_x(s.end_ns as f64).max(x0 + 1.0);
                                if x1 < tl.left() || x0 > tl.right() || !s.value.is_finite() {
                                    continue;
                                }
                                let top = lane.bottom() - (s.value * scale) as f32;
                                lane_p.rect_filled(
                                    Rect::from_min_max(pos2(x0, top), pos2(x1, lane.bottom())),
                                    0.0,
                                    fill,
                                );
                            }
                            if let Some(p) = hover
                                && row_rect.contains(p)
                                && p.x >= tl.left()
                                && let Some(s) = c.samples.iter().find(|s| {
                                    let x0 = to_x(s.start_ns as f64);
                                    p.x >= x0 && p.x <= to_x(s.end_ns as f64).max(x0 + 1.0)
                                })
                            {
                                let span = rows.iter().find_map(|r| match r {
                                    Row::Items(q) => q
                                        .items
                                        .iter()
                                        .find(|i| i.start_ns == s.start_ns && i.end_ns == s.end_ns),
                                    _ => None,
                                });
                                if let Some(span) = span {
                                    hovered = Some((span, Some(c)));
                                }
                            }
                        }
                        _ => {
                            for s in row.spans() {
                                let x0 = to_x(s.start_ns as f64);
                                let zero = s.end_ns <= s.start_ns;
                                let x1 = if zero {
                                    x0 + 2.0
                                } else {
                                    to_x(s.end_ns as f64).max(x0 + 1.0)
                                };
                                if x1 < tl.left() || x0 > tl.right() {
                                    continue;
                                }
                                let r = Rect::from_min_max(
                                    pos2(x0, lane.top()),
                                    pos2(x1, lane.bottom()),
                                );
                                let col = color_for(&s.label);
                                lane_p.rect_filled(r, 2.0, col);
                                if matches!(row, Row::Items(_)) && selected == Some(s.index) {
                                    lane_p.rect_stroke(
                                        r,
                                        2.0,
                                        Stroke::new(2.0, Color32::WHITE),
                                        StrokeKind::Inside,
                                    );
                                }
                                // Zero-width spans are bare ticks; labels only inside bars wide enough to hold them
                                // (quantized timestamps stack many spans at one x). Full label is in the tooltip.
                                let visible = r.intersect(lane);
                                if !zero && visible.width() > 6.0 {
                                    let galley = painter.layout_no_wrap(
                                        s.label.clone(),
                                        font.clone(),
                                        Color32::BLACK,
                                    );
                                    if galley.size().x + 6.0 <= visible.width() {
                                        lane_p.with_clip_rect(visible).galley(
                                            pos2(
                                                visible.left() + 3.0,
                                                r.center().y - galley.size().y / 2.0,
                                            ),
                                            galley,
                                            Color32::BLACK,
                                        );
                                    }
                                }
                                if let Some(p) = hover
                                    && r.expand2(vec2(1.0, 0.0)).contains(p)
                                {
                                    hovered = Some((s, None));
                                }
                            }
                        }
                    }
                    y += h;
                }
                if let Some((span, _)) = hovered {
                    let t_mid = hover.map(|p| to_t(p.x)).unwrap_or(span.start_ns as f64);
                    let t_at =
                        t_mid.clamp(span.start_ns as f64, span.end_ns.max(span.start_ns) as f64);
                    let counters = &tr.counters;
                    resp.clone().on_hover_ui_at_pointer(|ui| {
                        ui.strong(&span.label);
                        ui.label(format!("command #{}", span.index));
                        ui.label(format!(
                            "duration {:.3} µs",
                            (span.end_ns.saturating_sub(span.start_ns)) as f64 / 1e3
                        ));
                        ui.label(format!("start {}", fmt_ns(span.start_ns as f64)));
                        for c in counters {
                            if let Some(s) = counter_at(c, t_at) {
                                ui.label(format!(
                                    "{}: {} {} (total {})",
                                    c.name,
                                    fmt_num(s.value),
                                    c.unit,
                                    fmt_num(s.total)
                                ));
                            }
                        }
                    });
                    if resp.clicked() {
                        select = Some(span.index);
                    }
                }
            });
        drop(sh);
        if let Some(i) = select {
            st.select(i);
        }
    }
}
