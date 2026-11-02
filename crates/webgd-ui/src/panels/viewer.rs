//! Resource Viewer: textures, buffers, shaders, programs and descriptor trees, plus the
//! resource's revision history (create / write / read / destroy events).
use crate::replay_queue::{Key, ViewData, ViewReq};
use crate::state::{AppState, Loaded, Tab};
use crate::widgets::{
    ACCENT, ImageTex, access_color, banner, default_range, fmt_num, obj_link, value_tree,
};
use egui::{Color32, Pos2, Rect, RichText, Sense, TextStyle, Vec2, vec2};
use std::collections::HashMap;
use webgd_analysis::{Stage, StageStats};
use webgd_core::pixels::{Image, f16_to_f64};
use webgd_core::track::gl::{TEXTURE_2D_ARRAY, TEXTURE_3D, TEXTURE_CUBE_MAP};
use webgd_core::track::resources::wgsl_entry_points;
use webgd_core::track::revisions::{self, Access, Revision};
use webgd_core::{Call, Capture, ObjId, Section, Value};

const TEXTURE_CLASSES: &[&str] = &[
    "GPUTexture",
    "GPUTextureView",
    "WebGLTexture",
    "WebGLRenderbuffer",
];
const BUFFER_CLASSES: &[&str] = &["GPUBuffer", "WebGLBuffer"];
const GL_VERTEX_SHADER: f64 = 35633.0;
const GL_FRAGMENT_SHADER: f64 = 35632.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Elem {
    Hex,
    U8,
    U16,
    U32,
    I32,
    F32,
    F16,
}

impl Elem {
    const ALL: [Elem; 7] = [
        Elem::Hex,
        Elem::U8,
        Elem::U16,
        Elem::U32,
        Elem::I32,
        Elem::F32,
        Elem::F16,
    ];
    fn name(self) -> &'static str {
        match self {
            Elem::Hex => "hex",
            Elem::U8 => "u8",
            Elem::U16 => "u16",
            Elem::U32 => "u32",
            Elem::I32 => "i32",
            Elem::F32 => "f32",
            Elem::F16 => "f16",
        }
    }
    fn size(self) -> usize {
        match self {
            Elem::Hex | Elem::U8 => 1,
            Elem::U16 | Elem::F16 => 2,
            Elem::U32 | Elem::I32 | Elem::F32 => 4,
        }
    }
    fn fmt(self, b: &[u8]) -> String {
        match self {
            Elem::Hex => format!("{:02x}", b[0]),
            Elem::U8 => format!("{:>3}", b[0]),
            Elem::U16 => format!("{:>5}", u16::from_le_bytes([b[0], b[1]])),
            Elem::U32 => format!("{:>10}", u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Elem::I32 => format!("{:>11}", i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Elem::F32 => format!(
                "{:>12}",
                fmt_f(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64)
            ),
            Elem::F16 => format!(
                "{:>10}",
                fmt_f(f16_to_f64(u16::from_le_bytes([b[0], b[1]])))
            ),
        }
    }
}

fn fmt_f(v: f64) -> String {
    if v.is_finite() && v != 0.0 && (v.abs() >= 1e6 || v.abs() < 1e-4) {
        format!("{v:.4e}")
    } else {
        format!("{v:.6}")
    }
}

struct TexUi {
    mip: u32,
    layer: u32,
    channels: [bool; 4],
    range: (f64, f64),
    /// Image (generation, request) the range was last initialised for.
    range_for: Option<(u64, ViewReq)>,
    /// Auto button pressed: recompute from the shown image.
    auto: bool,
    zoom: f32,
    pan: Vec2,
    fit: bool,
    tex: ImageTex<(Key, ViewReq, [u64; 2], [bool; 4])>,
}

impl Default for TexUi {
    fn default() -> Self {
        TexUi {
            mip: 0,
            layer: 0,
            channels: [true; 4],
            range: (0.0, 1.0),
            range_for: None,
            auto: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            tex: ImageTex::default(),
        }
    }
}

struct BufUi {
    elem: Elem,
    cols: usize,
    offset: usize,
}

impl Default for BufUi {
    fn default() -> Self {
        BufUi {
            elem: Elem::Hex,
            cols: 4,
            offset: 0,
        }
    }
}

type StatsKey = (ObjId, String, Stage, usize);

/// Position of an event in capture order: setup calls precede frame calls.
type EventPos = (u8, usize);

fn event_pos(section: Section, index: usize) -> EventPos {
    match section {
        Section::Frame => (1, index),
        _ => (0, index),
    }
}

/// Position just after every setup call and before the first frame call.
const FRAME_START: EventPos = (0, usize::MAX);

/// Where the current selection sits relative to a resource history.
struct Cursor {
    /// Explicitly selected event (pre-frame setup call or frame command).
    selected: Option<EventPos>,
    /// Selection position used for prev/next navigation.
    pos: EventPos,
    /// Point whose contents the replay shows (`FRAME_START` when the replay stops at frame start).
    contents: EventPos,
    /// Replay stop (frame index; `None` = frame start).
    stop: Option<usize>,
}

impl Cursor {
    fn new(st: &AppState, stop: Option<usize>) -> Self {
        let contents = stop.map_or(FRAME_START, |s| (1, s));
        let selected = st
            .selected_setup
            .map(|i| (0, i))
            .or(st.selected.map(|i| (1, i)));
        let pos = st.selected_setup.map_or(contents, |i| (0, i));
        Cursor {
            selected,
            pos,
            contents,
            stop,
        }
    }

    /// Content revision of `hist` at the replay point.
    fn revision(&self, hist: &[Revision]) -> Option<u32> {
        match self.stop {
            Some(s) => revisions::revision_at(hist, s),
            None => hist
                .iter()
                .take_while(|r| r.section != Section::Frame)
                .last()
                .map(|r| r.revision),
        }
    }

    /// Index into `hist` of the Create/Write event that produced the content revision at the replay point.
    fn producing(&self, hist: &[Revision]) -> Option<usize> {
        self.revision(hist)?;
        hist.iter().rposition(|r| {
            matches!(r.access, Access::Create | Access::Write)
                && event_pos(r.section, r.index) <= self.contents
        })
    }
}

fn access_name(a: Access) -> &'static str {
    match a {
        Access::Create => "create",
        Access::Write => "write",
        Access::Read => "read",
        Access::Destroy => "destroy",
    }
}

