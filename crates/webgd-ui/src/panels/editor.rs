//! Command Editor: typed argument editors for the selected frame command, insertion of dynamic
//! state commands before/after it, revert/reset, and the list of every edit. Edits are
//! session-only; `AppState::apply_edits` turns them into the edited capture every panel shows.
use crate::state::{AppState, Loaded};
use crate::widgets::{origin_badge, origin_style};
use egui::{Color32, DragValue, Id, Response, RichText, Sense, vec2};
use std::collections::HashMap;
use webgd_core::display::{call_label, obj_label, value_label};
use webgd_core::edits::{self, CaptureEdits, CommandTemplate, EditOrigin, ParamKind};
use webgd_core::track::resources::RESOURCE_CLASSES;
use webgd_core::{Call, Capture, ObjId, Sym, Value};

/// Working copy of the edits while the selected command's arguments are being changed; applied
/// when a drag stops, a field loses focus, or a checkbox/combo changes.
struct Draft {
    generation: u64,
    index: usize,
    args: Vec<Value>,
    edits: CaptureEdits,
    /// Text being typed into string fields (interned when the field loses focus).
    texts: HashMap<Id, String>,
    /// Colours being edited (their dict keys may not be interned in the shown capture yet).
    colors: HashMap<Id, [f64; 4]>,
    /// `args` differ from the applied edits.
    dirty: bool,
}

#[derive(Default)]
pub struct EditorUi {
    draft: Option<Draft>,
    confirm_reset: bool,
}

enum Act {
    Apply(CaptureEdits),
    /// Apply, then select the command with this origin.
    ApplySelect(CaptureEdits, EditOrigin),
    Select(EditOrigin),
}

