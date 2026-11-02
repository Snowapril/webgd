//! Log panel: replay errors of the latest replay.
use crate::state::AppState;
use egui::{Color32, RichText};

pub fn show(ui: &mut egui::Ui, st: &mut AppState) {
    let Some(l) = &st.loaded else {
        return super::no_capture(ui);
    };
    let generation = l.generation;
    if let Some(k) = st.replay.busy() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("replaying to #{}…", k.stop));
        });
    }
    let mut select = None;
    st.replay.results(|r| match &r.replay {
        Some((k, res)) if k.generation == generation => match res {
            Err(e) => {
                ui.label(
                    RichText::new(format!("replay to #{} failed: {e}", k.stop))
                        .color(Color32::LIGHT_RED),
                );
            }
            Ok(info) => {
                ui.label(format!(
                    "replay to #{}: {} error(s)",
                    k.stop,
                    info.errors.len()
                ));
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, msg) in &info.errors {
                            ui.horizontal_wrapped(|ui| {
                                if *i == usize::MAX {
                                    ui.label(RichText::new("[setup/snapshot]").monospace());
                                } else if ui
                                    .link(RichText::new(format!("#{i}")).monospace())
                                    .clicked()
                                {
                                    select = Some(*i);
                                }
                                ui.label(
                                    RichText::new(msg).color(Color32::from_rgb(255, 120, 120)),
                                );
                            });
                        }
                    });
            }
        },
        _ => {
            ui.weak("no replay yet");
        }
    });
    if let Some(i) = select {
        st.select(i);
    }
}