/// `Revision r · contents at …` line for the viewer header.
fn contents_header(hist: &[Revision], cur: &Cursor, parent: bool) -> String {
    let rev = cur.revision(hist);
    let at = match cur.stop {
        Some(s)
            if rev.is_none()
                || hist
                    .iter()
                    .any(|r| r.section == Section::Frame && r.index <= s) =>
        {
            format!("command #{s}")
        }
        _ => "frame start".to_owned(),
    };
    let of = if parent { " of parent texture" } else { "" };
    match rev {
        Some(r) => format!("Revision {r}{of} · contents at {at}"),
        None if hist.is_empty() => format!("contents at {at}"),
        None => format!("not created yet at {at}"),
    }
}

const REV_COLS: [(&str, f32); 6] = [
    ("Rev", 44.0),
    ("Section", 64.0),
    ("Event", 56.0),
    ("Access", 54.0),
    ("Call", 280.0),
    ("Detail", 220.0),
];

/// Fixed-width, left-aligned table cell.
fn cell(ui: &mut egui::Ui, w: f32, h: f32, widget: impl egui::Widget) -> egui::Response {
    ui.allocate_ui_with_layout(
        vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(w);
            ui.add(widget)
        },
    )
    .inner
}

/// Virtualized revision table; returns the clicked revision.
fn revision_table(
    ui: &mut egui::Ui,
    salt: &str,
    l: &Loaded,
    hist: &[Revision],
    cur: &Cursor,
    scroll_to_current: bool,
    max_h: f32,
) -> Option<Revision> {
    if hist.is_empty() {
        ui.weak("no recorded events");
        return None;
    }
    let cap = &l.cap;
    let row_h = ui.text_style_height(&TextStyle::Body) + 2.0;
    let spacing = ui.spacing().item_spacing;
    let total_w: f32 =
        REV_COLS.iter().map(|c| c.1).sum::<f32>() + spacing.x * REV_COLS.len() as f32;
    let producing = cur.producing(hist);
    let mut picked = None;
    egui::ScrollArea::horizontal()
        .id_salt((salt, "h"))
        // Take the panel width instead of requesting the full table width (which would widen the panel).
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (name, w) in REV_COLS {
                    cell(ui, w, row_h, egui::Label::new(RichText::new(name).strong()));
                }
            });
            let mut area = egui::ScrollArea::vertical()
                .id_salt((salt, "v"))
                .max_height(max_h.max(row_h * 3.0))
                .auto_shrink([false, true]);
            if scroll_to_current {
                let target = hist
                    .iter()
                    .position(|r| Some(event_pos(r.section, r.index)) == cur.selected)
                    .or(producing);
                if let Some(t) = target {
                    let step = row_h + spacing.y;
                    area = area.vertical_scroll_offset((t as f32 * step - 2.0 * step).max(0.0));
                }
            }
            area.show_rows(ui, row_h, hist.len(), |ui, range| {
                for i in range {
                    let r = &hist[i];
                    let pos = event_pos(r.section, r.index);
                    let is_sel = cur.selected == Some(pos);
                    let is_cur = producing == Some(i);
                    let bg = Rect::from_min_size(ui.cursor().min, vec2(total_w, row_h + spacing.y));
                    let resp = ui
                        .interact(bg, ui.id().with((salt, i)), Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if is_sel {
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
                    } else if i % 2 == 1 {
                        ui.painter()
                            .rect_filled(bg, 0.0, ui.visuals().faint_bg_color);
                    }
                    let call = match r.section {
                        Section::Frame => l.labels.get(r.index).cloned(),
                        s => cap
                            .section(s)
                            .get(r.index)
                            .map(|c| webgd_core::display::call_label(cap, c)),
                    }
                    .unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.set_height(row_h);
                        let label = |t: RichText| egui::Label::new(t).truncate().selectable(false);
                        let rev = if is_cur {
                            RichText::new(format!("• {}", r.revision))
                                .monospace()
                                .strong()
                                .color(ACCENT)
                        } else {
                            RichText::new(r.revision.to_string()).monospace()
                        };
                        let rev_resp = cell(ui, REV_COLS[0].1, row_h, label(rev));
                        if is_cur {
                            rev_resp.on_hover_text("current revision at the selection");
                        }
                        let section = match r.section {
                            Section::Frame => RichText::new("frame"),
                            _ => RichText::new("pre-frame").weak(),
                        };
                        cell(ui, REV_COLS[1].1, row_h, label(section));
                        cell(
                            ui,
                            REV_COLS[2].1,
                            row_h,
                            label(RichText::new(format!("#{}", r.index)).monospace()),
                        );
                        cell(
                            ui,
                            REV_COLS[3].1,
                            row_h,
                            label(
                                RichText::new(access_name(r.access)).color(access_color(r.access)),
                            ),
                        );
                        cell(
                            ui,
                            REV_COLS[4].1,
                            row_h,
                            label(RichText::new(call).monospace()),
                        );
                        cell(
                            ui,
                            REV_COLS[5].1,
                            row_h,
                            label(RichText::new(&r.detail).weak()),
                        );
                    });
                    if resp.clicked() {
                        picked = Some(r.clone());
                    }
                }
            });
        });
    picked
}

