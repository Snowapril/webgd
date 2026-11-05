//! Inspection panels: Render Targets (attachment grid + preview) and the stage-based API Inspector.
use crate::replay_queue::Key;
use crate::state::{AppState, Loaded, Tab};
use crate::widgets::{ACCENT, ERROR, ImageTex, WARN, banner, default_range, fmt_num, load_image};
use egui::{Color32, Id, RichText, TextureHandle, TextureOptions};
use std::collections::HashMap;
use webgd_analysis::{Stage as ShaderStage, StageStats, occupancy, wgsl_stats};
use webgd_core::display::{obj_label, value_label};
use webgd_core::pixels::Image;
use webgd_core::track::commands::{CmdKind, classify};
use webgd_core::track::gpu::creation_call;
use webgd_core::{Call, Capture, ObjId, Value};
use webgd_replay::{BindingRow, Inspection, StateNode, TargetView};

/// What a click inside an inspection panel asks the app to do.
enum Action {
    Open(ObjId),
    Focus(Tab),
}

fn apply(st: &mut AppState, act: Option<Action>) {
    match act {
        Some(Action::Open(id)) => st.open_resource(id),
        Some(Action::Focus(tab)) => st.focus_tab = Some(tab),
        None => {}
    }
}

/// Runs `f` on the inspection for the selected event, or shows why there is none.
fn with_inspection<R>(
    ui: &mut egui::Ui,
    st: &AppState,
    f: impl FnOnce(&mut egui::Ui, &Loaded, Key, &Inspection) -> Option<R>,
) -> Option<R> {
    let Some(l) = &st.loaded else {
        super::no_capture(ui);
        return None;
    };
    let Some(sel) = st.selected else {
        if st.selected_setup.is_some() {
            ui.weak(
                "A pre-frame setup call is selected; the replay shows the frame-start state. \
                 Select a frame event to inspect its API state.",
            );
        } else {
            ui.weak("Select an event to inspect it.");
        }
        return None;
    };
    let key = Key {
        generation: l.generation,
        stop: Some(sel),
    };
    let out = st.replay.results(|r| match (&r.inspection, &r.replay) {
        (Some((k, insp)), _) if *k == key => Some(f(ui, l, key, insp)),
        (_, Some((k, Err(e)))) if *k == key => {
            ui.colored_label(ERROR, format!("replay failed: {e}"));
            Some(None)
        }
        _ => None,
    });
    match out {
        Some(r) => r,
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("replaying to #{sel}…"));
            });
            None
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Attachment tiles (shared by Render Targets and the inspector's output stages)
// ---------------------------------------------------------------------------------------------

type Thumbs = Option<(Key, Vec<Option<TextureHandle>>)>;

/// Thumbnail textures for every target of `insp`; re-uploaded only when the replay key changes.
fn thumbs<'c>(
    cache: &'c mut Thumbs,
    ctx: &egui::Context,
    key: Key,
    insp: &Inspection,
) -> &'c [Option<TextureHandle>] {
    if cache.as_ref().is_none_or(|(k, _)| *k != key) {
        let handles = insp
            .targets
            .iter()
            .map(|t| {
                t.image.as_ref().map(|img| {
                    load_image(
                        ctx,
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
    cache.as_ref().map(|(_, h)| h.as_slice()).unwrap_or(&[])
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// Largest rect with the image's aspect ratio centered in `bx`.
fn fit_rect(w: u32, h: u32, bx: egui::Rect) -> egui::Rect {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let s = (bx.width() / w).min(bx.height() / h);
    egui::Rect::from_center_size(bx.center(), egui::vec2(w * s, h * s))
}

/// `256×256 rgba16float` from the read image, else the resource summary.
fn target_dims(l: &Loaded, t: &TargetView) -> Option<String> {
    if let Some(img) = &t.image {
        let format = format!("{:?}", img.format).to_lowercase();
        return Some(format!("{}×{} {format}", img.width, img.height));
    }
    let id = t.resource?;
    l.resources
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.summary.clone())
        .filter(|s| !s.is_empty())
}

/// `Color 0 · GPUTexture#13 · 256×256 rgba16float`; returns the resource whose link was clicked.
fn target_caption(ui: &mut egui::Ui, l: &Loaded, t: &TargetView) -> Option<ObjId> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(&t.label).strong());
        if let Some(id) = t.resource {
            ui.weak("·");
            if ui
                .link(obj_label(&l.cap, id))
                .on_hover_text("Open in Resource Viewer")
                .clicked()
            {
                clicked = Some(id);
            }
        }
        if let Some(d) = target_dims(l, t) {
            ui.weak("·");
            ui.label(RichText::new(d).monospace());
        }
    });
    clicked
}

/// One attachment tile: thumbnail + caption. Returns (thumbnail clicked, caption link clicked).
fn target_cell(
    ui: &mut egui::Ui,
    l: &Loaded,
    t: &TargetView,
    tex: Option<&TextureHandle>,
    side: f32,
    selected: bool,
    hover: &str,
) -> (bool, Option<ObjId>) {
    let stroke = if selected {
        egui::Stroke::new(1.5, ACCENT)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    let mut out = (false, None);
    egui::Frame::new()
        .inner_margin(4.0)
        .corner_radius(3.0)
        .stroke(stroke)
        .show(ui, |ui| {
            ui.set_width(side.max(150.0));
            ui.vertical(|ui| {
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
                ui.painter().rect_filled(rect, 2.0, Color32::from_gray(24));
                match (tex, &t.image) {
                    (Some(h), Some(img)) => {
                        ui.painter().image(
                            h.id(),
                            fit_rect(img.width, img.height, rect),
                            full_uv(),
                            Color32::WHITE,
                        );
                    }
                    _ => {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "no preview",
                            egui::FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                }
                out.0 = resp.on_hover_text(hover).clicked();
                out.1 = target_caption(ui, l, t);
                if let Some(n) = &t.note {
                    ui.label(RichText::new(n).small().color(WARN));
                }
            });
        });
    out
}

/// `x, y: r g b a` raw texel values under the pointer.
fn texel_readout(resp: &egui::Response, img: &Image) -> Option<String> {
    let p = resp.hover_pos()?;
    let r = resp.rect;
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return None;
    }
    let x = ((p.x - r.min.x) / r.width() * img.width as f32).floor();
    let y = ((p.y - r.min.y) / r.height() * img.height as f32).floor();
    if x < 0.0 || y < 0.0 || x >= img.width as f32 || y >= img.height as f32 {
        return None;
    }
    let v = img.texel(x as u32, y as u32);
    Some(format!(
        "{x}, {y}:  {}  {}  {}  {}",
        fmt_num(v[0]),
        fmt_num(v[1]),
        fmt_num(v[2]),
        fmt_num(v[3])
    ))
}

// ---------------------------------------------------------------------------------------------
// Render Targets
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct TargetsUi {
    thumbs: Thumbs,
    preview: ImageTex<(Key, usize)>,
    /// Attachment shown in the large preview.
    focus: usize,
    /// 1:1 pixels (scrollable) instead of fit-to-panel.
    actual_size: bool,
}

impl TargetsUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let act = with_inspection(ui, st, |ui, l, key, insp| self.body(ui, l, key, insp));
        apply(st, act);
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        l: &Loaded,
        key: Key,
        insp: &Inspection,
    ) -> Option<Action> {
        for n in &insp.notes {
            banner(ui, n);
        }
        if insp.targets.is_empty() {
            ui.weak("No render targets are bound at this event.");
            return None;
        }
        let TargetsUi {
            thumbs: cache,
            preview,
            focus,
            actual_size,
        } = self;
        *focus = (*focus).min(insp.targets.len() - 1);
        let handles = thumbs(cache, ui.ctx(), key, insp);
        let mut act = None;
        egui::ScrollArea::horizontal()
            .id_salt("webgd-targets-strip")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    for (i, t) in insp.targets.iter().enumerate() {
                        let tex = handles.get(i).and_then(Option::as_ref);
                        let (clicked, link) =
                            target_cell(ui, l, t, tex, 112.0, i == *focus, "Show in preview");
                        if clicked {
                            *focus = i;
                        }
                        if let Some(id) = link {
                            act = Some(Action::Open(id));
                        }
                    }
                });
            });
        ui.separator();

        let t = &insp.targets[*focus];
        ui.horizontal(|ui| {
            ui.label(RichText::new(&t.label).strong());
            ui.separator();
            ui.selectable_value(actual_size, false, "Fit");
            ui.selectable_value(actual_size, true, "1:1");
            if let Some(id) = t.resource {
                ui.separator();
                if ui.button("Open in Resource Viewer").clicked() {
                    act = Some(Action::Open(id));
                }
            }
        });
        if let Some(n) = &t.note {
            banner(ui, n);
        }
        let Some(img) = &t.image else {
            ui.weak("No image available for this attachment.");
            return act;
        };
        let tex = preview.get(ui.ctx(), (key, *focus), img, default_range(img), [true; 4]);
        let footer = ui.spacing().interact_size.y + 6.0;
        let avail = ui.available_size() - egui::vec2(0.0, footer);
        let resp = if *actual_size {
            let size = egui::vec2(img.width as f32, img.height as f32);
            egui::ScrollArea::both()
                .id_salt("webgd-targets-preview")
                .max_height(avail.y.max(16.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::hover()))
                })
                .inner
        } else {
            let bx = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(avail.x.max(16.0), avail.y.max(16.0)),
            );
            let size = fit_rect(img.width, img.height, bx).size();
            ui.vertical_centered(|ui| {
                ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::hover()))
            })
            .inner
        };
        match texel_readout(&resp, img) {
            Some(r) => ui.label(RichText::new(r).monospace()),
            None => ui.weak("Hover the preview to read texel values."),
        };
        act
    }
}

