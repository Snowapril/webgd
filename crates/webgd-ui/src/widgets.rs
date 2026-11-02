//! Shared widgets: descriptor trees, banners, image textures.
use egui::{Color32, RichText, TextureHandle, TextureOptions};
use webgd_core::display::{obj_label, value_label};
use webgd_core::pixels::Image;
use webgd_core::{Capture, ObjId, Value};

pub const WARN: Color32 = Color32::from_rgb(230, 200, 60);

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