#[derive(Default)]
pub struct ViewerUi {
    current: Option<ObjId>,
    tex: TexUi,
    buf: BufUi,
    stats: HashMap<StatsKey, Result<StageStats, String>>,
    /// Revisions panel collapsed by the user.
    rev_hidden: bool,
    /// (resource, selection) the revision tables were last scrolled for.
    rev_scrolled: Option<(ObjId, Option<EventPos>, EventPos)>,
}

fn creation(cap: &Capture, id: ObjId) -> Option<&Call> {
    webgd_core::track::gpu::creation_call(cap, id).and_then(|(s, i)| cap.section(s).get(i))
}

fn creation_desc(cap: &Capture, id: ObjId) -> Option<&Value> {
    creation(cap, id).and_then(|c| c.args.first())
}

/// Texture behind a view (or the id itself).
fn base_texture(cap: &Capture, id: ObjId) -> ObjId {
    if cap.class_of(id) == "GPUTextureView" {
        cap.objects.get(&id).and_then(|o| o.parent).unwrap_or(id)
    } else {
        id
    }
}

/// (mip count, layer count, layers are cube faces) at `stop` (`None` = frame start).
fn tex_extent(cap: &Capture, id: ObjId, stop: Option<usize>, mip: u32) -> (u32, u32, bool) {
    let tex = base_texture(cap, id);
    match cap.class_of(tex) {
        "GPUTexture" => {
            let Some(desc) = creation_desc(cap, tex).filter(|d| matches!(d, Value::Dict(_))) else {
                return (1, 1, false);
            };
            let mips = cap
                .dict_get(desc, "mipLevelCount")
                .and_then(Value::as_f64)
                .unwrap_or(1.0) as u32;
            let layers = match cap.dict_get(desc, "size") {
                Some(Value::Array(a)) => a.get(2).and_then(Value::as_f64).unwrap_or(1.0),
                Some(d @ Value::Dict(_)) => cap
                    .dict_get(d, "depthOrArrayLayers")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0),
                _ => 1.0,
            } as u32;
            let is_3d = cap
                .dict_get(desc, "dimension")
                .is_some_and(|d| matches!(d, Value::String(s) if cap.str(*s) == "3d"));
            let layers = if is_3d {
                (layers >> mip).max(1)
            } else {
                layers
            };
            (mips.max(1), layers.max(1), false)
        }
        "WebGLTexture" => {
            let dims = match stop {
                Some(s) => webgd_core::track::gl::texture_dims(cap, s, tex),
                None => webgd_core::track::gl::texture_dims_at_frame_start(cap, tex),
            };
            let Some(d) = dims else {
                return (1, 1, false);
            };
            let mips = d.levels.keys().map(|(l, _)| l + 1).max().unwrap_or(1);
            match d.target {
                TEXTURE_CUBE_MAP => (mips, 6, true),
                TEXTURE_3D | TEXTURE_2D_ARRAY => (
                    mips,
                    d.levels.get(&(mip, 0)).map(|l| l.2).unwrap_or(1).max(1),
                    false,
                ),
                _ => (mips, 1, false),
            }
        }
        _ => (1, 1, false),
    }
}

/// Setup calls plus frame calls up to `stop` inclusive (`None` = setup only, i.e. frame start).
fn calls_upto(cap: &Capture, stop: Option<usize>) -> impl Iterator<Item = &Call> {
    cap.setup
        .iter()
        .chain(cap.frame.iter().take(stop.map_or(0, |s| s + 1)))
}