// ---------------------------------------------------------------------------------------------
// API Inspector
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Stage {
    InputAssembly,
    VertexShader,
    Rasterizer,
    FragmentShader,
    DepthStencil,
    ColorOutput,
    ComputeShader,
    Dispatch,
    All,
}

impl Stage {
    fn title(self) -> &'static str {
        match self {
            Stage::InputAssembly => "Input Assembly",
            Stage::VertexShader => "Vertex Shader",
            Stage::Rasterizer => "Rasterizer",
            Stage::FragmentShader => "Fragment Shader",
            Stage::DepthStencil => "Depth/Stencil",
            Stage::ColorOutput => "Color Output",
            Stage::ComputeShader => "Compute Shader",
            Stage::Dispatch => "Dispatch",
            Stage::All => "All state",
        }
    }
}

enum Cell {
    Text(String),
    Obj(ObjId),
}

type Props = Vec<(String, Cell)>;

enum Item<'a> {
    Nodes(Vec<&'a StateNode>),
    Bindings(Vec<&'a BindingRow>),
    Props(Props),
    /// Indices into `Inspection::targets`.
    Targets(Vec<usize>),
}

struct Section<'a> {
    title: String,
    item: Item<'a>,
}

struct Page<'a> {
    stage: Stage,
    sections: Vec<Section<'a>>,
}

