//! Geometry panel: vertex shader inputs (pre-VS) or outputs (post-VS) of the selected draw as an
//! orbitable 3D view plus a vertex table organised by the vertex input layout.
use crate::replay_queue::Key;
use crate::state::{AppState, Loaded};
use crate::widgets::{ACCENT, ERROR, banner};
use egui::{
    Align, Align2, Color32, FontId, Id, Layout, Mesh, PointerButton, Pos2, Rect, RichText, Sense,
    Stroke, TextStyle, UiBuilder, Vec2, pos2, vec2,
};
use std::collections::HashMap;
use webgd_core::ObjId;
use webgd_core::display::obj_label;
use webgd_core::geometry::{AttribStream, Primitive, ScalarKind, assemble};
use webgd_core::track::commands::{CmdKind, classify};
use webgd_replay::GeometryData;

/// Vertical field of view of the 3D view camera.
const FOV_Y: f64 = std::f64::consts::FRAC_PI_4;
/// Width of one value component column in the vertex table.
const COMP_W: f32 = 78.0;
const IDX_W: f32 = 64.0;
const VTX_W: f32 = 72.0;
/// Pointer distance (px) within which a projected vertex counts as hovered.
const HOVER_PX: f32 = 8.0;

const BG: Color32 = Color32::from_gray(18);
const WIRE: Color32 = Color32::from_rgb(110, 190, 230);
const POINT: Color32 = Color32::from_rgb(235, 220, 140);
const NDC_BOX: Color32 = Color32::from_gray(70);
const FRONT: [f32; 3] = [150.0, 175.0, 205.0];
const BACK: [f32; 3] = [225.0, 110.0, 110.0];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
enum Stage {
    #[default]
    PreVs,
    PostVs,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
enum DrawMode {
    #[default]
    Wireframe,
    Solid,
    Points,
}

impl DrawMode {
    fn label(self) -> &'static str {
        match self {
            DrawMode::Wireframe => "Wireframe",
            DrawMode::Solid => "Solid",
            DrawMode::Points => "Points",
        }
    }
}

/// Inputs the cached [`Scene`] was derived from.
#[derive(Clone, Copy, PartialEq, Debug)]
struct SceneKey {
    key: Key,
    instance: u32,
    stage: Stage,
    /// Position stream index (inputs for pre-VS, outputs for post-VS).
    source: Option<usize>,
    /// Post-VS without perspective divide.
    clip_space: bool,
}

/// What the camera was last fitted to; a change refits (instance changes keep the view).
type FitId = (Key, Stage, Option<usize>, bool);

type V3 = [f64; 3];

/// Positions and primitives of one geometry result, derived once per [`SceneKey`].
struct Scene {
    /// Position per vertex row (aligned to `GeometryData::vertex_ids`); None = not drawable.
    pos: Vec<Option<V3>>,
    /// Vertex id → row.
    rows: HashMap<u32, usize>,
    prims: Vec<Primitive>,
    bounds: Option<(V3, V3)>,
    notes: Vec<String>,
    /// Why nothing can be drawn.
    error: Option<String>,
}