impl EditorUi {
    pub fn show(&mut self, ui: &mut egui::Ui, st: &mut AppState) {
        let Some(l) = &st.loaded else {
            self.draft = None;
            return super::no_capture(ui);
        };
        let sel = st
            .selected
            .filter(|&i| i < l.cap.frame.len() && st.selected_setup.is_none());
        // A change still pending on a command that is no longer selected is applied first.
        if let Some(d) = self
            .draft
            .take_if(|d| d.generation != l.generation || Some(d.index) != sel)
            && d.dirty
            && d.generation == l.generation
        {
            st.apply_edits(d.edits);
            ui.ctx().request_repaint();
            return;
        }
        let act = egui::ScrollArea::vertical()
            .id_salt("webgd-command-editor")
            .auto_shrink([false, false])
            .show(ui, |ui| self.body(ui, l, sel))
            .inner;
        match act {
            Some(Act::Apply(e)) => {
                self.draft = None;
                st.apply_edits(e);
            }
            Some(Act::ApplySelect(e, o)) => {
                self.draft = None;
                st.apply_edits(e);
                st.select_origin(o);
            }
            Some(Act::Select(o)) => st.select_origin(o),
            None => {}
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, l: &Loaded, sel: Option<usize>) -> Option<Act> {
        let mut act = None;
        let n_edits = l.edit_count();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    n_edits > 0,
                    egui::Button::new(format!("Reset all edits ({n_edits})…")),
                )
                .clicked()
            {
                self.confirm_reset = true;
            }
        });
        ui.label(
            RichText::new(
                "Edits are session-only: replays use them, File > Save .webgd keeps the original capture.",
            )
            .small()
            .weak(),
        );
        if self.confirm_reset {
            let m = egui::Modal::new(Id::new("webgd-command-editor-reset")).show(ui.ctx(), |ui| {
                ui.label(RichText::new("Reset all edits?").strong());
                ui.label(format!(
                    "Discards {n_edits} edit{} (modified and inserted commands).",
                    if n_edits == 1 { "" } else { "s" }
                ));
                let mut answer = None;
                ui.horizontal(|ui| {
                    if ui.button("Reset all edits").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
                answer
            });
            match m.inner {
                Some(true) => {
                    self.confirm_reset = false;
                    act = Some(Act::Apply(CaptureEdits::default()));
                }
                Some(false) => self.confirm_reset = false,
                None if m.should_close() => self.confirm_reset = false,
                None => {}
            }
        }
        ui.separator();

        match sel {
            Some(i) => {
                if let Some(a) = self.command(ui, l, i) {
                    act = Some(a);
                }
            }
            None => {
                ui.weak("Select a frame command in Events to edit its arguments or insert commands next to it.");
            }
        }
        ui.separator();
        if let Some(a) = edit_list(ui, l, sel) {
            act = Some(a);
        }
        act
    }

    /// Edits of the loaded capture including the pending draft of command `i`.
    fn base_edits(&self, l: &Loaded, i: usize) -> CaptureEdits {
        match &self.draft {
            Some(d) if d.generation == l.generation && d.index == i => d.edits.clone(),
            _ => l.edits.clone(),
        }
    }

    /// Header, actions and argument editors of edited frame command `i`.
    fn command(&mut self, ui: &mut egui::Ui, l: &Loaded, i: usize) -> Option<Act> {
        let cap = &*l.cap;
        let call = &cap.frame[i];
        let origin = l.origin_of(i);
        let mut act = None;

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("#{i}")).monospace().strong());
            origin_badge(ui, origin);
        });
        ui.add(egui::Label::new(RichText::new(&l.labels[i]).monospace()).wrap());
        match origin {
            EditOrigin::Modified(o) => {
                if let Some(c) = l.original.frame.get(o) {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("original:");
                        ui.label(RichText::new(call_label(&l.original, c)).monospace().weak());
                    });
                }
            }
            EditOrigin::Inserted { before, .. } => {
                ui.weak(if before < l.original.frame.len() {
                    format!("inserted before original frame call #{before}")
                } else {
                    "inserted at the end of the frame".to_owned()
                });
            }
            EditOrigin::Original(_) => {}
        }

        ui.horizontal(|ui| {
            let (text, tip) = match origin {
                EditOrigin::Inserted { .. } => ("Delete", "Remove this inserted command"),
                _ => ("Revert", "Restore the recorded arguments"),
            };
            if ui
                .add_enabled(
                    !matches!(origin, EditOrigin::Original(_)),
                    egui::Button::new(text),
                )
                .on_hover_text(tip)
                .clicked()
            {
                let mut e = self.base_edits(l, i);
                revert(&mut e, origin);
                act = Some(Act::Apply(e));
            }
            for after in [false, true] {
                if let Some((recv, t)) = insert_menu(ui, l, i, after) {
                    let e = self.base_edits(l, i);
                    act = Some(insert(l, e, origin, after, recv, &t));
                }
            }
        });
        if act.is_some() {
            return act;
        }

        ui.separator();
        ui.label(RichText::new("Arguments").strong());
        let specs = edits::params_for_call(cap, call);
        if self
            .draft
            .as_ref()
            .is_some_and(|d| d.generation != l.generation || d.index != i)
        {
            self.draft = None;
        }
        let draft = self.draft.get_or_insert_with(|| Draft {
            generation: l.generation,
            index: i,
            args: call.args.clone(),
            edits: l.edits.clone(),
            texts: HashMap::new(),
            colors: HashMap::new(),
            dirty: false,
        });
        let Draft {
            args,
            edits: work,
            texts,
            colors,
            dirty,
            ..
        } = draft;
        let mut ed = Ed {
            l,
            edits: work,
            texts,
            colors,
            labels: l
                .resources
                .iter()
                .map(|r| (r.id, r.label.as_str()))
                .collect(),
            changed: false,
            commit: false,
        };
        let id = Id::new(("webgd-command-editor-args", l.generation, i));
        if args.is_empty() {
            ui.weak("No arguments.");
        }
        match &specs {
            Some(specs) => {
                egui::Grid::new(id)
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (k, spec) in specs.iter().enumerate() {
                            ui.label(spec.name);
                            match args.get_mut(k) {
                                Some(v) => ed.typed(ui, id.with(k), &spec.kind, v),
                                None => {
                                    ui.weak("(not recorded)");
                                }
                            }
                            ui.end_row();
                        }
                        // Trailing arguments without a spec (e.g. dynamic offsets) stay unchanged.
                        for (k, v) in args.iter().enumerate().skip(specs.len()) {
                            ui.label(format!("arg {k}"));
                            ui.label(RichText::new(value_label(cap, v)).monospace().weak())
                                .on_hover_text("kept unchanged");
                            ui.end_row();
                        }
                    });
            }
            None => {
                for (k, v) in args.iter_mut().enumerate() {
                    ed.generic_row(ui, id.with(k), &format!("arg {k}"), v);
                }
            }
        }
        let (changed, commit) = (ed.changed, ed.commit);
        drop(ed);
        if changed {
            store_args(work, &l.original, origin, args.clone());
            *dirty = true;
        }
        if commit && *dirty {
            return Some(Act::Apply(work.clone()));
        }
        None
    }
}

