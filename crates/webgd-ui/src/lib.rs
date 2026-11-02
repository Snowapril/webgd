//! webgd UI: eframe/egui app hosted in the extension page (or standalone).
#![cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

mod app;
mod files;
mod js;
mod panels;
mod port;
mod replay_queue;
mod state;
mod widgets;

#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&info.to_string().into());
    }));
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        use wasm_bindgen::JsCast;
        let canvas = web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .get_element_by_id("webgd_canvas")
            .unwrap()
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .unwrap();
        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
            )
            .await
            .expect("eframe start failed");
    });
}