/// Latest `shaderSource` for a GL shader at `stop`.
fn gl_shader_source(cap: &Capture, id: ObjId, stop: Option<usize>) -> Option<&str> {
    calls_upto(cap, stop)
        .filter(|c| {
            cap.method(c) == "shaderSource" && c.args.first().and_then(Value::as_obj) == Some(id)
        })
        .last()
        .and_then(|c| match c.args.get(1) {
            Some(Value::String(s)) => Some(cap.str(*s)),
            _ => None,
        })
}

fn gl_shader_stage(cap: &Capture, id: ObjId) -> Option<Stage> {
    let c = cap
        .setup
        .iter()
        .chain(&cap.frame)
        .find(|c| cap.method(c) == "createShader" && c.result.as_obj() == Some(id))?;
    match c.args.first().and_then(Value::as_f64) {
        Some(GL_VERTEX_SHADER) => Some(Stage::Vertex),
        Some(GL_FRAGMENT_SHADER) => Some(Stage::Fragment),
        _ => None,
    }
}

fn wgsl_entries(src: &str) -> Vec<(String, Stage)> {
    wgsl_entry_points(src)
        .into_iter()
        .filter_map(|(stage, name)| {
            let stage = match stage {
                "vertex" => Stage::Vertex,
                "fragment" => Stage::Fragment,
                "compute" => Stage::Compute,
                _ => return None,
            };
            Some((name, stage))
        })
        .collect()
}

fn stage_name(s: Stage) -> &'static str {
    match s {
        Stage::Vertex => "vertex",
        Stage::Fragment => "fragment",
        Stage::Compute => "compute",
    }
}

pub fn stats_table(ui: &mut egui::Ui, rows: &[(String, Stage, &Result<StageStats, String>)]) {
    egui::Grid::new("shader-stats")
        .striped(true)
        .show(ui, |ui| {
            for h in [
                "entry",
                "stage",
                "ALU (est.)",
                "branches",
                "tex samples",
                "regs (vec4)",
                "workgroup",
                "wg bytes",
                "occupancy (est.)",
            ] {
                ui.strong(h);
            }
            ui.end_row();
            for (entry, stage, res) in rows {
                ui.monospace(entry);
                ui.label(stage_name(*stage));
                match res {
                    Ok(s) => {
                        ui.monospace(fmt_num(s.alu));
                        ui.monospace(fmt_num(s.branches));
                        ui.monospace(fmt_num(s.tex_samples));
                        ui.monospace(s.est_regs_vec4.to_string());
                        ui.monospace(
                            s.workgroup_size
                                .map(|w| format!("{}×{}×{}", w[0], w[1], w[2]))
                                .unwrap_or_else(|| "—".into()),
                        );
                        ui.monospace(s.workgroup_bytes.to_string());
                        ui.monospace(format!("{:.0}%", webgd_analysis::occupancy(s) * 100.0));
                    }
                    Err(e) => {
                        ui.label(RichText::new("n/a").weak()).on_hover_text(e);
                        for _ in 0..6 {
                            ui.label("");
                        }
                    }
                }
                ui.end_row();
            }
        });
}

