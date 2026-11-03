//! Shared widgets and look: palette, theme, descriptor trees, banners, image textures.
use egui::{Color32, CornerRadius, Margin, RichText, Stroke, TextureHandle, TextureOptions, vec2};
use webgd_core::display::{obj_label, value_label};
use webgd_core::pixels::Image;
use webgd_core::track::commands::CmdKind;
use webgd_core::track::revisions::Access;
use webgd_core::{Capture, ObjId, Value};

/// Primary highlight: selection, active workspace, focused items.
pub const ACCENT: Color32 = Color32::from_rgb(46, 140, 160);
pub const WARN: Color32 = Color32::from_rgb(230, 200, 60);
pub const ERROR: Color32 = Color32::from_rgb(235, 96, 96);
/// Fill of the app chrome (menu/tool bar, status bar).
pub const CHROME_FILL: Color32 = Color32::from_rgb(24, 26, 30);

const PANEL_FILL: Color32 = Color32::from_rgb(32, 35, 40);
const WINDOW_FILL: Color32 = Color32::from_rgb(38, 41, 47);
const EXTREME_FILL: Color32 = Color32::from_rgb(18, 20, 23);
const FAINT_FILL: Color32 = Color32::from_rgb(37, 40, 46);
const BORDER: Color32 = Color32::from_rgb(54, 58, 66);
const LINK: Color32 = Color32::from_rgb(112, 196, 214);

/// Dark theme used by the whole app; applied once at startup.
pub fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        let sp = &mut s.spacing;
        sp.item_spacing = vec2(6.0, 3.0);
        sp.button_padding = vec2(6.0, 2.0);
        sp.interact_size.y = 18.0;
        sp.indent = 14.0;
        sp.window_margin = Margin::same(6);
        sp.menu_margin = Margin::same(4);

        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = PANEL_FILL;
        v.window_fill = WINDOW_FILL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.extreme_bg_color = EXTREME_FILL;
        v.faint_bg_color = FAINT_FILL;
        v.code_bg_color = EXTREME_FILL;
        v.hyperlink_color = LINK;
        v.warn_fg_color = WARN;
        v.error_fg_color = ERROR;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.window_corner_radius = CornerRadius::same(4);
        v.menu_corner_radius = CornerRadius::same(4);
        v.striped = true;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL_FILL;
        w.noninteractive.weak_bg_fill = PANEL_FILL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.bg_fill = Color32::from_rgb(46, 50, 57);
        w.inactive.weak_bg_fill = Color32::from_rgb(46, 50, 57);
        w.hovered.bg_fill = Color32::from_rgb(58, 64, 73);
        w.hovered.weak_bg_fill = Color32::from_rgb(58, 64, 73);
        w.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.bg_fill = Color32::from_rgb(68, 76, 86);
        w.active.weak_bg_fill = Color32::from_rgb(68, 76, 86);
        w.active.bg_stroke = Stroke::new(1.0, LINK);
        w.open.bg_fill = Color32::from_rgb(46, 50, 57);
        w.open.weak_bg_fill = Color32::from_rgb(46, 50, 57);
        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            wv.corner_radius = CornerRadius::same(3);
        }
    });
}

/// Kind indicator color of an event (event list strip, scrubber/trace kind strip).
pub fn kind_color(kind: &CmdKind) -> Color32 {
    match kind {
        CmdKind::Draw => Color32::from_rgb(112, 192, 124),
        CmdKind::Dispatch => Color32::from_rgb(186, 142, 228),
        CmdKind::Clear => Color32::from_rgb(228, 158, 84),
        CmdKind::Copy => Color32::from_rgb(104, 168, 228),
        CmdKind::Submit => Color32::from_rgb(214, 196, 96),
        CmdKind::GroupBegin(_) | CmdKind::GroupEnd => Color32::from_rgb(150, 162, 178),
        CmdKind::PassBegin | CmdKind::PassEnd => Color32::from_rgb(110, 196, 206),
        CmdKind::Call => Color32::from_rgb(182, 186, 192),
    }
}

/// Golden-angle hue step (137.5°) as a fraction of the hue circle.
const GOLDEN_HUE: f64 = 0.381_966_011_250_105;

/// Fill color of one action: hue rotates by the golden angle per `ordinal` (the action's position
/// among the frame's actions), so neighbours differ strongly and colors are stable across frames.
/// The kind sets saturation/value: shading work (draw, dispatch) is brightest, transfers dimmest.
pub fn event_color(kind: &CmdKind, ordinal: usize) -> Color32 {
    let (s, v) = match kind {
        CmdKind::Draw | CmdKind::Dispatch => (0.55, 0.80),
        CmdKind::Clear => (0.45, 0.72),
        CmdKind::Copy | CmdKind::Submit => (0.36, 0.64),
        _ => (0.24, 0.60),
    };
    hsv(((ordinal as f64 * GOLDEN_HUE).fract()) as f32, s, v)
}

/// sRGB-space HSV (all in 0..=1) to an opaque color.
fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let sector = h6.floor();
    let f = h6 - sector;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match sector as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let u = |c: f32| (c * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(u(r), u(g), u(b))
}

