//! Capture tab: live targets, open/save `.webgd`.
use crate::port::CaptureProgress;
use crate::state::AppState;
use egui::{Color32, RichText};

pub fn show(ui: &mut egui::Ui, st: &mut AppState) {
    ui.horizontal(|ui| {
        if ui.button("Open .webgd…").clicked()
            && let Err(e) = crate::files::open_file(st.ctx.clone(), st.inbox.clone())
        {
            st.status = format!("open failed: {e}");
        }
        let bytes = st.loaded.as_ref().map(|l| l.bytes.clone());
        if ui
            .add_enabled(bytes.is_some(), egui::Button::new("Save .webgd"))
            .clicked()
            && let Some(b) = bytes
        {
            st.status = match crate::files::save_file(&b) {
                Ok(name) => format!("saved {name}"),
                Err(e) => format!("save failed: {e}"),
            };
        }
    });
    if !st.status.is_empty() {
        ui.label(&st.status);
    }
    if let Some(l) = &st.loaded {
        if l.source == l.cap.url {
            ui.label(format!("Current capture: {}", l.cap.url));
        } else {
            ui.label(format!("Current capture: {} ({})", l.cap.url, l.source));
        }
    }
    ui.separator();
    if st.port.standalone {
        ui.label(
            RichText::new("standalone mode: live capture unavailable").color(crate::widgets::WARN),
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.heading("Targets");
        if ui
            .small_button("Refresh")
            .on_hover_text("Ask every tab's bridge to reconnect")
            .clicked()
        {
            st.port.refresh();
        }
    });
    let mut capture = None;
    st.port.with_targets(|targets| {
        if targets.is_empty() {
            ui.weak("No targets connected. Open or reload a page using WebGL2/WebGPU.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for t in targets {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let title = if t.title.is_empty() {
                        t.url.as_str()
                    } else {
                        t.title.as_str()
                    };
                    ui.horizontal(|ui| {
                        ui.strong(title);
                        if t.is_frame {
                            ui.weak("(iframe)");
                        }
                        if let Some(id) = t.tab_id {
                            ui.weak(format!("tab {id}"));
                        }
                    });
                    ui.weak(&t.url);
                    ui.horizontal(|ui| {
                        let s = t.status;
                        ui.label(format!("gl2: {}", s.gl2));
                        ui.label(format!("gpuDevices: {}", s.gpu_devices));
                        if s.webgl1 > 0 {
                            ui.label(
                                RichText::new(format!("webgl1: {} (unsupported)", s.webgl1))
                                    .color(crate::widgets::WARN),
                            );
                        }
                    });
                    ui.horizontal(|ui| {
                        let busy = matches!(
                            t.progress,
                            CaptureProgress::Armed | CaptureProgress::Receiving { .. }
                        );
                        let usable = t.status.gl2 + t.status.gpu_devices > 0;
                        let btn = ui.add_enabled(!busy && usable, egui::Button::new("Capture"));
                        if btn.clicked() {
                            capture = Some(t.key);
                        }
                        match &t.progress {
                            CaptureProgress::Idle => {}
                            CaptureProgress::Armed => {
                                ui.spinner();
                                ui.label("waiting for next frame…");
                            }
                            CaptureProgress::Receiving { received, total } => {
                                ui.add(
                                    egui::ProgressBar::new(
                                        *received as f32 / (*total).max(1) as f32,
                                    )
                                    .text(format!("{received} / {total} bytes")),
                                );
                            }
                            CaptureProgress::Failed(m) => {
                                ui.label(RichText::new(m).color(Color32::LIGHT_RED));
                            }
                            CaptureProgress::Done => {
                                ui.label("captured");
                            }
                        }
                    });
                });
            }
        });
    });
    if let Some(key) = capture {
        st.port.request_capture(key);
    }
}