impl Scene {
    fn build(g: &GeometryData, streams: &[AttribStream], k: &SceneKey) -> Scene {
        let rows = g
            .vertex_ids
            .iter()
            .enumerate()
            .map(|(r, &id)| (id, r))
            .collect();
        let prims = assemble(g.topology, &g.draw_order, g.restart);
        let mut pos = vec![None; g.vertex_ids.len()];
        let mut notes = Vec::new();
        let mut error = None;
        match k.source.and_then(|i| streams.get(i)) {
            None => {
                error = Some(
                    match k.stage {
                        Stage::PreVs => {
                            "No vertex input with at least 2 components to use as position."
                        }
                        Stage::PostVs => "The vertex shader outputs carry no position.",
                    }
                    .to_owned(),
                );
            }
            Some(s) => {
                let divide = k.stage == Stage::PostVs && !k.clip_space;
                let (mut missing, mut behind) = (0usize, 0usize);
                for (r, p) in pos.iter_mut().enumerate() {
                    let Some(v) = s.values.get(r).copied().flatten() else {
                        missing += 1;
                        continue;
                    };
                    let z = if s.components >= 3 { v[2] } else { 0.0 };
                    let q = if divide {
                        if v[3] > 0.0 {
                            [v[0] / v[3], v[1] / v[3], z / v[3]]
                        } else {
                            behind += 1;
                            continue;
                        }
                    } else {
                        [v[0], v[1], z]
                    };
                    if q.iter().all(|c| c.is_finite()) {
                        *p = Some(q);
                    } else {
                        missing += 1;
                    }
                }
                if missing > 0 {
                    notes.push(format!(
                        "{missing} vertices have no finite value for {}; they are not drawn.",
                        s.name
                    ));
                }
                if behind > 0 {
                    notes.push(format!(
                        "{behind} vertices have clip w ≤ 0 and are not drawn in NDC; enable \
                         \"Clip space (no divide)\" to see them."
                    ));
                }
            }
        }
        let bounds = pos.iter().flatten().fold(None, |acc: Option<(V3, V3)>, p| {
            Some(match acc {
                None => (*p, *p),
                Some((lo, hi)) => (
                    [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                    [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
                ),
            })
        });
        Scene {
            pos,
            rows,
            prims,
            bounds,
            notes,
            error,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Camera {
    target: V3,
    yaw: f64,
    pitch: f64,
    dist: f64,
    /// Bounding radius the camera was fitted to (bounds the zoom range).
    radius: f64,
}

impl Camera {
    fn fit(bounds: (V3, V3), stage: Stage) -> Camera {
        let target = scale(add(bounds.0, bounds.1), 0.5);
        let mut radius = length(sub(bounds.1, bounds.0)) * 0.5;
        if !(radius > 1e-12 && radius.is_finite()) {
            radius = 1.0;
        }
        // Post-VS looks straight down the clip-space z axis (x right, y up, like the screen).
        let (yaw, pitch) = match stage {
            Stage::PreVs => (0.6, 0.35),
            Stage::PostVs => (0.0, 0.0),
        };
        Camera {
            target,
            yaw,
            pitch,
            dist: radius / (FOV_Y * 0.5).sin() * 1.1,
            radius,
        }
    }

    /// Unit vector from the target towards the eye.
    fn dir(&self) -> V3 {
        [
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        ]
    }

    fn orbit(&mut self, d: Vec2) {
        self.yaw -= d.x as f64 * 0.01;
        self.pitch = (self.pitch + d.y as f64 * 0.01).clamp(-1.55, 1.55);
    }

    fn pan(&mut self, d: Vec2, proj: &Proj) {
        let k = self.dist / proj.focal;
        self.target = add(
            self.target,
            add(
                scale(proj.r, -d.x as f64 * k),
                scale(proj.u, d.y as f64 * k),
            ),
        );
    }

    fn zoom(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.dist = (self.dist * factor).clamp(self.radius * 1e-4, self.radius * 1e4);
        }
    }
}

/// Perspective projection of the current camera into a screen rect.
struct Proj {
    eye: V3,
    r: V3,
    u: V3,
    f: V3,
    center: Pos2,
    focal: f64,
    near: f64,
}

impl Proj {
    fn new(cam: &Camera, rect: Rect) -> Proj {
        let eye = add(cam.target, scale(cam.dir(), cam.dist));
        let f = scale(cam.dir(), -1.0);
        let r = unit(cross(f, [0.0, 1.0, 0.0]));
        let u = cross(r, f);
        Proj {
            eye,
            r,
            u,
            f,
            center: rect.center(),
            focal: (rect.width().min(rect.height()).max(1.0) as f64 * 0.5) / (FOV_Y * 0.5).tan(),
            near: cam.dist * 1e-3,
        }
    }

    /// Screen position and view depth, or None behind the near plane.
    fn project(&self, p: V3) -> Option<(Pos2, f64)> {
        let d = sub(p, self.eye);
        let z = dot(d, self.f);
        if z.is_nan() || z <= self.near {
            return None;
        }
        let x = dot(d, self.r) / z * self.focal;
        let y = dot(d, self.u) / z * self.focal;
        Some((pos2(self.center.x + x as f32, self.center.y - y as f32), z))
    }
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}
/// Normalized `a`, or None for (near-)zero / non-finite vectors.
fn normalized(a: V3) -> Option<V3> {
    let l = length(a);
    (l > 1e-300 && l.is_finite()).then(|| scale(a, 1.0 / l))
}
fn unit(a: V3) -> V3 {
    normalized(a).unwrap_or([1.0, 0.0, 0.0])
}

/// Thin screen-space quad from `a` to `b`.
fn push_line(mesh: &mut Mesh, a: Pos2, b: Pos2, width: f32, color: Color32) {
    let d = b - a;
    let len = d.length();
    if len.is_nan() || len <= 1e-3 {
        return;
    }
    let n = vec2(-d.y, d.x) / len * (width * 0.5);
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(a + n, color);
    mesh.colored_vertex(a - n, color);
    mesh.colored_vertex(b - n, color);
    mesh.colored_vertex(b + n, color);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

fn push_point(mesh: &mut Mesh, p: Pos2, half: f32, color: Color32) {
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(p + vec2(-half, -half), color);
    mesh.colored_vertex(p + vec2(half, -half), color);
    mesh.colored_vertex(p + vec2(half, half), color);
    mesh.colored_vertex(p + vec2(-half, half), color);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

fn shade(base: [f32; 3], k: f32) -> Color32 {
    let c = |v: f32| (v * k).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c(base[0]), c(base[1]), c(base[2]))
}

fn fmt_float(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    let a = v.abs();
    if a != 0.0 && !(1e-4..1e6).contains(&a) {
        return format!("{v:.3e}");
    }
    let mut s = format!("{v:.5}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.push('0');
    }
    s
}

fn fmt_comp(v: f64, kind: ScalarKind) -> String {
    match kind {
        ScalarKind::Float => fmt_float(v),
        ScalarKind::Sint | ScalarKind::Uint => {
            if v.is_finite() && v.fract() == 0.0 {
                format!("{}", v as i64)
            } else {
                format!("{v}")
            }
        }
    }
}

fn fmt_value(v: Option<[f64; 4]>, components: u32, kind: ScalarKind) -> String {
    match v {
        None => "—".to_owned(),
        Some(v) => (0..components.clamp(1, 4) as usize)
            .map(|c| fmt_comp(v[c], kind))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// `@0 a_position · float32x3` (location omitted when unknown).
fn stream_label(s: &AttribStream) -> String {
    match s.location {
        Some(loc) => format!("@{loc} {} · {}", s.name, s.format),
        None => format!("{} · {}", s.name, s.format),
    }
}

/// Pre-VS position candidates: inputs with at least two components.
fn position_candidates(inputs: &[AttribStream]) -> Vec<usize> {
    (0..inputs.len())
        .filter(|&i| inputs[i].components >= 2)
        .collect()
}

/// Default pre-VS position: a name containing "pos", else location 0, else the first candidate.
fn default_position(inputs: &[AttribStream], cands: &[usize]) -> Option<usize> {
    cands
        .iter()
        .copied()
        .find(|&i| inputs[i].name.to_lowercase().contains("pos"))
        .or_else(|| {
            cands
                .iter()
                .copied()
                .find(|&i| inputs[i].location == Some(0))
        })
        .or_else(|| cands.first().copied())
}

/// One value column group of the vertex table (one per attribute / output).
struct Col {
    header: String,
    stream: Option<usize>,
    components: u32,
    kind: ScalarKind,
}

impl Col {
    fn components(&self) -> u32 {
        self.components.clamp(1, 4)
    }
    fn width(&self) -> f32 {
        self.components().max(2) as f32 * COMP_W
    }
    fn comp_w(&self) -> f32 {
        self.width() / self.components() as f32
    }
}

/// Header group spanning the columns of one vertex buffer slot (or the shader outputs).
struct Group {
    title: String,
    hover: String,
    link: Option<ObjId>,
    cols: Vec<Col>,
}

impl Group {
    fn width(&self) -> f32 {
        self.cols.iter().map(Col::width).sum()
    }
}

/// Pre-VS columns: one group per layout buffer, one column group per attribute; inputs not
/// covered by the layout go to a trailing "Other inputs" group.
fn input_groups(g: &GeometryData) -> Vec<Group> {
    let mut used = vec![false; g.inputs.len()];
    let mut groups = Vec::new();
    for b in &g.layout {
        let step = if !b.per_instance {
            "per-vertex".to_owned()
        } else if b.divisor > 1 {
            format!("per-instance (divisor {})", b.divisor)
        } else {
            "per-instance".to_owned()
        };
        let title = match b.buffer {
            Some(_) => format!("Slot {} · stride {} · {step} · ", b.slot, b.stride),
            None => format!("Slot {} · stride {} · {step} · no buffer", b.slot, b.stride),
        };
        let mut cols = Vec::new();
        for a in &b.attributes {
            let si = g
                .inputs
                .iter()
                .position(|s| s.slot == Some(b.slot) && s.location == Some(a.location));
            if let Some(i) = si {
                used[i] = true;
            }
            let s = si.map(|i| &g.inputs[i]);
            let name = match (a.name.is_empty(), s) {
                (false, _) => a.name.clone(),
                (true, Some(s)) => s.name.clone(),
                (true, None) => String::new(),
            };
            let format = a.format.label();
            let header = if name.is_empty() {
                format!("@{} · {format}", a.location)
            } else {
                format!("@{} {name} · {format}", a.location)
            };
            cols.push(Col {
                header,
                stream: si,
                components: s.map_or_else(|| a.format.components(), |s| s.components),
                kind: s.map_or_else(|| a.format.kind(), |s| s.kind),
            });
        }
        if cols.is_empty() {
            continue;
        }
        groups.push(Group {
            hover: format!(
                "{title}{}\nbinding offset {} bytes",
                b.buffer
                    .map(|id| format!("buffer #{}", id.0))
                    .unwrap_or_default(),
                b.buffer_offset
            ),
            title,
            link: b.buffer,
            cols,
        });
    }
    let other: Vec<Col> = (0..g.inputs.len())
        .filter(|&i| !used[i])
        .map(|i| {
            let s = &g.inputs[i];
            Col {
                header: stream_label(s),
                stream: Some(i),
                components: s.components,
                kind: s.kind,
            }
        })
        .collect();
    if !other.is_empty() {
        groups.push(Group {
            title: "Other inputs".to_owned(),
            hover: "Inputs not fed by a vertex buffer of the layout".to_owned(),
            link: None,
            cols: other,
        });
    }
    groups
}

/// Post-VS columns: one group with every output stream, position first.
fn output_groups(outputs: &[AttribStream], position: Option<usize>, instance: u32) -> Vec<Group> {
    let mut order: Vec<usize> = (0..outputs.len()).collect();
    if let Some(p) = position.filter(|&p| p < outputs.len()) {
        order.retain(|&i| i != p);
        order.insert(0, p);
    }
    let title = format!("Vertex shader outputs · instance {instance}");
    vec![Group {
        hover: title.clone(),
        title,
        link: None,
        cols: order
            .into_iter()
            .map(|i| {
                let s = &outputs[i];
                Col {
                    header: stream_label(s),
                    stream: Some(i),
                    components: s.components,
                    kind: s.kind,
                }
            })
            .collect(),
    }]
}

/// Allocates exactly `w`×`h` and lays `add` out clipped inside it (left-aligned, 4 px padding).
fn cell<R>(ui: &mut egui::Ui, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (_, rect) = ui.allocate_space(vec2(w, h));
    let inner = rect.shrink2(vec2(4.0, 0.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    add(&mut child)
}

fn text_cell(ui: &mut egui::Ui, w: f32, h: f32, text: RichText) -> egui::Response {
    cell(ui, w, h, |ui| {
        ui.add(egui::Label::new(text).selectable(false))
    })
}

fn vertex_tooltip(ui: &mut egui::Ui, g: &GeometryData, streams: &[AttribStream], row: usize) {
    let Some(&vid) = g.vertex_ids.get(row) else {
        return;
    };
    ui.label(RichText::new(format!("VTX {vid}")).strong());
    egui::Grid::new("webgd-geometry-vertex-tip")
        .num_columns(2)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            for s in streams {
                ui.label(stream_label(s));
                ui.monospace(fmt_value(
                    s.values.get(row).copied().flatten(),
                    s.components,
                    s.kind,
                ));
                ui.end_row();
            }
        });
}

#[derive(Default)]
pub struct GeometryUi {
    /// Shown this frame (reset by [`GeometryUi::begin_frame`]); geometry is only requested while shown.
    visible: bool,
    stage: Stage,
    mode: DrawMode,
    backface_tint: bool,
    clip_space: bool,
    instance: u32,
    /// Chosen pre-VS position input (index into `GeometryData::inputs`); None = default.
    pos_input: Option<usize>,
    /// (capture generation, frame index) the per-draw state belongs to.
    draw: Option<(u64, usize)>,
    selected_vertex: Option<u32>,
    /// Draw-order row to scroll the table to on the next frame.
    scroll_to: Option<usize>,
    camera: Option<(FitId, Camera)>,
    scene: Option<(SceneKey, Scene)>,
}

impl GeometryUi {
    /// Called once per frame before the dock is drawn.
    pub fn begin_frame(&mut self) {
        self.visible = false;
    }

    /// Instance to extract geometry for, when the panel was shown and `selected` is a draw call.
    pub fn wanted(&self, l: &Loaded, selected: Option<usize>) -> Option<u32> {
        if !self.visible {
            return None;
        }
        let sel = selected?;
        let call = l.cap.frame.get(sel)?;
        if !matches!(classify(&l.cap, call), CmdKind::Draw) {
            return None;
        }
        Some(if self.draw == Some((l.generation, sel)) {
            self.instance
        } else {
            0
        })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        self.visible = true;
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        let draw_sel = st.selected.filter(|&i| {
            l.cap
                .frame
                .get(i)
                .is_some_and(|c| matches!(classify(&l.cap, c), CmdKind::Draw))
        });
        let Some(sel) = draw_sel else {
            ui.weak("Select a draw call to view its geometry.");
            return;
        };
        if self.draw != Some((l.generation, sel)) {
            self.draw = Some((l.generation, sel));
            self.instance = 0;
            self.pos_input = None;
            self.selected_vertex = None;
            self.scroll_to = None;
            self.camera = None;
        }
        let key = Key {
            generation: l.generation,
            stop: Some(sel),
        };
        let mut open = None;
        let shown = st.replay.results(|r| match (&r.geometry, &r.replay) {
            (Some((k, inst, res)), _) if *k == key => {
                let loading = *inst != self.instance;
                match res {
                    Ok(g) => self.show_geometry(ui, l, key, g, loading, &mut open),
                    Err(e) => {
                        ui.horizontal(|ui| {
                            ui.colored_label(ERROR, format!("geometry unavailable: {e}"));
                            if loading {
                                ui.spinner();
                            }
                        });
                    }
                }
                true
            }
            (_, Some((k, Err(e)))) if *k == key => {
                ui.colored_label(ERROR, format!("replay failed: {e}"));
                true
            }
            _ => false,
        });
        if !shown {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("extracting geometry of #{sel}…"));
            });
        }
        if let Some(id) = open {
            st.open_resource(id);
        }
    }

    fn show_geometry(
        &mut self,
        ui: &mut egui::Ui,
        l: &Loaded,
        key: Key,
        g: &GeometryData,
        loading: bool,
        open: &mut Option<ObjId>,
    ) {
        let outputs = g.outputs.as_ref();
        if outputs.is_err() {
            self.stage = Stage::PreVs;
        }
        if g.instance_count > 0 && self.instance >= g.instance_count {
            self.instance = g.instance_count - 1;
        }
        let cands = position_candidates(&g.inputs);
        if self.pos_input.is_none_or(|i| !cands.contains(&i)) {
            self.pos_input = default_position(&g.inputs, &cands);
        }

        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.stage, Stage::PreVs, "Pre-VS")
                .on_hover_text("Vertex shader inputs fetched from the bound vertex buffers");
            let post = ui.add_enabled(
                outputs.is_ok(),
                egui::Button::selectable(self.stage == Stage::PostVs, "Post-VS"),
            );
            let post = match outputs {
                Ok(_) => post.on_hover_text("Vertex shader outputs computed on the replay device"),
                Err(reason) => post.on_disabled_hover_text(reason.as_str()),
            };
            if post.clicked() {
                self.stage = Stage::PostVs;
            }
            ui.separator();
            for m in [DrawMode::Wireframe, DrawMode::Solid, DrawMode::Points] {
                ui.selectable_value(&mut self.mode, m, m.label());
            }
            if self.mode == DrawMode::Solid {
                ui.checkbox(&mut self.backface_tint, "Tint back faces")
                    .on_hover_text("Back faces assume counter-clockwise front faces");
            }
            ui.separator();
            match (self.stage, outputs) {
                (Stage::PostVs, Ok(outs)) => {
                    let pos = g
                        .position_output
                        .and_then(|i| outs.get(i))
                        .map_or_else(|| "none".to_owned(), stream_label);
                    ui.label(format!("Position: {pos}"));
                    ui.checkbox(&mut self.clip_space, "Clip space (no divide)")
                        .on_hover_text("Unchecked: clip-space positions divided by w (NDC)");
                }
                _ => {
                    ui.label("Position");
                    let current = self
                        .pos_input
                        .and_then(|i| g.inputs.get(i))
                        .map_or_else(|| "none".to_owned(), stream_label);
                    egui::ComboBox::from_id_salt("webgd-geometry-position")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for &i in &cands {
                                ui.selectable_value(
                                    &mut self.pos_input,
                                    Some(i),
                                    stream_label(&g.inputs[i]),
                                );
                            }
                        });
                }
            }
            if g.instance_count > 1 {
                ui.separator();
                ui.label("Instance");
                ui.add(
                    egui::DragValue::new(&mut self.instance)
                        .range(0..=g.instance_count - 1)
                        .speed(0.1),
                );
                ui.weak(format!("of {}", g.instance_count));
            }
            ui.separator();
            if ui
                .button("Reset view")
                .on_hover_text("Fit the camera to the geometry (also: double-click the view)")
                .clicked()
            {
                self.camera = None;
            }
            if loading {
                ui.spinner();
            }
        });