/// Static compute-shader stats per (generation, module, entry point).
type WgCache = HashMap<(u64, ObjId, Option<String>), Result<StageStats, String>>;

#[derive(Default)]
pub struct InspectorUi {
    /// Last stage the user picked; kept across events when the new page list contains it.
    stage: Option<Stage>,
    thumbs: Thumbs,
    wg: WgCache,
}

impl InspectorUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let act = with_inspection(ui, st, |ui, l, key, insp| self.body(ui, l, key, insp));
        apply(st, act);
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        l: &Loaded,
        key: Key,
        insp: &Inspection,
    ) -> Option<Action> {
        let cap = &*l.cap;
        // GL inspection always reports a `Program` group; WebGPU never does.
        let gl = top(insp, "Program").is_some();
        let call = cap.frame.get(insp.index);
        let compute = !gl
            && (insp.pipeline.iter().any(|p| child(p, "compute").is_some())
                || call.is_some_and(|c| cap.class_of(c.this) == "GPUComputePassEncoder"));
        let InspectorUi {
            stage,
            thumbs: cache,
            wg,
        } = self;
        let pages = if gl {
            gl_pages(l, insp)
        } else {
            gpu_pages(l, insp, compute, wg)
        };
        let mut act = None;

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if gl { "WebGL2" } else { "WebGPU" })
                    .strong()
                    .color(ACCENT),
            );
            ui.label(
                RichText::new(format!("#{}", insp.index))
                    .monospace()
                    .strong(),
            );
            crate::widgets::origin_badge(ui, l.origin_of(insp.index));
            let label = l.labels.get(insp.index).map(String::as_str).unwrap_or("");
            ui.add(egui::Label::new(RichText::new(label).monospace()).truncate());
        });
        for n in &insp.notes {
            banner(ui, n);
        }
        ui.separator();

        let cur = stage
            .filter(|s| pages.iter().any(|p| p.stage == *s))
            .unwrap_or(pages[0].stage);
        let handles = thumbs(cache, ui.ctx(), key, insp);

        egui::Panel::left(Id::new("webgd-api-inspector-stages"))
            .resizable(true)
            .default_size(140.0)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                    for p in &pages {
                        let text = if p.sections.is_empty() {
                            RichText::new(p.stage.title()).weak()
                        } else {
                            RichText::new(p.stage.title())
                        };
                        if ui.selectable_label(p.stage == cur, text).clicked() {
                            *stage = Some(p.stage);
                        }
                    }
                });
            });

        let page = pages.iter().find(|p| p.stage == cur).unwrap_or(&pages[0]);
        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt(("webgd-api-inspector-page", cur))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(2.0);
                    ui.label(RichText::new(cur.title()).size(15.0).strong());
                    if page.sections.is_empty() {
                        ui.weak("Nothing recorded for this stage at the selected event.");
                    }
                    for s in &page.sections {
                        let id = Id::new("webgd-api-inspector").with(cur).with(&s.title);
                        section_ui(ui, l, insp, handles, s, id, &mut act);
                    }
                });
        });
        act
    }
}

fn section_ui(
    ui: &mut egui::Ui,
    l: &Loaded,
    insp: &Inspection,
    handles: &[Option<TextureHandle>],
    s: &Section,
    id: Id,
    act: &mut Option<Action>,
) {
    egui::CollapsingHeader::new(RichText::new(&s.title).strong())
        .id_salt(id)
        .default_open(true)
        .show(ui, |ui| match &s.item {
            Item::Nodes(nodes) => nodes_ui(ui, &l.cap, nodes.iter().copied(), id, 0, act),
            Item::Bindings(rows) => bindings_table(ui, &l.cap, rows, id, act),
            Item::Props(rows) => props_grid(ui, &l.cap, rows, id, act),
            Item::Targets(idx) => {
                ui.horizontal_wrapped(|ui| {
                    for &i in idx {
                        let tex = handles.get(i).and_then(Option::as_ref);
                        let (clicked, link) = target_cell(
                            ui,
                            l,
                            &insp.targets[i],
                            tex,
                            88.0,
                            false,
                            "Show in Render Targets",
                        );
                        if clicked {
                            *act = Some(Action::Focus(Tab::RenderTargets));
                        }
                        if let Some(id) = link {
                            *act = Some(Action::Open(id));
                        }
                    }
                });
            }
        });
}

/// `Class#id` strings produced by the inspectors → the object, when it exists in the capture.
fn parse_obj(cap: &Capture, s: &str) -> Option<ObjId> {
    let (class, num) = s.rsplit_once('#')?;
    let id = ObjId(num.parse().ok()?);
    (!class.is_empty() && cap.class_of(id) == class).then_some(id)
}

fn value_cell(ui: &mut egui::Ui, cap: &Capture, value: &str, act: &mut Option<Action>) {
    match parse_obj(cap, value) {
        Some(id) => {
            if ui
                .link(RichText::new(value).monospace())
                .on_hover_text("Open in Resource Viewer")
                .clicked()
            {
                *act = Some(Action::Open(id));
            }
        }
        None => {
            ui.label(RichText::new(value).monospace());
        }
    }
}