/// Argument editing context for one frame.
struct Ed<'a> {
    l: &'a Loaded,
    edits: &'a mut CaptureEdits,
    texts: &'a mut HashMap<Id, String>,
    colors: &'a mut HashMap<Id, [f64; 4]>,
    /// Resource label per resource id (every listed resource, label possibly empty).
    labels: HashMap<ObjId, &'a str>,
    /// A value changed this frame.
    changed: bool,
    /// The edit is finished (drag stopped, focus lost, discrete widget changed).
    commit: bool,
}

impl Ed<'_> {
    /// String of `s` in the edited capture (original strings followed by edit strings).
    fn sym_str(&self, s: Sym) -> &str {
        let n = self.l.original.strings.len();
        match (s.0 as usize).checked_sub(n) {
            Some(k) => self.edits.strings.get(k).map_or("", String::as_str),
            None => self.l.original.str(s),
        }
    }

    /// Records a continuous widget (drag value): live change and whether the edit is finished.
    fn track(&mut self, r: &Response) {
        if r.changed() {
            self.changed = true;
        }
        if r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus()) {
            self.commit = true;
        }
    }

    /// Records a discrete widget change (checkbox, combo): applied right away.
    fn immediate(&mut self) {
        self.changed = true;
        self.commit = true;
    }

    /// `Class#id "label"`.
    fn obj_text(&self, id: ObjId) -> String {
        let base = obj_label(&self.l.cap, id);
        match self.labels.get(&id) {
            Some(s) if !s.is_empty() => format!("{base} \"{s}\""),
            _ => base,
        }
    }

    /// Existing objects of `classes` (resources must be live in this capture).
    fn candidates(&self, classes: &[&str]) -> Vec<ObjId> {
        let cap = &*self.l.cap;
        cap.objects
            .values()
            .filter(|o| {
                let class = cap.str(o.class);
                classes.contains(&class)
                    && (!RESOURCE_CLASSES.contains(&class) || self.labels.contains_key(&o.id))
            })
            .map(|o| o.id)
            .collect()
    }

    fn object_combo(&mut self, ui: &mut egui::Ui, id: Id, classes: &[&str], v: &mut Value) {
        let cur = v.as_obj();
        let text = match cur {
            Some(o) => self.obj_text(o),
            None => value_label(&self.l.cap, v),
        };
        let mut pick = None;
        egui::ComboBox::from_id_salt(id)
            .selected_text(text)
            .width(240.0)
            .truncate()
            .show_ui(ui, |ui| {
                let options = self.candidates(classes);
                if options.is_empty() {
                    ui.weak("no objects of this class");
                }
                for o in options {
                    if ui
                        .selectable_label(cur == Some(o), self.obj_text(o))
                        .clicked()
                    {
                        pick = Some(o);
                    }
                }
            });
        if let Some(o) = pick
            && cur != Some(o)
        {
            *v = Value::ObjRef(o);
            self.immediate();
        }
    }

    /// Editor for a parameter with a known kind.
    fn typed(&mut self, ui: &mut egui::Ui, id: Id, kind: &ParamKind, v: &mut Value) {
        match kind {
            ParamKind::Float { min, max } => {
                let mut x = v.as_f64().unwrap_or(0.0);
                let speed = match (min, max) {
                    (Some(a), Some(b)) if b - a <= 4.0 => (b - a) / 200.0,
                    _ => 1.0,
                };
                let mut dv = DragValue::new(&mut x)
                    .speed(speed)
                    .update_while_editing(false);
                if min.is_some() || max.is_some() {
                    dv = dv.range(min.unwrap_or(f64::NEG_INFINITY)..=max.unwrap_or(f64::INFINITY));
                }
                let r = ui.add(dv);
                if r.changed() {
                    *v = Value::Number(x);
                }
                self.track(&r);
            }
            ParamKind::Int { min, max } => {
                let mut n = v.as_f64().unwrap_or(0.0) as i64;
                let mut dv = DragValue::new(&mut n)
                    .speed(0.25)
                    .update_while_editing(false);
                if min.is_some() || max.is_some() {
                    dv = dv.range(min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX));
                }
                let r = ui.add(dv);
                if r.changed() {
                    *v = Value::Number(n as f64);
                }
                self.track(&r);
            }
            ParamKind::Bool => {
                let mut b = v.as_f64().is_some_and(|n| n != 0.0);
                if ui.checkbox(&mut b, "").changed() {
                    *v = Value::Bool(b);
                    self.immediate();
                }
            }
            ParamKind::Enum(names) => {
                let cur = v.as_f64().map(|n| n as u32);
                let text = match cur {
                    Some(c) => names
                        .iter()
                        .find(|(_, x)| *x == c)
                        .map_or_else(|| format!("{c:#x}"), |(n, _)| (*n).to_owned()),
                    None => value_label(&self.l.cap, v),
                };
                let mut pick = None;
                egui::ComboBox::from_id_salt(id)
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for &(name, x) in names.iter() {
                            if ui.selectable_label(cur == Some(x), name).clicked() {
                                pick = Some(x);
                            }
                        }
                    });
                if let Some(x) = pick
                    && cur != Some(x)
                {
                    *v = Value::Number(x as f64);
                    self.immediate();
                }
            }
            ParamKind::StrEnum(names) => {
                let cur = match v {
                    Value::String(s) => Some(self.sym_str(*s).to_owned()),
                    _ => None,
                };
                let text = cur.clone().unwrap_or_else(|| value_label(&self.l.cap, v));
                let mut pick = None;
                egui::ComboBox::from_id_salt(id)
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for &name in names.iter() {
                            if ui
                                .selectable_label(cur.as_deref() == Some(name), name)
                                .clicked()
                            {
                                pick = Some(name);
                            }
                        }
                    });
                if let Some(name) = pick
                    && cur.as_deref() != Some(name)
                {
                    let sym = self.edits.intern(&self.l.original, name);
                    *v = Value::String(sym);
                    self.immediate();
                }
            }
            ParamKind::Bits(bits) => {
                let before = v.as_f64().unwrap_or(0.0) as u32;
                let mut cur = before;
                ui.horizontal_wrapped(|ui| {
                    for &(name, bit) in bits.iter() {
                        let mut on = cur & bit != 0;
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                cur |= bit;
                            } else {
                                cur &= !bit;
                            }
                        }
                    }
                    let known = bits.iter().fold(0, |a, &(_, b)| a | b);
                    let rest = cur & !known;
                    if rest != 0 {
                        ui.weak(format!("+{rest:#x}"));
                    }
                });
                if cur != before {
                    *v = Value::Number(cur as f64);
                    self.immediate();
                }
            }
            ParamKind::Object(classes) => self.object_combo(ui, id, classes, v),
            ParamKind::Color => {
                let cur = self
                    .colors
                    .get(&id)
                    .copied()
                    .or_else(|| edits::color_components(&self.l.cap, v))
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                let mut c = cur;
                let mut resps = Vec::with_capacity(4);
                ui.horizontal(|ui| {
                    for (k, name) in ["r", "g", "b", "a"].into_iter().enumerate() {
                        ui.weak(name);
                        resps.push(
                            ui.add(
                                DragValue::new(&mut c[k])
                                    .speed(0.005)
                                    .range(0.0..=1.0)
                                    .max_decimals(3)
                                    .update_while_editing(false),
                            ),
                        );
                    }
                    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    let u8c = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                    ui.painter().rect_filled(
                        rect,
                        2.0,
                        Color32::from_rgba_unmultiplied(u8c(c[0]), u8c(c[1]), u8c(c[2]), u8c(c[3])),
                    );
                });
                for r in &resps {
                    self.track(r);
                }
                if c != cur {
                    self.colors.insert(id, c);
                    *v = edits::color_value(&self.l.original, self.edits, c);
                }
            }
        }
    }

    /// Generic editor row for a value without a parameter spec.
    fn generic_row(&mut self, ui: &mut egui::Ui, id: Id, name: &str, v: &mut Value) {
        match v {
            Value::Dict(entries) => {
                egui::CollapsingHeader::new(name)
                    .id_salt(id)
                    .default_open(true)
                    .show(ui, |ui| {
                        for (k, (key, child)) in entries.iter_mut().enumerate() {
                            let key = self.sym_str(*key).to_owned();
                            self.generic_row(ui, id.with(k), &key, child);
                        }
                    });
            }
            Value::Array(items) if !items.is_empty() => {
                egui::CollapsingHeader::new(format!("{name} [{}]", items.len()))
                    .id_salt(id)
                    .default_open(true)
                    .show(ui, |ui| {
                        for (k, child) in items.iter_mut().enumerate() {
                            self.generic_row(ui, id.with(k), &format!("[{k}]"), child);
                        }
                    });
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.label(format!("{name}:"));
                    self.generic_leaf(ui, id, v);
                });
            }
        }
    }

    fn generic_leaf(&mut self, ui: &mut egui::Ui, id: Id, v: &mut Value) {
        match v {
            Value::Number(n) => {
                let mut x = *n;
                let speed = if x.fract() == 0.0 { 1.0 } else { 0.01 };
                let r = ui.add(
                    DragValue::new(&mut x)
                        .speed(speed)
                        .update_while_editing(false),
                );
                if r.changed() {
                    *v = Value::Number(x);
                }
                self.track(&r);
            }
            Value::Bool(b) => {
                if ui.checkbox(b, "").changed() {
                    self.immediate();
                }
            }
            Value::String(s) => {
                let cur = self.sym_str(*s).to_owned();
                let buf = self.texts.entry(id).or_insert_with(|| cur.clone());
                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(220.0));
                if r.lost_focus() {
                    let text = self.texts.remove(&id).unwrap_or_default();
                    if text != cur {
                        let sym = self.edits.intern(&self.l.original, &text);
                        *v = Value::String(sym);
                        self.immediate();
                    }
                } else if !r.has_focus() {
                    self.texts.remove(&id);
                }
            }
            Value::ObjRef(o) => {
                let class = self.l.cap.class_of(*o).to_owned();
                self.object_combo(ui, id, &[class.as_str()], v);
            }
            other => {
                ui.label(
                    RichText::new(value_label(&self.l.cap, other))
                        .monospace()
                        .weak(),
                )
                .on_hover_text("not editable");
            }
        }
    }
}