/// Black or white, whichever contrasts more with `bg` (WCAG relative luminance).
pub fn text_on(bg: Color32) -> Color32 {
    let lin = egui::ecolor::linear_f32_from_gamma_u8;
    let lum = 0.2126 * lin(bg.r()) + 0.7152 * lin(bg.g()) + 0.0722 * lin(bg.b());
    // Equal contrast point: (L + 0.05) / 0.05 == 1.05 / (L + 0.05).
    if lum > 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// Color of a resource revision by access type.
pub fn access_color(a: Access) -> Color32 {
    match a {
        Access::Create => Color32::from_rgb(112, 200, 132),
        Access::Write => Color32::from_rgb(232, 150, 72),
        Access::Read => Color32::from_rgb(110, 170, 232),
        Access::Destroy => Color32::from_rgb(220, 96, 96),
    }
}

/// Muted palette for marker/group ranges.
const MARKER_PALETTE: [Color32; 12] = [
    Color32::from_rgb(78, 121, 140),
    Color32::from_rgb(140, 104, 72),
    Color32::from_rgb(92, 132, 92),
    Color32::from_rgb(128, 88, 128),
    Color32::from_rgb(140, 124, 70),
    Color32::from_rgb(70, 116, 120),
    Color32::from_rgb(134, 84, 92),
    Color32::from_rgb(96, 104, 148),
    Color32::from_rgb(112, 132, 74),
    Color32::from_rgb(150, 98, 120),
    Color32::from_rgb(84, 128, 112),
    Color32::from_rgb(124, 112, 96),
];

/// Stable color for a marker/group range: FNV-1a hash of `label`, offset by nesting depth.
pub fn marker_color(depth: usize, label: &str) -> Color32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in label.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let i = (h as usize).wrapping_add(depth.wrapping_mul(5)) % MARKER_PALETTE.len();
    MARKER_PALETTE[i]
}

pub fn banner(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(Color32::from_rgb(80, 68, 10))
        .inner_margin(4.0)
        .corner_radius(3.0)
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(WARN));
        });
}

/// Clickable `Class#id` link; returns true when clicked.
pub fn obj_link(ui: &mut egui::Ui, cap: &Capture, id: ObjId) -> bool {
    ui.link(obj_label(cap, id)).clicked()
}

/// Generic Value tree (Dict/Array → collapsible groups, ObjRef → link). Sets `clicked` on link click.
pub fn value_tree(
    ui: &mut egui::Ui,
    cap: &Capture,
    name: &str,
    v: &Value,
    clicked: &mut Option<ObjId>,
    id_salt: egui::Id,
) {
    let id = id_salt.with(name);
    match v {
        Value::Dict(entries) => {
            egui::CollapsingHeader::new(name)
                .id_salt(id)
                .default_open(true)
                .show(ui, |ui| {
                    for (k, child) in entries {
                        value_tree(ui, cap, cap.str(*k), child, clicked, id);
                    }
                });
        }
        Value::Array(items) if !items.is_empty() => {
            egui::CollapsingHeader::new(format!("{name} [{}]", items.len()))
                .id_salt(id)
                .default_open(true)
                .show(ui, |ui| {
                    for (i, child) in items.iter().enumerate() {
                        value_tree(ui, cap, &format!("[{i}]"), child, clicked, id);
                    }
                });
        }
        Value::ObjRef(oid) => {
            ui.horizontal(|ui| {
                ui.label(format!("{name}:"));
                if obj_link(ui, cap, *oid) {
                    *clicked = Some(*oid);
                }
            });
        }
        Value::String(s) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{name}:"));
                ui.label(RichText::new(cap.str(*s)).monospace());
            });
        }
        other => {
            ui.horizontal(|ui| {
                ui.label(format!("{name}:"));
                ui.label(RichText::new(value_label(cap, other)).monospace());
            });
        }
    }
}

/// Texture cache for one displayed image; reuploads only when `key` changes.
pub struct ImageTex<K: PartialEq> {
    cached: Option<(K, TextureHandle)>,
}

impl<K: PartialEq> Default for ImageTex<K> {
    fn default() -> Self {
        ImageTex { cached: None }
    }
}

impl<K: PartialEq> ImageTex<K> {
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        key: K,
        image: &Image,
        range: (f64, f64),
        channels: [bool; 4],
    ) -> TextureHandle {
        if let Some((k, h)) = &self.cached
            && *k == key
        {
            return h.clone();
        }
        let h = load_image(ctx, image, range, channels, TextureOptions::NEAREST);
        self.cached = Some((key, h.clone()));
        h
    }
}

pub fn load_image(
    ctx: &egui::Context,
    image: &Image,
    range: (f64, f64),
    channels: [bool; 4],
    opts: TextureOptions,
) -> TextureHandle {
    let rgba = image.to_display_rgba8(range, channels);
    let size = [image.width as usize, image.height as usize];
    let color = if rgba.len() == size[0] * size[1] * 4 {
        egui::ColorImage::from_rgba_unmultiplied(size, &rgba)
    } else {
        egui::ColorImage::new([1, 1], vec![Color32::from_rgb(255, 0, 255)])
    };
    ctx.load_texture("webgd-image", color, opts)
}

/// Default display range: pass-through for 8-bit normalized formats, auto range otherwise.
pub fn default_range(image: &Image) -> (f64, f64) {
    if image.format.is_passthrough() {
        (0.0, 255.0)
    } else {
        image.auto_range()
    }
}

/// Compact number formatting for counters / readouts.
pub fn fmt_num(v: f64) -> String {
    let a = v.abs();
    if v == v.trunc() && a < 1e9 {
        format!("{v}")
    } else if a >= 1e6 {
        format!("{:.3e}", v)
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.4}")
    }
}

pub fn fmt_ns(ns: f64) -> String {
    if ns >= 1e6 {
        format!("{:.3} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.2} µs", ns / 1e3)
    } else {
        format!("{ns:.0} ns")
    }
}