/// Dense name/value grid for a run of leaves; drains `leaves`.
fn leaf_grid(
    ui: &mut egui::Ui,
    cap: &Capture,
    leaves: &mut Vec<(&str, &str)>,
    id: Id,
    act: &mut Option<Action>,
) {
    if leaves.is_empty() {
        return;
    }
    egui::Grid::new(id)
        .num_columns(2)
        .striped(true)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for (name, value) in leaves.drain(..) {
                ui.label(name);
                value_cell(ui, cap, value, act);
                ui.end_row();
            }
        });
}

/// State tree: consecutive leaves share a grid, groups collapse. Object labels are links.
fn nodes_ui<'n>(
    ui: &mut egui::Ui,
    cap: &Capture,
    nodes: impl IntoIterator<Item = &'n StateNode>,
    id: Id,
    depth: usize,
    act: &mut Option<Action>,
) {
    let mut leaves: Vec<(&'n str, &'n str)> = Vec::new();
    let mut run = 0usize;
    for n in nodes {
        match n {
            StateNode::Leaf { name, value } => leaves.push((name.as_str(), value.as_str())),
            StateNode::Group { name, children } => {
                leaf_grid(ui, cap, &mut leaves, id.with(("leaves", run)), act);
                run += 1;
                let gid = id.with(name.as_str());
                egui::CollapsingHeader::new(name.as_str())
                    .id_salt(gid)
                    .default_open(depth < 2)
                    .show(ui, |ui| nodes_ui(ui, cap, children, gid, depth + 1, act));
            }
        }
    }
    leaf_grid(ui, cap, &mut leaves, id.with(("leaves", run)), act);
}

fn props_grid(
    ui: &mut egui::Ui,
    cap: &Capture,
    rows: &[(String, Cell)],
    id: Id,
    act: &mut Option<Action>,
) {
    egui::Grid::new(id)
        .num_columns(2)
        .striped(true)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for (k, v) in rows {
                ui.label(k.as_str());
                match v {
                    Cell::Text(s) => {
                        ui.label(RichText::new(s.as_str()).monospace());
                    }
                    Cell::Obj(o) => {
                        if ui
                            .link(RichText::new(obj_label(cap, *o)).monospace())
                            .on_hover_text("Open in Resource Viewer")
                            .clicked()
                        {
                            *act = Some(Action::Open(*o));
                        }
                    }
                }
                ui.end_row();
            }
        });
}

fn bindings_table(
    ui: &mut egui::Ui,
    cap: &Capture,
    rows: &[&BindingRow],
    id: Id,
    act: &mut Option<Action>,
) {
    egui::Grid::new(id)
        .num_columns(4)
        .striped(true)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for h in ["Group", "Slot", "Resource", "Detail"] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for b in rows {
                ui.label(b.group.as_str());
                ui.label(RichText::new(b.slot.as_str()).monospace());
                match b.resource {
                    Some(r) => {
                        if ui
                            .link(obj_label(cap, r))
                            .on_hover_text("Open in Resource Viewer")
                            .clicked()
                        {
                            *act = Some(Action::Open(r));
                        }
                    }
                    None => {
                        ui.weak("—");
                    }
                }
                ui.label(b.detail.as_str());
                ui.end_row();
            }
        });
}

// ---------------------------------------------------------------------------------------------
// Page building: map inspection output onto pipeline stages by name. Everything also appears
// unfiltered on the `All state` page.
// ---------------------------------------------------------------------------------------------

fn push<'a>(v: &mut Vec<Section<'a>>, title: impl Into<String>, item: Item<'a>) {
    let empty = match &item {
        Item::Nodes(n) => n.is_empty(),
        Item::Bindings(b) => b.is_empty(),
        Item::Props(p) => p.is_empty(),
        Item::Targets(t) => t.is_empty(),
    };
    if !empty {
        v.push(Section {
            title: title.into(),
            item,
        });
    }
}

fn node_name(n: &StateNode) -> &str {
    match n {
        StateNode::Leaf { name, .. } | StateNode::Group { name, .. } => name.as_str(),
    }
}

fn children(n: &StateNode) -> &[StateNode] {
    match n {
        StateNode::Group { children, .. } => children.as_slice(),
        StateNode::Leaf { .. } => &[],
    }
}

fn is_group(n: &StateNode, name: &str) -> bool {
    matches!(n, StateNode::Group { name: g, .. } if g == name)
}

fn child<'a>(n: &'a StateNode, name: &str) -> Option<&'a StateNode> {
    children(n).iter().find(|c| node_name(c) == name)
}

fn top<'a>(insp: &'a Inspection, name: &str) -> Option<&'a StateNode> {
    insp.pipeline.iter().find(|n| is_group(n, name))
}

/// Value of the leaf child `name` of `n`.
fn leaf_in<'a>(n: Option<&'a StateNode>, name: &str) -> Option<&'a str> {
    match n.and_then(|n| child(n, name))? {
        StateNode::Leaf { value, .. } => Some(value.as_str()),
        StateNode::Group { .. } => None,
    }
}

/// Children of `n` whose names pass `keep`.
fn pick(n: Option<&StateNode>, keep: impl Fn(&str) -> bool) -> Vec<&StateNode> {
    n.map(|n| children(n).iter().filter(|c| keep(node_name(c))).collect())
        .unwrap_or_default()
}

