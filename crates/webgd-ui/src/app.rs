//! eframe app: menu/tool bar, status bar, one dock layout per workspace, replay scheduling.
use crate::panels::{self, Panels};
use crate::port::CaptureProgress;
use crate::state::{AppState, Tab, Workspace};
use crate::widgets::{self, CHROME_FILL, ERROR, WARN};
use egui::{Align, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use webgd_core::track::commands::CmdKind;

const OPEN_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);

/// Every tab, in the order listed by the Window menu.
const ALL_TABS: [Tab; 9] = [
    Tab::Events,
    Tab::Scrubber,
    Tab::RenderTargets,
    Tab::ApiInspector,
    Tab::Resources,
    Tab::Viewer,
    Tab::Trace,
    Tab::Log,
    Tab::Capture,
];

pub struct App {
    /// Frame Debugger layout.
    frame_dock: DockState<Tab>,
    /// GPU Trace layout.
    trace_dock: DockState<Tab>,
    state: AppState,
    panels: Panels,
    /// Capture summary shown in the tool bar, keyed by capture generation.
    summary: Option<(u64, String)>,
}

/// Frame Debugger: Events/Capture left, Render Targets/Resource Viewer center,
/// API Inspector/Resources right, Scrubber + Log as a thin bottom strip.
fn frame_debugger_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::RenderTargets, Tab::Viewer]);
    let surface = dock.main_surface_mut();
    let [main, bottom] = surface.split_below(NodeIndex::root(), 0.87, vec![Tab::Scrubber]);
    surface.split_right(bottom, 0.72, vec![Tab::Log]);
    let [main, _left] = surface.split_left(main, 0.25, vec![Tab::Events, Tab::Capture]);
    surface.split_right(main, 0.62, vec![Tab::ApiInspector, Tab::Resources]);
    dock
}

/// GPU Trace: timeline center, Events/Log below, API Inspector/Resource Viewer right.
fn gpu_trace_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Trace]);
    let surface = dock.main_surface_mut();
    let [main, _right] = surface.split_right(
        NodeIndex::root(),
        0.70,
        vec![Tab::ApiInspector, Tab::Viewer],
    );
    surface.split_below(main, 0.66, vec![Tab::Events, Tab::Log]);
    dock
}

fn other(ws: Workspace) -> Workspace {
    match ws {
        Workspace::FrameDebugger => Workspace::GpuTrace,
        Workspace::GpuTrace => Workspace::FrameDebugger,
    }
}