impl ViewerUi {
    /// Replay read needed for the resource currently viewed.
    pub fn wanted_read(&self, l: &Loaded, view: Option<ObjId>) -> Option<ViewReq> {
        let id = view?;
        let class = l.cap.class_of(id);
        if TEXTURE_CLASSES.contains(&class) {
            // Mip/layer belong to the previous resource until `show` resets them.
            let (mip, layer) = if self.current == Some(id) {
                (self.tex.mip, self.tex.layer)
            } else {
                (0, 0)
            };
            Some(ViewReq::Texture { id, mip, layer })
        } else if BUFFER_CLASSES.contains(&class) {
            Some(ViewReq::Buffer(id))
        } else if class == "WebGLShader" {
            Some(ViewReq::Shader(id))
        } else if class == "WebGLProgram" {
            Some(ViewReq::Program(id))
        } else {
            None
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            return super::no_capture(ui);
        };
        let Some(id) = st.view else {
            ui.weak("Select a resource in Resources, Bindings or Render Targets.");
            return;
        };
        if self.current != Some(id) {
            self.current = Some(id);
            self.tex = TexUi::default();
            self.buf = BufUi::default();
            self.rev_scrolled = None;
        }
        let Some(key) = st.replay_key() else {
            ui.weak("Nothing to replay.");
            return;
        };
        let cap = l.cap.clone();
        let stop = key.stop;
        let class = cap.class_of(id).to_owned();
        let row = l.resources.iter().find(|r| r.id == id);
        let mut open: Option<ObjId> = None;
        let mut picked: Option<Revision> = None;

        // Texture views share their parent texture's contents, so its history drives the header
        // and prev/next navigation.
        let base = base_texture(&cap, id);
        let empty: &[Revision] = &[];
        let own_hist = l.revisions.get(&id).map(Vec::as_slice).unwrap_or(empty);
        let content_hist = l.revisions.get(&base).map(Vec::as_slice).unwrap_or(empty);
        let cursor = Cursor::new(st, stop);

        let mut expanded = !self.rev_hidden;
        // ≈35% of the viewer by default (≥ 240 px), never more than 55%, so the contents keep most of the width.
        let full_w = ui.available_width();
        let rev_max_w = (full_w * 0.55).max(96.0);
        let rev_min_w = 240.0_f32.min(rev_max_w);
        let rev_default_w = (full_w * 0.35).clamp(rev_min_w, rev_max_w);
        egui::Panel::right(egui::Id::new("webgd-viewer-revisions-side"))
            .resizable(true)
            .default_size(rev_default_w)
            .size_range(rev_min_w..=rev_max_w)
            .show_collapsible(ui, &mut expanded, |ui| {
                let tag = (id, cursor.selected, cursor.pos);
                let scroll = self.rev_scrolled != Some(tag);
                self.rev_scrolled = Some(tag);
                let writes = own_hist
                    .iter()
                    .filter(|r| r.access == Access::Write)
                    .count();
                ui.horizontal(|ui| {
                    ui.strong("Revisions");
                    ui.weak(format!("{} events · {writes} writes", own_hist.len()));
                });
                if base != id {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Parent texture").strong());
                        if obj_link(ui, &cap, base) {
                            open = Some(base);
                        }
                    });
                    let h = (ui.available_height() - 60.0) * 0.6;
                    if let Some(r) =
                        revision_table(ui, "rev-parent", l, content_hist, &cursor, scroll, h)
                    {
                        picked = Some(r);
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "This view ({})",
                            webgd_core::display::obj_label(&cap, id)
                        ))
                        .strong(),
                    );
                }
                let h = ui.available_height() - 24.0;
                if let Some(r) = revision_table(ui, "rev-own", l, own_hist, &cursor, scroll, h) {
                    picked = Some(r);
                }
            });
        self.rev_hidden = !expanded;

        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(webgd_core::display::obj_label(&cap, id));
                if let Some(r) = row {
                    if !r.label.is_empty() {
                        ui.label(RichText::new(format!("\"{}\"", r.label)).italics());
                    }
                    ui.weak(&r.summary);
                }
            });
            if let Some(parent) = cap.objects.get(&id).and_then(|o| o.parent)
                && !cap.class_of(parent).is_empty()
            {
                ui.horizontal(|ui| {
                    ui.weak("parent:");
                    if obj_link(ui, &cap, parent) {
                        open = Some(parent);
                    }
                });
            }
            let notes: Vec<String> = st.replay.results(|r| match &r.replay {
                Some((k, Ok(info))) if k.generation == l.generation => {
                    let mut n = info.notes.get(&id).cloned().unwrap_or_default();
                    if base != id {
                        n.extend(info.notes.get(&base).cloned().unwrap_or_default());
                    }
                    n
                }
                _ => Vec::new(),
            });
            for n in &notes {
                banner(ui, n);
            }
            ui.separator();

            let want = self.wanted_read(l, Some(id));
            // Latest read of this exact request (may be from a previous stop while the new one replays).
            let current_ok = st.replay.results(|r| {
                r.view
                    .as_ref()
                    .is_some_and(|(k, v, _)| *k == key && Some(*v) == want)
            });
            ui.horizontal(|ui| {
                let writes: Vec<&Revision> = content_hist
                    .iter()
                    .filter(|r| r.access == Access::Write)
                    .collect();
                let prev = writes
                    .iter()
                    .rev()
                    .find(|r| event_pos(r.section, r.index) < cursor.pos);
                let next = writes
                    .iter()
                    .find(|r| event_pos(r.section, r.index) > cursor.pos);
                if ui
                    .add_enabled(
                        prev.is_some(),
                        egui::Button::new(RichText::new("←").monospace()),
                    )
                    .on_hover_text("Previous write")
                    .clicked()
                {
                    picked = prev.map(|r| (*r).clone());
                }
                if ui
                    .add_enabled(
                        next.is_some(),
                        egui::Button::new(RichText::new("→").monospace()),
                    )
                    .on_hover_text("Next write")
                    .clicked()
                {
                    picked = next.map(|r| (*r).clone());
                }
                ui.label(
                    RichText::new(contents_header(content_hist, &cursor, base != id)).strong(),
                );
                if want.is_some() && !current_ok {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut shown = !self.rev_hidden;
                    if ui
                        .toggle_value(&mut shown, "Revisions")
                        .on_hover_text("Show the revision history panel")
                        .changed()
                    {
                        self.rev_hidden = !shown;
                    }
                });
            });
            if st.selected_setup.is_some() {
                ui.weak("pre-frame call selected: replay shows the state at frame start");
            }

            if let Some(o) = descriptor_ui(ui, &cap, id, &class) {
                open = Some(o);
            }
            if TEXTURE_CLASSES.contains(&class.as_str()) {
                self.texture_ui(ui, st, &cap, id, key, want);
            } else if BUFFER_CLASSES.contains(&class.as_str()) {
                self.buffer_ui(ui, st, want);
            } else if class == "WebGLShader" || class == "GPUShaderModule" {
                self.shader_ui(ui, st, &cap, id, key, stop);
            } else if class == "WebGLProgram"
                && let Some(o) = self.program_ui(ui, st, &cap, id, key, stop)
            {
                open = Some(o);
            }
        });
        if let Some(rev) = picked {
            st.select_revision(&rev);
            // Keep the viewer (same resource, same viewer state) in front.
            st.focus_tab = Some(Tab::Viewer);
        }
        if let Some(o) = open {
            st.open_resource(o);
        }
    }

    fn texture_ui(
        &mut self,
        ui: &mut egui::Ui,
        st: &AppState,
        cap: &Capture,
        id: ObjId,
        key: Key,
        want: Option<ViewReq>,
    ) {
        let (mips, layers, cube) = tex_extent(cap, id, key.stop, self.tex.mip);
        let t = &mut self.tex;
        ui.horizontal_wrapped(|ui| {
            ui.label("mip");
            ui.add(egui::DragValue::new(&mut t.mip).range(0..=mips - 1));
            ui.label(if cube { "face" } else { "layer" });
            ui.add(egui::DragValue::new(&mut t.layer).range(0..=layers - 1));
            ui.separator();
            for (i, n) in ["R", "G", "B", "A"].iter().enumerate() {
                ui.toggle_value(&mut t.channels[i], *n);
            }
            ui.separator();
            ui.label("range");
            let speed = ((t.range.1 - t.range.0).abs() / 200.0).max(1e-4);
            ui.add(
                egui::DragValue::new(&mut t.range.0)
                    .speed(speed)
                    .max_decimals(4),
            );
            ui.add(
                egui::DragValue::new(&mut t.range.1)
                    .speed(speed)
                    .max_decimals(4),
            );
            if ui.button("Auto").clicked() {
                t.auto = true;
            }
            ui.separator();
            if ui.button("Fit").clicked() {
                t.fit = true;
            }
            ui.label(format!("{:.3}×", t.zoom));
        });
        t.mip = t.mip.min(mips - 1);
        t.layer = t.layer.min(layers - 1);
        let ctx = ui.ctx().clone();
        st.replay.results(|r| {
            let Some((k, v, res)) = &r.view else {
                ui.weak("reading…");
                return;
            };
            if Some(*v) != want && !matches!((v, want), (ViewReq::Texture { id: a, .. }, Some(ViewReq::Texture { id: b, .. })) if *a == b) {
                ui.weak("reading…");
                return;
            }
            let img = match res {
                Ok(ViewData::Texture(img)) => img,
                Ok(_) => return,
                Err(e) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                    return;
                }
            };
            if t.auto || t.range_for != Some((k.generation, *v)) {
                t.range = if t.auto { img.auto_range() } else { default_range(img) };
                t.range_for = Some((k.generation, *v));
                t.auto = false;
            }
            ui.weak(format!("{}×{} {:?}", img.width, img.height, img.format));
            let handle = t.tex.get(&ctx, (*k, *v, [t.range.0.to_bits(), t.range.1.to_bits()], t.channels), img, t.range, t.channels);
            image_view(ui, t, img, &handle);
        });
    }

    fn buffer_ui(&mut self, ui: &mut egui::Ui, st: &AppState, want: Option<ViewReq>) {
        let b = &mut self.buf;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("buf-elem")
                .selected_text(b.elem.name())
                .show_ui(ui, |ui| {
                    for e in Elem::ALL {
                        ui.selectable_value(&mut b.elem, e, e.name());
                    }
                });
            ui.label("columns");
            ui.add(egui::DragValue::new(&mut b.cols).range(1..=16));
            ui.label("byte offset");
            ui.add(egui::DragValue::new(&mut b.offset).speed(4.0));
        });
        st.replay.results(|r| {
            let Some((_, v, res)) = &r.view else {
                ui.weak("reading…");
                return;
            };
            if Some(*v) != want {
                ui.weak("reading…");
                return;
            }
            let bytes = match res {
                Ok(ViewData::Buffer(b)) => b,
                Ok(_) => return,
                Err(e) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                    return;
                }
            };
            b.offset = b.offset.min(bytes.len());
            ui.weak(format!("{} bytes", bytes.len()));
            let es = b.elem.size();
            let row_bytes = es * b.cols;
            let data = &bytes[b.offset..];
            let rows = data.len().div_ceil(row_bytes);
            let row_h = ui.text_style_height(&TextStyle::Monospace);
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows, |ui, range| {
                    for r in range {
                        let start = r * row_bytes;
                        let mut line = format!("{:08x}: ", b.offset + start);
                        for c in 0..b.cols {
                            let o = start + c * es;
                            if o + es <= data.len() {
                                line.push_str(&b.elem.fmt(&data[o..o + es]));
                                line.push(' ');
                            }
                        }
                        ui.add(egui::Label::new(RichText::new(line).monospace()).extend());
                    }
                });
        });
    }

    fn shader_ui(
        &mut self,
        ui: &mut egui::Ui,
        st: &AppState,
        cap: &Capture,
        id: ObjId,
        key: Key,
        stop: Option<usize>,
    ) {
        let (src, rows): (String, Vec<(String, Stage)>) = if cap.class_of(id) == "WebGLShader" {
            let src = gl_shader_source(cap, id, stop).unwrap_or("").to_owned();
            let rows = gl_shader_stage(cap, id)
                .map(|s| vec![("main".to_owned(), s)])
                .unwrap_or_default();
            (src, rows)
        } else {
            let src = creation_desc(cap, id)
                .and_then(|d| cap.dict_get(d, "code"))
                .and_then(|v| match v {
                    Value::String(s) => Some(cap.str(*s).to_owned()),
                    _ => None,
                })
                .unwrap_or_default();
            let rows = wgsl_entries(&src);
            (src, rows)
        };
        if cap.class_of(id) == "WebGLShader" {
            st.replay.results(|r| {
                if let Some((k, ViewReq::Shader(sid), Ok(ViewData::ShaderLog(log)))) = &r.view
                    && *sid == id
                    && k.generation == key.generation
                {
                    match log.as_deref() {
                        Some(l) if !l.trim().is_empty() => {
                            ui.label("Info log:");
                            ui.label(
                                RichText::new(l)
                                    .monospace()
                                    .color(Color32::from_rgb(255, 160, 120)),
                            );
                        }
                        Some(_) => {
                            ui.weak("Info log: (empty)");
                        }
                        None => {
                            ui.weak("Info log unavailable");
                        }
                    }
                }
            });
        }
        let src_len = src.len();
        let results: Vec<(String, Stage, Result<StageStats, String>)> = rows
            .into_iter()
            .map(|(entry, stage)| {
                let r = self
                    .stats
                    .entry((id, entry.clone(), stage, src_len))
                    .or_insert_with(|| {
                        if cap.class_of(id) == "WebGLShader" {
                            webgd_analysis::glsl_stats(&src, stage)
                        } else {
                            webgd_analysis::wgsl_stats(&src, Some(&entry), stage)
                        }
                        .map_err(|e| e.to_string())
                    })
                    .clone();
                (entry, stage, r)
            })
            .collect();
        egui::CollapsingHeader::new("Shader stats (est.)")
            .default_open(true)
            .show(ui, |ui| {
                let refs: Vec<_> = results.iter().map(|(e, s, r)| (e.clone(), *s, r)).collect();
                if refs.is_empty() {
                    ui.weak("no entry points found");
                } else {
                    egui::ScrollArea::horizontal()
                        .id_salt("stats-h")
                        .show(ui, |ui| stats_table(ui, &refs));
                }
            });
        egui::ScrollArea::both()
            .id_salt("shader-src")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut s = src.as_str();
                ui.add(
                    egui::TextEdit::multiline(&mut s)
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
            });
    }

    fn program_ui(
        &mut self,
        ui: &mut egui::Ui,
        st: &AppState,
        cap: &Capture,
        id: ObjId,
        key: Key,
        stop: Option<usize>,
    ) -> Option<ObjId> {
        let mut open = None;
        let mut attached: Vec<ObjId> = Vec::new();
        for c in calls_upto(cap, stop) {
            if c.this.0 == 0 || c.args.first().and_then(Value::as_obj) != Some(id) {
                continue;
            }
            let Some(sh) = c.args.get(1).and_then(Value::as_obj) else {
                continue;
            };
            match cap.method(c) {
                "attachShader" if !attached.contains(&sh) => attached.push(sh),
                "detachShader" => attached.retain(|s| *s != sh),
                _ => {}
            }
        }
        ui.horizontal(|ui| {
            ui.label("Attached shaders:");
            for sh in &attached {
                if obj_link(ui, cap, *sh) {
                    open = Some(*sh);
                }
            }
        });
        st.replay.results(|r| {
            let Some((k, ViewReq::Program(pid), Ok(ViewData::Program(info)))) = &r.view else {
                return;
            };
            if *pid != id || k.generation != key.generation {
                return;
            }
            let Some(info) = info else {
                ui.weak("program not present in replay");
                return;
            };
            ui.label(format!(
                "Link status: {}",
                if info.link_status { "ok" } else { "FAILED" }
            ));
            if !info.info_log.trim().is_empty() {
                ui.label(
                    RichText::new(&info.info_log)
                        .monospace()
                        .color(Color32::from_rgb(255, 160, 120)),
                );
            }
            egui::CollapsingHeader::new(format!("Active attributes ({})", info.attributes.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for a in &info.attributes {
                        ui.monospace(a);
                    }
                });
            egui::CollapsingHeader::new(format!("Active uniforms ({})", info.uniforms.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for u in &info.uniforms {
                        ui.monospace(u);
                    }
                });
        });
        open
    }
}