/// Scalar descriptor fields with WebGPU defaults filled in for omitted keys.
fn props_with_defaults(n: Option<&StateNode>, keys: &[(&str, Option<&str>)]) -> Props {
    let mut out = Vec::new();
    for (k, default) in keys {
        match leaf_in(n, k) {
            Some(v) => out.push((k.to_string(), Cell::Text(v.to_string()))),
            None => {
                if let Some(d) = default {
                    out.push((k.to_string(), Cell::Text(format!("{d} (default)"))));
                }
            }
        }
    }
    out
}

fn rows(insp: &Inspection, keep: impl Fn(&BindingRow) -> bool) -> Vec<&BindingRow> {
    insp.bindings.iter().filter(|b| keep(b)).collect()
}

fn is_depth_target(t: &TargetView) -> bool {
    let l = t.label.to_ascii_lowercase();
    l.contains("depth") || l.contains("stencil")
}

fn target_idx(insp: &Inspection, depth: bool) -> Vec<usize> {
    (0..insp.targets.len())
        .filter(|&i| is_depth_target(&insp.targets[i]) == depth)
        .collect()
}

fn all_page(insp: &Inspection) -> Page<'_> {
    let mut s = Vec::new();
    push(
        &mut s,
        "Pipeline state",
        Item::Nodes(insp.pipeline.iter().collect()),
    );
    push(
        &mut s,
        "Bindings",
        Item::Bindings(insp.bindings.iter().collect()),
    );
    push(
        &mut s,
        "Attachments",
        Item::Targets((0..insp.targets.len()).collect()),
    );
    Page {
        stage: Stage::All,
        sections: s,
    }
}

/// Parameter names of draw/dispatch calls, for the call summary sections.
fn arg_names(gl: bool, method: &str) -> &'static [&'static str] {
    match (gl, method) {
        (false, "draw") => &[
            "vertexCount",
            "instanceCount",
            "firstVertex",
            "firstInstance",
        ],
        (false, "drawIndexed") => &[
            "indexCount",
            "instanceCount",
            "firstIndex",
            "baseVertex",
            "firstInstance",
        ],
        (false, "drawIndirect" | "drawIndexedIndirect" | "dispatchWorkgroupsIndirect") => {
            &["indirectBuffer", "indirectOffset"]
        }
        (false, "executeBundles") => &["bundles"],
        (false, "dispatchWorkgroups") => &["workgroupCountX", "workgroupCountY", "workgroupCountZ"],
        (true, "drawArrays") => &["mode", "first", "count"],
        (true, "drawArraysInstanced" | "drawArraysInstancedANGLE") => {
            &["mode", "first", "count", "instanceCount"]
        }
        (true, "drawElements") => &["mode", "count", "type", "offset"],
        (true, "drawElementsInstanced" | "drawElementsInstancedANGLE") => {
            &["mode", "count", "type", "offset", "instanceCount"]
        }
        (true, "drawRangeElements") => &["mode", "start", "end", "count", "type", "offset"],
        // Every other GL draw entry point (multi-draw extensions, …) also starts with `mode`.
        (true, _) => &["mode"],
        _ => &[],
    }
}

fn gl_mode(v: u32) -> Option<&'static str> {
    Some(match v {
        0 => "POINTS",
        1 => "LINES",
        2 => "LINE_LOOP",
        3 => "LINE_STRIP",
        4 => "TRIANGLES",
        5 => "TRIANGLE_STRIP",
        6 => "TRIANGLE_FAN",
        _ => return None,
    })
}

fn gl_index_type(v: u32) -> Option<&'static str> {
    Some(match v {
        0x1401 => "UNSIGNED_BYTE",
        0x1403 => "UNSIGNED_SHORT",
        0x1405 => "UNSIGNED_INT",
        _ => return None,
    })
}

/// Method + named arguments of the selected draw/dispatch call.
fn call_props(cap: &Capture, c: &Call, gl: bool) -> Props {
    let m = cap.method(c);
    let names = arg_names(gl, m);
    let mut out = vec![("method".to_string(), Cell::Text(m.to_string()))];
    for (i, a) in c.args.iter().enumerate() {
        let name = names
            .get(i)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("arg{i}"));
        match a {
            Value::ObjRef(id) => out.push((name, Cell::Obj(*id))),
            Value::Array(items)
                if !items.is_empty() && items.iter().all(|x| x.as_obj().is_some()) =>
            {
                for (j, x) in items.iter().enumerate() {
                    if let Some(id) = x.as_obj() {
                        out.push((format!("{name}[{j}]"), Cell::Obj(id)));
                    }
                }
            }
            _ => {
                let num = a.as_f64().map(|x| x as u32);
                let named = match (gl, name.as_str(), num) {
                    (true, "mode", Some(x)) => gl_mode(x),
                    (true, "type", Some(x)) => gl_index_type(x),
                    _ => None,
                };
                let text = named
                    .map(str::to_string)
                    .unwrap_or_else(|| value_label(cap, a));
                out.push((name, Cell::Text(text)));
            }
        }
    }
    out
}

/// Link to the pipeline object plus its label and layout.
fn pipeline_props(cap: &Capture, p: &StateNode) -> Props {
    let cell = |s: &str| {
        parse_obj(cap, s)
            .map(Cell::Obj)
            .unwrap_or_else(|| Cell::Text(s.to_string()))
    };
    let mut out = Vec::new();
    match p {
        StateNode::Leaf { value, .. } => out.push(("pipeline".to_string(), cell(value.as_str()))),
        StateNode::Group { name, .. } => {
            out.push(("pipeline".to_string(), cell(name.as_str())));
            if let Some(label) = leaf_in(Some(p), "label") {
                out.push(("label".to_string(), Cell::Text(label.to_string())));
            }
            if let Some(layout) = leaf_in(Some(p), "layout") {
                out.push(("layout".to_string(), cell(layout)));
            }
        }
    }
    out
}