        let streams: &[AttribStream] = match (self.stage, outputs) {
            (Stage::PostVs, Ok(o)) => o,
            _ => &g.inputs,
        };
        let skey = SceneKey {
            key,
            instance: g.instance,
            stage: self.stage,
            source: match self.stage {
                Stage::PreVs => self.pos_input,
                Stage::PostVs => g.position_output,
            },
            clip_space: self.stage == Stage::PostVs && self.clip_space,
        };
        let scene = match self.scene.take() {
            Some((k, s)) if k == skey => s,
            _ => Scene::build(g, streams, &skey),
        };

        ui.weak(format!(
            "{:?} · {} vertices in draw order · {} unique · {} primitives{}",
            g.topology,
            g.draw_order.len(),
            g.vertex_ids.len(),
            scene.prims.len(),
            if g.instance_count > 1 {
                format!(" · instance {} of {}", g.instance, g.instance_count)
            } else {
                String::new()
            }
        ));
        for n in g.notes.iter().chain(&scene.notes) {
            banner(ui, n);
        }

        let avail = ui.available_height();
        egui::Panel::bottom(Id::new("webgd-geometry-table"))
            .resizable(true)
            .default_size((avail * 0.4).max(80.0))
            .min_size(60.0)
            .show(ui, |ui| self.table(ui, l, g, streams, &scene, open));
        let fit: FitId = (key, skey.stage, skey.source, skey.clip_space);
        egui::CentralPanel::no_frame().show(ui, |ui| self.view(ui, l, g, streams, &scene, fit));
        self.scene = Some((skey, scene));
    }

    fn view(
        &mut self,
        ui: &mut egui::Ui,
        l: &Loaded,
        g: &GeometryData,
        streams: &[AttribStream],
        scene: &Scene,
        fit: FitId,
    ) {
        if let Some(e) = &scene.error {
            ui.colored_label(ERROR, e);
            return;
        }
        let Some(bounds) = scene.bounds else {
            ui.weak("No drawable vertex positions.");
            return;
        };
        let mut cam = match self.camera {
            Some((id, c)) if id == fit => c,
            _ => Camera::fit(bounds, self.stage),
        };
        let (rect, resp) = ui.allocate_exact_size(
            ui.available_size().max(vec2(64.0, 64.0)),
            Sense::click_and_drag(),
        );

        // Camera input.
        if resp.dragged() {
            let d = resp.drag_delta();
            let pan = resp.dragged_by(PointerButton::Secondary)
                || resp.dragged_by(PointerButton::Middle)
                || ui.input(|i| i.modifiers.shift);
            if pan {
                let p = Proj::new(&cam, rect);
                cam.pan(d, &p);
            } else {
                cam.orbit(d);
            }
        }
        if resp.hovered() {
            let scroll = ui
                .ctx()
                .input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.y));
            let pinch = ui.input(|i| i.zoom_delta());
            cam.zoom((-scroll as f64 * 0.0015).exp() / pinch as f64);
        }
        if resp.double_clicked() {
            cam = Camera::fit(bounds, self.stage);
        }
        self.camera = Some((fit, cam));

        let proj = Proj::new(&cam, rect);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, BG);
        let screen: Vec<Option<(Pos2, f64)>> = scene
            .pos
            .iter()
            .map(|p| p.and_then(|p| proj.project(p)))
            .collect();
        let row_of = |id: u32| scene.rows.get(&id).copied();
        let at = |id: u32| row_of(id).and_then(|r| screen[r]).map(|(p, _)| p);
        let mut mesh = Mesh::default();

        // Reference clip volume in NDC: x, y ∈ [-1, 1]; z ∈ [0, 1] (WebGPU) or [-1, 1] (WebGL).
        if self.stage == Stage::PostVs && !self.clip_space {
            let gpu = l
                .cap
                .frame
                .get(g.index)
                .is_some_and(|c| l.cap.class_of(c.this).starts_with("GPU"));
            let z0 = if gpu { 0.0 } else { -1.0 };
            let corner = |i: usize| -> V3 {
                [
                    if i & 1 == 0 { -1.0 } else { 1.0 },
                    if i & 2 == 0 { -1.0 } else { 1.0 },
                    if i & 4 == 0 { z0 } else { 1.0 },
                ]
            };
            for i in 0..8usize {
                for bit in [1usize, 2, 4] {
                    let j = i | bit;
                    if j != i
                        && let (Some((a, _)), Some((b, _))) =
                            (proj.project(corner(i)), proj.project(corner(j)))
                    {
                        push_line(&mut mesh, a, b, 1.0, NDC_BOX);
                    }
                }
            }
        }

        match self.mode {
            DrawMode::Points => {
                for (p, _) in screen.iter().flatten() {
                    push_point(&mut mesh, *p, 1.5, POINT);
                }
            }
            DrawMode::Wireframe | DrawMode::Solid => {
                let light = unit(add(
                    scale(proj.f, -1.0),
                    add(scale(proj.u, 0.5), scale(proj.r, 0.3)),
                ));
                let mut tris: Vec<(f64, [Pos2; 3], Color32)> = Vec::new();
                for prim in &scene.prims {
                    match *prim {
                        Primitive::Point(a) => {
                            if let Some(p) = at(a) {
                                push_point(&mut mesh, p, 1.5, POINT);
                            }
                        }
                        Primitive::Line(a, b) => {
                            if let (Some(pa), Some(pb)) = (at(a), at(b)) {
                                push_line(&mut mesh, pa, pb, 1.0, WIRE);
                            }
                        }
                        Primitive::Triangle(a, b, c) => {
                            let (Some(ra), Some(rb), Some(rc)) = (row_of(a), row_of(b), row_of(c))
                            else {
                                continue;
                            };
                            let (Some((pa, da)), Some((pb, db)), Some((pc, dc))) =
                                (screen[ra], screen[rb], screen[rc])
                            else {
                                continue;
                            };
                            if self.mode == DrawMode::Wireframe {
                                push_line(&mut mesh, pa, pb, 1.0, WIRE);
                                push_line(&mut mesh, pb, pc, 1.0, WIRE);
                                push_line(&mut mesh, pc, pa, 1.0, WIRE);
                                continue;
                            }
                            let (Some(wa), Some(wb), Some(wc)) =
                                (scene.pos[ra], scene.pos[rb], scene.pos[rc])
                            else {
                                continue;
                            };
                            let k = normalized(cross(sub(wb, wa), sub(wc, wa)))
                                .map_or(0.25, |n| 0.25 + 0.75 * dot(n, light).abs())
                                as f32;
                            // Counter-clockwise on a y-up screen = negative area in egui's y-down space.
                            let area2 =
                                (pb.x - pa.x) * (pc.y - pa.y) - (pb.y - pa.y) * (pc.x - pa.x);
                            let base = if self.backface_tint && area2 > 0.0 {
                                BACK
                            } else {
                                FRONT
                            };
                            tris.push(((da + db + dc) / 3.0, [pa, pb, pc], shade(base, k)));
                        }
                    }
                }
                // Painter's algorithm: far to near.
                tris.sort_by(|a, b| b.0.total_cmp(&a.0));
                mesh.reserve_vertices(tris.len() * 3);
                mesh.reserve_triangles(tris.len());
                for (_, [a, b, c], color) in tris {
                    let i = mesh.vertices.len() as u32;
                    mesh.colored_vertex(a, color);
                    mesh.colored_vertex(b, color);
                    mesh.colored_vertex(c, color);
                    mesh.add_triangle(i, i + 1, i + 2);
                }
            }
        }
        painter.add(egui::Shape::mesh(mesh));

        // Selected / hovered vertex.
        if let Some((p, _)) = self
            .selected_vertex
            .and_then(row_of)
            .and_then(|r| screen[r])
        {
            painter.circle_filled(p, 3.5, ACCENT);
            painter.circle_stroke(p, 6.0, Stroke::new(1.5, ACCENT));
        }
        let hover = if resp.dragged() {
            None
        } else {
            resp.hover_pos().and_then(|hp| {
                screen
                    .iter()
                    .enumerate()
                    .filter_map(|(r, s)| s.map(|(p, _)| (r, p.distance_sq(hp))))
                    .filter(|(_, d)| *d <= HOVER_PX * HOVER_PX)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(r, _)| r)
            })
        };
        if let Some((p, _)) = hover.and_then(|r| screen[r]) {
            painter.circle_stroke(p, 5.0, Stroke::new(1.5, Color32::WHITE));
        }

        // Axes gizmo.
        let origin = pos2(rect.left() + 34.0, rect.bottom() - 34.0);
        for (axis, name, color) in [
            ([1.0, 0.0, 0.0], "X", Color32::from_rgb(230, 90, 90)),
            ([0.0, 1.0, 0.0], "Y", Color32::from_rgb(110, 200, 110)),
            ([0.0, 0.0, 1.0], "Z", Color32::from_rgb(100, 150, 240)),
        ] {
            let d = vec2(dot(axis, proj.r) as f32, -dot(axis, proj.u) as f32) * 24.0;
            painter.line_segment([origin, origin + d], Stroke::new(2.0, color));
            painter.text(
                origin + d * 1.3,
                Align2::CENTER_CENTER,
                name,
                FontId::proportional(11.0),
                color,
            );
        }
        let space = match (self.stage, self.clip_space) {
            (Stage::PreVs, _) => "attribute space",
            (Stage::PostVs, false) => "NDC (clip ÷ w)",
            (Stage::PostVs, true) => "clip space",
        };
        let weak = ui.visuals().weak_text_color();
        painter.text(
            rect.left_top() + vec2(6.0, 6.0),
            Align2::LEFT_TOP,
            space,
            FontId::proportional(11.0),
            weak,
        );
        painter.text(
            rect.right_bottom() - vec2(6.0, 6.0),
            Align2::RIGHT_BOTTOM,
            "drag: orbit · shift/right-drag: pan · wheel: zoom · double-click: fit",
            FontId::proportional(10.0),
            weak,
        );

        if resp.clicked()
            && let Some(r) = hover
            && let Some(&vid) = g.vertex_ids.get(r)
        {
            self.selected_vertex = Some(vid);
            self.scroll_to = g.draw_order.iter().position(|&v| v == vid);
        }
        if let Some(r) = hover {
            resp.on_hover_ui_at_pointer(|ui| vertex_tooltip(ui, g, streams, r));
        }
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        l: &Loaded,
        g: &GeometryData,
        streams: &[AttribStream],
        scene: &Scene,
        open: &mut Option<ObjId>,
    ) {
        let groups = match self.stage {
            Stage::PreVs => input_groups(g),
            Stage::PostVs => output_groups(streams, g.position_output, g.instance),
        };
        let row_h = ui.text_style_height(&TextStyle::Body) + 2.0;
        let total_w = IDX_W + VTX_W + groups.iter().map(Group::width).sum::<f32>();
        let selected = self.selected_vertex;
        let scroll_to = self.scroll_to.take();
        let mut clicked = None;
        egui::ScrollArea::horizontal()
            .id_salt("webgd-geometry-table-h")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let faint = ui.visuals().faint_bg_color;
                // Header: buffer groups, attribute columns, component letters.
                ui.horizontal(|ui| {
                    text_cell(ui, IDX_W + VTX_W, row_h, RichText::new(""));
                    for grp in &groups {
                        let w = grp.width();
                        ui.painter().rect_filled(
                            Rect::from_min_size(ui.cursor().min, vec2(w, row_h)),
                            0.0,
                            faint,
                        );
                        cell(ui, w, row_h, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.add(
                                egui::Label::new(RichText::new(&grp.title).strong())
                                    .selectable(false),
                            )
                            .on_hover_text(&grp.hover);
                            if let Some(b) = grp.link
                                && ui
                                    .link(obj_label(&l.cap, b))
                                    .on_hover_text("Open in Resource Viewer")
                                    .clicked()
                            {
                                *open = Some(b);
                            }
                        });
                    }
                });
                ui.horizontal(|ui| {
                    text_cell(ui, IDX_W, row_h, RichText::new("IDX").strong())
                        .on_hover_text("Position in draw order");
                    text_cell(ui, VTX_W, row_h, RichText::new("VTX").strong())
                        .on_hover_text("Vertex id (index value + base vertex)");
                    for c in groups.iter().flat_map(|g| &g.cols) {
                        text_cell(ui, c.width(), row_h, RichText::new(&c.header).strong())
                            .on_hover_text(&c.header);
                    }
                });
                ui.horizontal(|ui| {
                    text_cell(ui, IDX_W + VTX_W, row_h, RichText::new(""));
                    for c in groups.iter().flat_map(|g| &g.cols) {
                        for name in ["x", "y", "z", "w"].iter().take(c.components() as usize) {
                            text_cell(ui, c.comp_w(), row_h, RichText::new(*name).weak());
                        }
                    }
                });
                ui.separator();

                let pitch = row_h + ui.spacing().item_spacing.y;
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("webgd-geometry-table-v")
                    .auto_shrink([false, false]);
                if let Some(r) = scroll_to {
                    area = area.vertical_scroll_offset((r as f32 - 3.0).max(0.0) * pitch);
                }
                area.show_rows(ui, row_h, g.draw_order.len(), |ui, range| {
                    for r in range {
                        let vid = g.draw_order[r];
                        let restart = g.restart == Some(vid);
                        let srow = if restart {
                            None
                        } else {
                            scene.rows.get(&vid).copied()
                        };
                        let bg = Rect::from_min_size(ui.cursor().min, vec2(total_w, pitch));
                        let resp =
                            ui.interact(bg, ui.id().with(("geometry-row", r)), Sense::click());
                        if !restart && selected == Some(vid) {
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
                        } else if r % 2 == 1 {
                            ui.painter().rect_filled(bg, 0.0, faint);
                        }
                        if resp.clicked() && !restart {
                            clicked = Some(vid);
                        }
                        ui.horizontal(|ui| {
                            ui.set_height(row_h);
                            text_cell(ui, IDX_W, row_h, RichText::new(r.to_string()).monospace());
                            let vtx = if restart {
                                RichText::new("restart").weak()
                            } else {
                                RichText::new(vid.to_string()).monospace()
                            };
                            text_cell(ui, VTX_W, row_h, vtx);
                            for c in groups.iter().flat_map(|g| &g.cols) {
                                let v = srow.and_then(|sr| {
                                    c.stream
                                        .and_then(|i| streams.get(i))
                                        .and_then(|s| s.values.get(sr).copied().flatten())
                                });
                                for k in 0..c.components() as usize {
                                    let text = match v {
                                        Some(v) => {
                                            RichText::new(fmt_comp(v[k], c.kind)).monospace()
                                        }
                                        None => RichText::new("—").weak(),
                                    };
                                    text_cell(ui, c.comp_w(), row_h, text);
                                }
                            }
                        });
                    }
                });
            });
        if let Some(vid) = clicked {
            self.selected_vertex = Some(vid);
        }
    }
}