/// Creation call, descriptor tree and GL snapshot dicts; returns a clicked ObjRef.
fn descriptor_ui(ui: &mut egui::Ui, cap: &Capture, id: ObjId, class: &str) -> Option<ObjId> {
    let mut clicked = None;
    let creation_call = creation(cap, id);
    let snapshot: Vec<&Call> = cap
        .snapshot
        .iter()
        .filter(|c| {
            cap.method(c).starts_with("webgd:gl")
                && c.args.first().and_then(Value::as_obj) == Some(id)
        })
        .collect();
    if creation_call.is_none() && snapshot.is_empty() {
        return None;
    }
    let open_default = !(TEXTURE_CLASSES.contains(&class)
        || BUFFER_CLASSES.contains(&class)
        || class.contains("Shader")
        || class == "WebGLProgram");
    egui::CollapsingHeader::new("Descriptor")
        .id_salt(("descriptor", id))
        .default_open(open_default)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("desc-tree")
                .auto_shrink([false, true])
                .max_height(if open_default { f32::INFINITY } else { 240.0 })
                .show(ui, |ui| {
                    if let Some(c) = creation_call {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("created by");
                            ui.label(
                                RichText::new(webgd_core::display::call_label(cap, c)).monospace(),
                            );
                        });
                        if cap.method(c) == "getBindGroupLayout" {
                            ui.horizontal(|ui| {
                                ui.label("auto layout from");
                                if obj_link(ui, cap, c.this) {
                                    clicked = Some(c.this);
                                }
                            });
                        }
                        for (i, a) in c.args.iter().enumerate() {
                            if matches!(a, Value::Dict(_) | Value::Array(_)) {
                                value_tree(
                                    ui,
                                    cap,
                                    &format!("arg{i}"),
                                    a,
                                    &mut clicked,
                                    egui::Id::new(("desc", id)),
                                );
                            }
                        }
                    }
                    for c in snapshot {
                        ui.separator();
                        ui.weak(format!("pre-frame snapshot ({})", cap.method(c)));
                        for (i, a) in c.args.iter().enumerate().skip(1) {
                            if let Value::Dict(entries) = a {
                                for (k, v) in entries {
                                    if cap.str(*k) == "data" {
                                        ui.label(format!(
                                            "data: {}",
                                            webgd_core::display::value_label(cap, v)
                                        ));
                                    } else {
                                        value_tree(
                                            ui,
                                            cap,
                                            cap.str(*k),
                                            v,
                                            &mut clicked,
                                            egui::Id::new(("snap", id, i)),
                                        );
                                    }
                                }
                            } else {
                                value_tree(
                                    ui,
                                    cap,
                                    &format!("arg{i}"),
                                    a,
                                    &mut clicked,
                                    egui::Id::new(("snap", id, i)),
                                );
                            }
                        }
                    }
                });
        });
    clicked
}