/// Shader-stage visibility from a bind-group row's trailing `[VERTEX|FRAGMENT …]` layout summary.
/// `None` when unknown (auto layout, no layout entry, offsets row).
fn gpu_visibility(detail: &str) -> Option<&str> {
    let inner = detail.strip_suffix(']')?;
    let inner = &inner[inner.rfind('[')? + 1..];
    let first = inner.split_whitespace().next()?;
    first
        .split('|')
        .all(|t| matches!(t, "VERTEX" | "FRAGMENT" | "COMPUTE"))
        .then_some(first)
}

/// Bind-group rows visible to `stage`; rows of unknown visibility count as visible everywhere.
fn gpu_visible(b: &BindingRow, stage: &str) -> bool {
    b.group.starts_with("Bind group")
        && gpu_visibility(&b.detail).is_none_or(|v| v.split('|').any(|t| t == stage))
}

fn wgsl_code(cap: &Capture, module: ObjId) -> Option<&str> {
    let (sec, i) = creation_call(cap, module)?;
    let c = cap.section(sec).get(i)?;
    match cap.dict_get(c.args.first()?, "code")? {
        Value::String(s) => Some(cap.str(*s)),
        _ => None,
    }
}

fn compute_stats(
    cache: &mut WgCache,
    cap: &Capture,
    generation: u64,
    module: ObjId,
    entry: Option<&str>,
) -> Result<StageStats, String> {
    cache.retain(|k, _| k.0 == generation);
    cache
        .entry((generation, module, entry.map(str::to_string)))
        .or_insert_with(|| match wgsl_code(cap, module) {
            Some(src) => wgsl_stats(src, entry, ShaderStage::Compute).map_err(|e| e.to_string()),
            None => Err("shader source unavailable".into()),
        })
        .clone()
}

fn workgroup_props(stats: &Result<StageStats, String>) -> Props {
    let text = |k: &str, v: String| (k.to_string(), Cell::Text(v));
    match stats {
        Ok(s) => {
            let mut out = Vec::new();
            if let Some([x, y, z]) = s.workgroup_size {
                out.push(text("workgroup size", format!("{x} × {y} × {z}")));
                out.push(text(
                    "invocations / workgroup",
                    (x as u64 * y as u64 * z as u64).to_string(),
                ));
            }
            out.push(text(
                "workgroup memory",
                format!("{} bytes", s.workgroup_bytes),
            ));
            out.push(text("registers (vec4, est.)", s.est_regs_vec4.to_string()));
            out.push(text(
                "occupancy (est.)",
                format!("{:.0}%", occupancy(s) * 100.0),
            ));
            out.push(text("ALU ops / invocation (est.)", fmt_num(s.alu)));
            out.push(text("branches / invocation (est.)", fmt_num(s.branches)));
            out.push(text(
                "texture samples / invocation (est.)",
                fmt_num(s.tex_samples),
            ));
            out
        }
        Err(e) => vec![text("analysis", format!("n/a ({e})"))],
    }
}

fn dispatch_props(cap: &Capture, c: &Call, wg: Option<[u32; 3]>) -> Props {
    let text = |k: &str, v: String| (k.to_string(), Cell::Text(v));
    if cap.method(c) != "dispatchWorkgroups" {
        return vec![text(
            "workgroups",
            "indirect (read from the indirect buffer on the GPU)".into(),
        )];
    }
    let g = |i: usize| {
        c.args
            .get(i)
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .max(0.0) as u64
    };
    let (x, y, z) = (g(0), g(1), g(2));
    let groups = x * y * z;
    let mut out = vec![text("workgroups", format!("{x} × {y} × {z} = {groups}"))];
    if let Some([a, b, d]) = wg {
        out.push(text(
            "invocations",
            (groups * a as u64 * b as u64 * d as u64).to_string(),
        ));
    }
    out
}