/// Char-boundary-safe truncation with an ellipsis.
fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        widgets::apply_theme(&cc.egui_ctx);
        App {
            frame_dock: frame_debugger_layout(),
            trace_dock: gpu_trace_layout(),
            state: AppState::new(cc.egui_ctx.clone()),
            panels: Panels::default(),
            summary: None,
        }
    }

    fn dock_mut(&mut self, ws: Workspace) -> &mut DockState<Tab> {
        match ws {
            Workspace::FrameDebugger => &mut self.frame_dock,
            Workspace::GpuTrace => &mut self.trace_dock,
        }
    }

    /// Activates `st.focus_tab` in the current workspace, or switches to the workspace holding it.
    fn apply_focus(&mut self) {
        let Some(tab) = self.state.focus_tab.take() else {
            return;
        };
        let current = self.state.workspace;
        for ws in [current, other(current)] {
            let dock = self.dock_mut(ws);
            if let Some(path) = dock.find_tab(&tab) {
                let _ = dock.set_active_tab(path);
                self.state.workspace = ws;
                return;
            }
        }
    }

    /// Submits the replay needed by the current selection / viewer (latest wins).
    fn schedule_replay(&mut self) {
        let Some(key) = self.state.replay_key() else {
            return;
        };
        let Some(l) = &self.state.loaded else { return };
        // Frame-start replays have no command to inspect.
        let inspect = if key.stop.is_some() {
            self.state.selected
        } else {
            None
        };
        let view = self.panels.viewer.wanted_read(l, self.state.view);
        let cap = l.cap.clone();
        self.state.replay.ensure(&cap, key, inspect, view);
    }

    fn open_file(&mut self) {
        if let Err(e) = crate::files::open_file(self.state.ctx.clone(), self.state.inbox.clone()) {
            self.state.status = format!("open failed: {e}");
        }
    }

    fn save_file(&mut self) {
        let Some(bytes) = self.state.loaded.as_ref().map(|l| l.bytes.clone()) else {
            return;
        };
        self.state.status = match crate::files::save_file(&bytes) {
            Ok(name) => format!("saved {name}"),
            Err(e) => format!("save failed: {e}"),
        };
    }

    /// Global shortcuts: Ctrl/Cmd+O open, Home/End first/last event (not while typing).
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.text_edit_focused();
        let (open, home, end) = ctx.input_mut(|i| {
            let open = i.consume_shortcut(&OPEN_SHORTCUT);
            if typing {
                (open, false, false)
            } else {
                (
                    open,
                    i.consume_key(Modifiers::NONE, Key::Home),
                    i.consume_key(Modifiers::NONE, Key::End),
                )
            }
        });
        if open {
            self.open_file();
        }
        let n = self.state.loaded.as_ref().map_or(0, |l| l.cap.frame.len());
        if n > 0 {
            if home {
                self.state.select(0);
            } else if end {
                self.state.select(n - 1);
            }
        }
    }

    fn capture_summary(&mut self) -> Option<String> {
        let l = self.state.loaded.as_ref()?;
        if let Some((g, s)) = &self.summary
            && *g == l.generation
        {
            return Some(s.clone());
        }
        let cap = &l.cap;
        let classes: Vec<&str> = cap.objects.keys().map(|id| cap.class_of(*id)).collect();
        let gpu = classes.contains(&"GPUDevice");
        let gl = classes.contains(&"WebGL2RenderingContext");
        let api = match (gl, gpu) {
            (true, true) => "WebGL2 + WebGPU",
            (true, false) => "WebGL2",
            (false, true) => "WebGPU",
            (false, false) => "no context",
        };
        let work = l
            .tree
            .iter()
            .filter(|n| matches!(n.kind, CmdKind::Draw | CmdKind::Dispatch))
            .count();
        let s = format!(
            "{api}  ·  {} events  ·  {work} draws/dispatches  ·  {} resources",
            cap.frame.len(),
            l.resources.len()
        );
        self.summary = Some((l.generation, s.clone()));
        Some(s)
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let summary = self.capture_summary();
        let mut open = false;
        let mut save = false;
        let has_capture = self.state.loaded.is_some();
        let shortcut = ui.ctx().format_shortcut(&OPEN_SHORTCUT);
        let st = &mut self.state;
        egui::Panel::top("webgd_menu_bar")
            .frame(
                egui::Frame::new()
                    .fill(CHROME_FILL)
                    .inner_margin(Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui
                            .add(egui::Button::new("Open .webgd…").shortcut_text(shortcut))
                            .clicked()
                        {
                            open = true;
                            ui.close();
                        }
                        if ui
                            .add_enabled(has_capture, egui::Button::new("Save .webgd"))
                            .clicked()
                        {
                            save = true;
                            ui.close();
                        }
                    });
                    ui.menu_button("Capture", |ui| capture_menu(ui, st));
                    ui.menu_button("Window", |ui| {
                        for tab in ALL_TABS {
                            if ui.button(tab.title()).clicked() {
                                st.focus_tab = Some(tab);
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    ui.selectable_value(
                        &mut st.workspace,
                        Workspace::FrameDebugger,
                        "Frame Debugger",
                    );
                    ui.selectable_value(&mut st.workspace, Workspace::GpuTrace, "GPU Trace");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        match &summary {
                            Some(s) => ui.label(RichText::new(s).small()),
                            None => ui.weak("no capture"),
                        };
                    });
                });
            });
        if open {
            self.open_file();
        }
        if save {
            self.save_file();
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.state;
        egui::Panel::bottom("webgd_status_bar")
            .frame(
                egui::Frame::new()
                    .fill(CHROME_FILL)
                    .inner_margin(Margin::symmetric(8, 2)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let Some(l) = &st.loaded else {
                        ui.weak("No capture");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&st.status).weak()).truncate());
                        });
                        return;
                    };
                    ui.label(RichText::new(short(&l.source, 48)).small())
                        .on_hover_text(&l.cap.url);
                    ui.separator();
                    let n = l.cap.frame.len();
                    match (st.selected, st.selected_setup) {
                        (Some(i), _) => {
                            ui.label(format!("Event {} / {n}", i + 1))
                                .on_hover_text(format!(
                                    "frame call #{i}: {}",
                                    l.labels.get(i).map_or("", String::as_str)
                                ));
                        }
                        (None, Some(k)) => {
                            ui.label(format!("Pre-frame setup call #{k}"));
                        }
                        (None, None) if n > 0 => {
                            ui.label("Frame end");
                        }
                        (None, None) => {
                            ui.label("Frame start");
                        }
                    }
                    ui.separator();
                    let generation = l.generation;
                    if let Some(k) = st.replay.busy() {
                        ui.spinner();
                        ui.label(format!("replaying to {}…", k.label()));
                    }
                    let mut show_log = false;
                    st.replay.results(|r| match &r.replay {
                        Some((k, Ok(info))) if k.generation == generation => {
                            if st.replay.busy().is_none() {
                                ui.weak(format!("replayed to {}", k.label()));
                            }
                            let errs = info.errors.len();
                            if errs > 0 {
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(format!("{errs} replay error(s)"))
                                                .color(ERROR),
                                        )
                                        .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text("Show in Log");
                                show_log = resp.clicked();
                            }
                        }
                        Some((k, Err(e))) if k.generation == generation => {
                            ui.label(
                                RichText::new(format!("replay to {} failed", k.label()))
                                    .color(ERROR),
                            )
                            .on_hover_text(e);
                        }
                        _ => {}
                    });
                    if show_log {
                        st.focus_tab = Some(Tab::Log);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&st.status).weak()).truncate());
                    });
                });
            });
    }
}