/// Writes edited `args` of the command with origin `o` (dropping a modification equal to the
/// recorded arguments).
fn store_args(e: &mut CaptureEdits, original: &Capture, o: EditOrigin, args: Vec<Value>) {
    match o {
        EditOrigin::Original(i) | EditOrigin::Modified(i) => {
            if original.frame.get(i).is_some_and(|c| c.args == args) {
                e.modified.remove(&i);
            } else {
                e.modified.insert(i, args);
            }
        }
        EditOrigin::Inserted { before, slot } => {
            if let Some(c) = e.inserted.get_mut(&before).and_then(|v| v.get_mut(slot)) {
                c.args = args;
            }
        }
    }
}

/// Removes the modification, or deletes the inserted command.
fn revert(e: &mut CaptureEdits, o: EditOrigin) {
    match o {
        EditOrigin::Original(_) => {}
        EditOrigin::Modified(i) => {
            e.modified.remove(&i);
        }
        EditOrigin::Inserted { before, slot } => {
            if let Some(list) = e.inserted.get_mut(&before) {
                if slot < list.len() {
                    list.remove(slot);
                }
                if list.is_empty() {
                    e.inserted.remove(&before);
                }
            }
        }
    }
}

/// Inserts `t` on `recv` next to the command with origin `at` and selects it.
fn insert(
    l: &Loaded,
    mut e: CaptureEdits,
    at: EditOrigin,
    after: bool,
    recv: ObjId,
    t: &CommandTemplate,
) -> Act {
    let (before, slot) = match at {
        EditOrigin::Original(i) | EditOrigin::Modified(i) if after => (i + 1, 0),
        EditOrigin::Original(i) | EditOrigin::Modified(i) => {
            (i, e.inserted.get(&i).map_or(0, Vec::len))
        }
        EditOrigin::Inserted { before, slot } => (before, slot + after as usize),
    };
    let args = edits::default_args(&l.original, &mut e, t, recv, before);
    let method = e.intern(&l.original, t.method);
    let list = e.inserted.entry(before).or_default();
    let slot = slot.min(list.len());
    list.insert(
        slot,
        Call {
            this: recv,
            method,
            args,
            result: Value::Undefined,
        },
    );
    Act::ApplySelect(e, EditOrigin::Inserted { before, slot })
}

