//! Log panel: replay errors and resource notes of the latest replay, filterable by severity.
use crate::state::AppState;
use crate::widgets::{ERROR, WARN};
use egui::{Color32, RichText, TextStyle};
use webgd_core::ObjId;
use webgd_core::display::obj_label;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    fn tag(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warn ",
            Severity::Info => "info ",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Severity::Error => ERROR,
            Severity::Warning => WARN,
            Severity::Info => Color32::from_rgb(150, 170, 190),
        }
    }
}

/// Where a log entry points to.
#[derive(Clone, Copy)]
enum Loc {
    /// Setup / snapshot section (no frame index).
    PreFrame,
    Frame(usize),
    Resource(ObjId),
}

struct Entry {
    sev: Severity,
    loc: Loc,
    msg: String,
}

/// Per-panel filter, kept in egui temp memory.
#[derive(Clone)]
struct Filter {
    errors: bool,
    warnings: bool,
    info: bool,
    text: String,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            errors: true,
            warnings: true,
            info: true,
            text: String::new(),
        }
    }
}

/// Errors caused by capabilities missing on the replay side rather than by a failing call.
fn is_warning(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("missing feature") || m.contains("unsupported") || m.contains("not captured")
}

/// Severity filter toggle: a colored dot marks the severity; the label stays in the regular text
/// color on a faint tint (on) or no fill (off), so it reads in both states.
fn severity_chip(ui: &mut egui::Ui, on: &mut bool, name: &str, n: usize, color: Color32) {
    let v = ui.visuals();
    let (fill, stroke, text) = if *on {
        (
            color.gamma_multiply(0.18),
            egui::Stroke::new(1.0, color.gamma_multiply(0.7)),
            v.strong_text_color(),
        )
    } else {
        (
            Color32::TRANSPARENT,
            egui::Stroke::new(1.0, v.widgets.inactive.bg_stroke.color),
            v.weak_text_color(),
        )
    };
    // Leading spaces leave room for the painted dot.
    let resp = ui.add(
        egui::Button::new(RichText::new(format!("    {name} {n}")).color(text))
            .fill(fill)
            .stroke(stroke),
    );
    let dot_color = if *on {
        color
    } else {
        color.gamma_multiply(0.45)
    };
    let c = egui::pos2(
        resp.rect.left() + ui.spacing().button_padding.x + 4.0,
        resp.rect.center().y,
    );
    ui.painter().circle_filled(c, 3.5, dot_color);
    if resp
        .on_hover_text(if *on {
            "Click to hide"
        } else {
            "Click to show"
        })
        .clicked()
    {
        *on = !*on;
    }
}

pub fn show(ui: &mut egui::Ui, st: &mut AppState) {
    let Some(l) = &st.loaded else {
        return super::no_capture(ui);
    };
    let generation = l.generation;
    let cap = l.cap.clone();

    // Header: replay in flight + latest result.
    let busy = st.replay.busy();
    let (head, entries) = st.replay.results(|r| match &r.replay {
        Some((k, res)) if k.generation == generation => match res {
            Err(e) => (
                Some((format!("replay to {} failed: {e}", k.label()), ERROR)),
                Vec::new(),
            ),
            Ok(info) => {
                let mut entries: Vec<Entry> = info
                    .errors
                    .iter()
                    .map(|(i, msg)| Entry {
                        sev: if is_warning(msg) {
                            Severity::Warning
                        } else {
                            Severity::Error
                        },
                        loc: if *i == usize::MAX {
                            Loc::PreFrame
                        } else {
                            Loc::Frame(*i)
                        },
                        msg: msg.clone(),
                    })
                    .collect();
                let mut notes: Vec<(&ObjId, &Vec<String>)> = info.notes.iter().collect();
                notes.sort_by_key(|(id, _)| **id);
                for (id, list) in notes {
                    for msg in list {
                        entries.push(Entry {
                            sev: Severity::Info,
                            loc: Loc::Resource(*id),
                            msg: msg.clone(),
                        });
                    }
                }
                (
                    Some((
                        format!("replay to {}: {} error(s)", k.label(), info.errors.len()),
                        ui.visuals().text_color(),
                    )),
                    entries,
                )
            }
        },
        _ => (None, Vec::new()),
    });

    let filter_id = ui.id().with("webgd_log_filter");
    let mut filter: Filter =
        ui.data_mut(|d| d.get_temp_mut_or_default::<Filter>(filter_id).clone());
    let count = |s: Severity| entries.iter().filter(|e| e.sev == s).count();
    ui.horizontal(|ui| {
        severity_chip(
            ui,
            &mut filter.errors,
            "Errors",
            count(Severity::Error),
            ERROR,
        );
        severity_chip(
            ui,
            &mut filter.warnings,
            "Warnings",
            count(Severity::Warning),
            WARN,
        );
        severity_chip(
            ui,
            &mut filter.info,
            "Notes",
            count(Severity::Info),
            Severity::Info.color(),
        );
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut filter.text)
                .hint_text("filter")
                .desired_width(160.0),
        );
        if let Some(k) = busy {
            ui.spinner();
            ui.weak(format!("replaying to {}…", k.label()));
        }
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    match &head {
        Some((text, color)) => {
            ui.label(RichText::new(text).color(*color));
        }
        None => {
            ui.weak("no replay yet");
        }
    }
    ui.separator();

    let needle = filter.text.to_ascii_lowercase();
    let shown: Vec<&Entry> = entries
        .iter()
        .filter(|e| match e.sev {
            Severity::Error => filter.errors,
            Severity::Warning => filter.warnings,
            Severity::Info => filter.info,
        })
        .filter(|e| needle.is_empty() || e.msg.to_ascii_lowercase().contains(&needle))
        .collect();

    let selected = st.selected;
    let mut select = None;
    let mut open = None;
    let row_h = ui
        .text_style_height(&TextStyle::Monospace)
        .max(ui.spacing().interact_size.y);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_h, shown.len(), |ui, range| {
            for e in &shown[range] {
                let is_sel = matches!(e.loc, Loc::Frame(i) if Some(i) == selected);
                let row = ui
                    .horizontal(|ui| {
                        ui.label(RichText::new(e.sev.tag()).monospace().color(e.sev.color()));
                        let loc_text = match e.loc {
                            Loc::PreFrame => "pre-frame".to_owned(),
                            Loc::Frame(i) => format!("#{i}"),
                            Loc::Resource(id) => obj_label(&cap, id),
                        };
                        let loc = RichText::new(format!("{loc_text:<10}")).monospace();
                        match e.loc {
                            Loc::PreFrame => {
                                ui.label(loc.weak());
                            }
                            Loc::Frame(i) => {
                                if ui.selectable_label(is_sel, loc).clicked() {
                                    select = Some(i);
                                }
                            }
                            Loc::Resource(id) => {
                                if ui.link(loc).clicked() {
                                    open = Some(id);
                                }
                            }
                        }
                        ui.add(egui::Label::new(&e.msg).truncate());
                    })
                    .response;
                // Frame errors: the whole row selects the failing command.
                if let Loc::Frame(i) = e.loc {
                    let row = row.interact(egui::Sense::click()).on_hover_text(&e.msg);
                    if row.clicked() {
                        select = Some(i);
                    }
                }
            }
        });
    if let Some(i) = select {
        st.select(i);
    }
    if let Some(id) = open {
        st.open_resource(id);
    }
}