fn gpu_pages<'a>(
    l: &'a Loaded,
    insp: &'a Inspection,
    compute: bool,
    wg: &mut WgCache,
) -> Vec<Page<'a>> {
    let cap = &*l.cap;
    let call = cap.frame.get(insp.index);
    let kind = call.map(|c| classify(cap, c));
    let dynamic = top(insp, "Dynamic");
    // Pipeline descriptor trees (several when an executeBundles replays recorded bundles).
    let pipes: Vec<&StateNode> = insp
        .pipeline
        .iter()
        .filter(|n| !is_group(n, "Dynamic"))
        .collect();
    let multi = pipes.len() > 1;
    let titled = |base: &str, p: &StateNode| {
        if multi {
            format!("{base} · {}", node_name(p))
        } else {
            base.to_string()
        }
    };

    if compute {
        let (mut cs, mut dispatch) = (Vec::new(), Vec::new());
        let mut wg_size = None;
        for &p in &pipes {
            let comp = child(p, "compute");
            push(
                &mut cs,
                titled("Pipeline", p),
                Item::Props(pipeline_props(cap, p)),
            );
            push(
                &mut cs,
                titled("Compute shader", p),
                Item::Nodes(pick(comp, |_| true)),
            );
            if let Some(module) = leaf_in(comp, "module").and_then(|v| parse_obj(cap, v)) {
                let stats =
                    compute_stats(wg, cap, l.generation, module, leaf_in(comp, "entryPoint"));
                if let Ok(s) = &stats {
                    wg_size = wg_size.or(s.workgroup_size);
                }
                push(
                    &mut cs,
                    titled("Workgroup (est.)", p),
                    Item::Props(workgroup_props(&stats)),
                );
            }
        }
        push(
            &mut cs,
            "Bound resources",
            Item::Bindings(rows(insp, |b| gpu_visible(b, "COMPUTE"))),
        );
        if let Some(c) = call
            && kind == Some(CmdKind::Dispatch)
        {
            push(
                &mut dispatch,
                "Dispatch call",
                Item::Props(call_props(cap, c, false)),
            );
            push(
                &mut dispatch,
                "Totals",
                Item::Props(dispatch_props(cap, c, wg_size)),
            );
        }
        return vec![
            Page {
                stage: Stage::ComputeShader,
                sections: cs,
            },
            Page {
                stage: Stage::Dispatch,
                sections: dispatch,
            },
            all_page(insp),
        ];
    }

    let (mut ia, mut vs, mut rs, mut fs, mut ds, mut co) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    if let Some(c) = call
        && kind == Some(CmdKind::Draw)
    {
        push(&mut ia, "Draw call", Item::Props(call_props(cap, c, false)));
    }
    for &p in &pipes {
        let is_desc = matches!(p, StateNode::Group { .. });
        let vertex = child(p, "vertex");
        let primitive = child(p, "primitive");
        let depth_stencil = child(p, "depthStencil");
        let fragment = child(p, "fragment");
        if is_desc {
            push(
                &mut ia,
                titled("Primitive assembly", p),
                Item::Props(props_with_defaults(
                    primitive,
                    &[
                        ("topology", Some("triangle-list")),
                        ("stripIndexFormat", None),
                    ],
                )),
            );
        }
        push(
            &mut ia,
            titled("Vertex buffer layouts", p),
            Item::Nodes(pick(vertex.and_then(|v| child(v, "buffers")), |_| true)),
        );

        push(
            &mut vs,
            titled("Pipeline", p),
            Item::Props(pipeline_props(cap, p)),
        );
        push(
            &mut vs,
            titled("Vertex shader", p),
            Item::Nodes(pick(vertex, |k| k != "buffers")),
        );

        if is_desc {
            push(
                &mut rs,
                titled("Primitive state", p),
                Item::Props(props_with_defaults(
                    primitive,
                    &[
                        ("frontFace", Some("ccw")),
                        ("cullMode", Some("none")),
                        ("unclippedDepth", None),
                    ],
                )),
            );
            push(
                &mut rs,
                titled("Multisample", p),
                Item::Props(props_with_defaults(
                    child(p, "multisample"),
                    &[
                        ("count", Some("1")),
                        ("mask", None),
                        ("alphaToCoverageEnabled", Some("false")),
                    ],
                )),
            );
        }
        push(
            &mut rs,
            titled("Depth bias", p),
            Item::Nodes(pick(depth_stencil, |k| k.starts_with("depthBias"))),
        );

        push(
            &mut fs,
            titled("Pipeline", p),
            Item::Props(pipeline_props(cap, p)),
        );
        if is_desc && fragment.is_none() {
            push(
                &mut fs,
                titled("Fragment shader", p),
                Item::Props(vec![(
                    "fragment".into(),
                    Cell::Text("none (no fragment stage)".into()),
                )]),
            );
        }
        push(
            &mut fs,
            titled("Fragment shader", p),
            Item::Nodes(pick(fragment, |k| k != "targets")),
        );

        if is_desc && depth_stencil.is_none() {
            push(
                &mut ds,
                titled("Depth/stencil state", p),
                Item::Props(vec![(
                    "depthStencil".into(),
                    Cell::Text("none (depth/stencil disabled)".into()),
                )]),
            );
        }
        push(
            &mut ds,
            titled("Depth/stencil state", p),
            Item::Nodes(pick(depth_stencil, |_| true)),
        );

        push(
            &mut co,
            titled("Color targets", p),
            Item::Nodes(pick(fragment.and_then(|f| child(f, "targets")), |_| true)),
        );
    }
    push(
        &mut ia,
        "Vertex & index buffers",
        Item::Bindings(rows(insp, |b| {
            matches!(b.group.as_str(), "Vertex buffers" | "Index buffer")
        })),
    );
    push(
        &mut vs,
        "Bound resources",
        Item::Bindings(rows(insp, |b| gpu_visible(b, "VERTEX"))),
    );
    push(
        &mut rs,
        "Viewport & scissor",
        Item::Nodes(pick(dynamic, |k| matches!(k, "viewport" | "scissor"))),
    );
    push(
        &mut fs,
        "Bound resources",
        Item::Bindings(rows(insp, |b| gpu_visible(b, "FRAGMENT"))),
    );
    push(
        &mut ds,
        "Stencil reference",
        Item::Nodes(pick(dynamic, |k| k == "stencilReference")),
    );
    push(
        &mut ds,
        "Attachments",
        Item::Targets(target_idx(insp, true)),
    );
    push(
        &mut co,
        "Blend constant",
        Item::Nodes(pick(dynamic, |k| k == "blendConstant")),
    );
    push(
        &mut co,
        "Attachments",
        Item::Targets(target_idx(insp, false)),
    );

    render_pages(ia, vs, rs, fs, ds, co, insp)
}