fn is_pass(class: &str) -> bool {
    matches!(class, "GPURenderPassEncoder" | "GPUComputePassEncoder")
}

fn is_gl(class: &str) -> bool {
    matches!(class, "WebGL2RenderingContext" | "WebGLRenderingContext")
}

/// Pass encoders begun and not yet ended in `frame[..end]`, oldest first.
fn open_passes(cap: &Capture, end: usize) -> Vec<ObjId> {
    let mut open = Vec::new();
    for call in &cap.frame[..end.min(cap.frame.len())] {
        match cap.method(call) {
            "beginRenderPass" | "beginComputePass" => open.extend(call.result.as_obj()),
            "end" => open.retain(|&p| p != call.this),
            _ => {}
        }
    }
    open
}

/// Receivers (with their templates) a command can be inserted on before/after edited frame call
/// `sel`: the selected command's receiver when it has templates and is usable there, the innermost
/// open pass encoder at that point, and the GL context used last.
fn insert_targets(l: &Loaded, sel: usize, after: bool) -> Vec<(ObjId, Vec<CommandTemplate>)> {
    let cap = &*l.cap;
    let end = if after { sel + 1 } else { sel };
    let open = open_passes(cap, end);
    let mut out: Vec<(ObjId, Vec<CommandTemplate>)> = Vec::new();
    let mut push = |id: ObjId| {
        if out.iter().any(|(o, _)| *o == id) {
            return;
        }
        let t = edits::templates_for(cap.class_of(id));
        if !t.is_empty() {
            out.push((id, t));
        }
    };
    let this = cap.frame[sel].this;
    if !is_pass(cap.class_of(this)) || open.contains(&this) {
        push(this);
    }
    if let Some(&p) = open.last() {
        push(p);
    }
    let gl = cap.frame[..=sel]
        .iter()
        .rev()
        .chain(cap.setup.iter().rev())
        .map(|c| c.this)
        .find(|&id| is_gl(cap.class_of(id)));
    if let Some(g) = gl {
        push(g);
    }
    out
}