/// Pannable / zoomable image with hover texel readout.
fn image_view(ui: &mut egui::Ui, t: &mut TexUi, img: &Image, handle: &egui::TextureHandle) {
    let readout_h = ui.text_style_height(&TextStyle::Monospace) + 6.0;
    let avail = ui.available_size() - vec2(0.0, readout_h);
    let (rect, resp) = ui.allocate_exact_size(avail.max(vec2(64.0, 64.0)), Sense::click_and_drag());
    let size = vec2(img.width.max(1) as f32, img.height.max(1) as f32);
    if t.fit {
        t.zoom = (rect.width() / size.x)
            .min(rect.height() / size.y)
            .clamp(0.125, 32.0);
        t.pan = Vec2::ZERO;
        t.fit = false;
    }
    if resp.hovered() {
        let (zoom_delta, pointer) = ui.input(|i| (i.zoom_delta(), i.pointer.hover_pos()));
        if zoom_delta != 1.0
            && let Some(p) = pointer
        {
            let old = t.zoom;
            t.zoom = (t.zoom * zoom_delta).clamp(0.125, 32.0);
            // Keep the point under the cursor fixed.
            let c = rect.center() + t.pan;
            let rel = (p - c) / old;
            t.pan += rel * old - rel * t.zoom;
        }
    }
    if resp.dragged() {
        t.pan += resp.drag_delta();
    }
    let disp = size * t.zoom;
    let img_rect = Rect::from_center_size(rect.center() + t.pan, disp);
    let painter = ui.painter_at(rect);
    // Checkerboard background reveals alpha.
    painter.rect_filled(rect, 0.0, Color32::from_gray(30));
    painter.rect_filled(img_rect, 0.0, Color32::from_gray(60));
    painter.image(
        handle.id(),
        img_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    painter.rect_stroke(
        img_rect,
        0.0,
        (1.0, Color32::from_gray(90)),
        egui::StrokeKind::Outside,
    );
    let mut readout = String::from(" ");
    if let Some(p) = resp.hover_pos() {
        let uv = (p - img_rect.min) / t.zoom;
        if uv.x >= 0.0 && uv.y >= 0.0 && uv.x < size.x && uv.y < size.y {
            let (x, y) = (uv.x as u32, uv.y as u32);
            let v = img.texel(x, y);
            readout = format!(
                "{x}, {y}: {} {} {} {}",
                fmt_num(v[0]),
                fmt_num(v[1]),
                fmt_num(v[2]),
                fmt_num(v[3])
            );
        }
    }
    ui.label(RichText::new(readout).monospace());
}