/// Live targets with a Capture action each.
fn capture_menu(ui: &mut egui::Ui, st: &mut AppState) {
    if st.port.standalone {
        ui.label(RichText::new("standalone mode: live capture unavailable").color(WARN));
        return;
    }
    if ui.button("Refresh targets").clicked() {
        st.port.refresh();
    }
    if ui.button("Show Capture panel").clicked() {
        st.focus_tab = Some(Tab::Capture);
        ui.close();
    }
    ui.separator();
    let mut capture = None;
    st.port.with_targets(|targets| {
        if targets.is_empty() {
            ui.weak("No targets connected");
        }
        for t in targets {
            let title = if t.title.is_empty() { &t.url } else { &t.title };
            let busy = matches!(
                t.progress,
                CaptureProgress::Armed | CaptureProgress::Receiving { .. }
            );
            let usable = t.status.gl2 + t.status.gpu_devices > 0;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(usable && !busy, egui::Button::new("Capture"))
                    .on_hover_text(&t.url)
                    .clicked()
                {
                    capture = Some(t.key);
                }
                ui.label(short(title, 48));
                ui.weak(format!(
                    "gl2 {} · gpu {}",
                    t.status.gl2, t.status.gpu_devices
                ));
                if busy {
                    ui.spinner();
                }
            });
        }
    });
    if let Some(key) = capture {
        st.port.request_capture(key);
        st.focus_tab = Some(Tab::Capture);
        ui.close();
    }
}

struct Viewer<'a> {
    state: &'a mut AppState,
    panels: &'a mut Panels,
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }
    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        panels::show(*tab, ui, self.state, self.panels);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.state.drain_inbox() {
            self.panels.reset();
        }
        self.handle_keys(&ui.ctx().clone());
        self.menu_bar(ui);
        self.status_bar(ui);
        self.apply_focus();
        self.schedule_replay();
        let ws = self.state.workspace;
        let style = Style::from_egui(ui.style().as_ref());
        let App {
            frame_dock,
            trace_dock,
            state,
            panels,
            ..
        } = self;
        let dock = match ws {
            Workspace::FrameDebugger => frame_dock,
            Workspace::GpuTrace => trace_dock,
        };
        egui::CentralPanel::no_frame().show(ui, |ui| {
            DockArea::new(dock)
                .id(egui::Id::new(("webgd_dock", ws)))
                .style(style)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut Viewer { state, panels });
        });
        // Selection/view/focus changes made by panels this frame: apply them right away.
        self.apply_focus();
        self.schedule_replay();
    }
}