/// `Insert before` / `Insert after` menu; returns the chosen receiver and template.
fn insert_menu(
    ui: &mut egui::Ui,
    l: &Loaded,
    sel: usize,
    after: bool,
) -> Option<(ObjId, CommandTemplate)> {
    let mut pick = None;
    let title = if after {
        "Insert after"
    } else {
        "Insert before"
    };
    ui.menu_button(title, |ui| {
        let targets = insert_targets(l, sel, after);
        if targets.is_empty() {
            ui.weak("No insertable commands at this point");
        }
        let multi = targets.len() > 1;
        for (recv, templates) in &targets {
            if multi {
                ui.label(RichText::new(obj_label(&l.cap, *recv)).strong());
            }
            let mut groups: Vec<&str> = Vec::new();
            for t in templates {
                if !groups.contains(&t.group) {
                    groups.push(t.group);
                }
            }
            for g in groups {
                ui.menu_button(g, |ui| {
                    for t in templates.iter().filter(|t| t.group == g) {
                        if ui.button(t.label).clicked() {
                            pick = Some((*recv, t.clone()));
                            ui.close();
                        }
                    }
                });
            }
            if multi {
                ui.separator();
            }
        }
    });
    pick
}

/// `Edits (N)`: every modified and inserted command in frame order; a click selects it.
fn edit_list(ui: &mut egui::Ui, l: &Loaded, sel: Option<usize>) -> Option<Act> {
    let mut entries: Vec<(usize, u8, usize, EditOrigin)> = l
        .edits
        .inserted
        .iter()
        .flat_map(|(&b, v)| {
            (0..v.len()).map(move |s| (b, 0, s, EditOrigin::Inserted { before: b, slot: s }))
        })
        .chain(
            l.edits
                .modified
                .keys()
                .map(|&i| (i, 1, 0, EditOrigin::Modified(i))),
        )
        .collect();
    entries.sort_by_key(|e| (e.0, e.1, e.2));
    let cur = sel.map(|i| l.origin_of(i));
    let mut act = None;
    egui::CollapsingHeader::new(format!("Edits ({})", entries.len()))
        .id_salt("webgd-command-editor-edits")
        .default_open(true)
        .show(ui, |ui| {
            if entries.is_empty() {
                ui.weak("No edits yet.");
            }
            for &(_, _, _, o) in &entries {
                let Some((color, badge, _)) = origin_style(o) else {
                    continue;
                };
                let index = l.index_of(o);
                let label = index
                    .and_then(|i| l.labels.get(i))
                    .map_or("", String::as_str);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(badge).monospace().strong().color(color));
                    let text = match index {
                        Some(i) => format!("#{i}  {label}"),
                        None => label.to_owned(),
                    };
                    let resp = ui.selectable_label(cur == Some(o), RichText::new(text).monospace());
                    let resp = match o {
                        EditOrigin::Modified(i) => match l.original.frame.get(i) {
                            Some(c) => resp.on_hover_text(format!(
                                "original: {}\nedited: {label}",
                                call_label(&l.original, c)
                            )),
                            None => resp,
                        },
                        _ => resp.on_hover_text("inserted command"),
                    };
                    if resp.clicked() {
                        act = Some(Act::Select(o));
                    }
                });
            }
        });
    act
}