fn render_pages<'a>(
    ia: Vec<Section<'a>>,
    vs: Vec<Section<'a>>,
    rs: Vec<Section<'a>>,
    fs: Vec<Section<'a>>,
    ds: Vec<Section<'a>>,
    co: Vec<Section<'a>>,
    insp: &'a Inspection,
) -> Vec<Page<'a>> {
    let page = |stage, sections| Page { stage, sections };
    vec![
        page(Stage::InputAssembly, ia),
        page(Stage::VertexShader, vs),
        page(Stage::Rasterizer, rs),
        page(Stage::FragmentShader, fs),
        page(Stage::DepthStencil, ds),
        page(Stage::ColorOutput, co),
        all_page(insp),
    ]
}

const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_VERTEX_SHADER: u32 = 0x8B31;

/// `createShader(type)` argument of a GL shader.
fn gl_shader_type(cap: &Capture, shader: ObjId) -> Option<u32> {
    let (sec, i) = creation_call(cap, shader)?;
    let c = cap.section(sec).get(i)?;
    if cap.method(c) != "createShader" {
        return None;
    }
    c.args.first()?.as_f64().map(|t| t as u32)
}

/// Shaders attached to `prog` as of `frame[upto]` (setup log, then frame calls).
fn gl_attached(cap: &Capture, prog: ObjId, upto: usize) -> Vec<ObjId> {
    let mut out = Vec::new();
    for c in cap.setup.iter().chain(cap.frame.iter().take(upto + 1)) {
        let m = cap.method(c);
        if (m != "attachShader" && m != "detachShader")
            || c.args.first().and_then(Value::as_obj) != Some(prog)
        {
            continue;
        }
        let Some(sh) = c.args.get(1).and_then(Value::as_obj) else {
            continue;
        };
        if m == "attachShader" {
            if !out.contains(&sh) {
                out.push(sh);
            }
        } else {
            out.retain(|x| *x != sh);
        }
    }
    out
}

fn gl_program_props(
    cap: &Capture,
    prog_label: Option<&str>,
    prog: Option<ObjId>,
    shaders: &[ObjId],
    ty: u32,
    stage_name: &str,
) -> Props {
    let mut out = vec![(
        "program".to_string(),
        match prog {
            Some(p) => Cell::Obj(p),
            None => Cell::Text(prog_label.unwrap_or("null").to_string()),
        },
    )];
    for &sh in shaders {
        match gl_shader_type(cap, sh) {
            Some(t) if t == ty => out.push((stage_name.to_string(), Cell::Obj(sh))),
            Some(_) => {}
            None => out.push(("shader".to_string(), Cell::Obj(sh))),
        }
    }
    out
}

fn gl_pages<'a>(l: &'a Loaded, insp: &'a Inspection) -> Vec<Page<'a>> {
    let cap = &*l.cap;
    let call = cap.frame.get(insp.index);
    let program = top(insp, "Program");
    let prog_label = leaf_in(program, "CURRENT_PROGRAM");
    let prog = prog_label.and_then(|v| parse_obj(cap, v));
    let shaders = prog
        .map(|p| gl_attached(cap, p, insp.index))
        .unwrap_or_default();
    let group = |name: &str| Item::Nodes(pick(top(insp, name), |_| true));
    let uniforms = || {
        Item::Bindings(rows(insp, |b| {
            matches!(b.group.as_str(), "Uniforms" | "Uniform blocks")
        }))
    };

    let (mut ia, mut vs, mut rs, mut fs, mut ds, mut co) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    if let Some(c) = call
        && classify(cap, c) == CmdKind::Draw
    {
        push(&mut ia, "Draw call", Item::Props(call_props(cap, c, true)));
    }
    push(
        &mut ia,
        "Vertex attributes",
        Item::Bindings(rows(insp, |b| b.group == "Attributes")),
    );
    push(
        &mut ia,
        "Index buffer",
        Item::Bindings(rows(insp, |b| b.group == "Index buffer")),
    );

    push(
        &mut vs,
        "Program",
        Item::Props(gl_program_props(
            cap,
            prog_label,
            prog,
            &shaders,
            GL_VERTEX_SHADER,
            "vertex shader",
        )),
    );
    push(&mut vs, "Program state", group("Program"));
    push(&mut vs, "Uniforms & uniform blocks", uniforms());

    push(&mut rs, "Rasterizer", group("Rasterizer"));
    push(&mut rs, "Viewport & scissor", group("Viewport/Scissor"));
    push(&mut rs, "Multisample", group("Multisample"));

    push(
        &mut fs,
        "Program",
        Item::Props(gl_program_props(
            cap,
            prog_label,
            prog,
            &shaders,
            GL_FRAGMENT_SHADER,
            "fragment shader",
        )),
    );
    push(
        &mut fs,
        "Textures & samplers",
        Item::Bindings(rows(insp, |b| b.group == "Textures")),
    );
    push(&mut fs, "Uniforms & uniform blocks", uniforms());

    push(&mut ds, "Depth", group("Depth"));
    push(&mut ds, "Stencil", group("Stencil"));
    push(
        &mut ds,
        "Attachments",
        Item::Targets(target_idx(insp, true)),
    );

    push(&mut co, "Blend", group("Blend"));
    push(&mut co, "Color mask", group("ColorMask"));
    push(&mut co, "Draw buffers", group("DrawBuffers"));
    push(
        &mut co,
        "Attachments",
        Item::Targets(target_idx(insp, false)),
    );

    render_pages(ia, vs, rs, fs, ds, co, insp)
}
