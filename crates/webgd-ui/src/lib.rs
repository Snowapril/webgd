#![cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

struct App {
    tree: egui_dock::DockState<String>,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self { tree: egui_dock::DockState::new(vec!["Capture".to_owned()]) }
    }
}

struct Viewer;
impl egui_dock::TabViewer for Viewer {
    type Tab = String;
    fn id(&mut self, tab: &mut String) -> egui::Id {
        egui::Id::new(tab.as_str())
    }
    fn title(&mut self, tab: &mut String) -> egui::WidgetText {
        tab.as_str().into()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut String) {
        ui.label(tab.as_str());
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui_dock::DockArea::new(&mut self.tree).show_inside(ui, &mut Viewer);
    }
}

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
            .start(canvas, eframe::WebOptions::default(), Box::new(|cc| Ok(Box::new(App::new(cc)))))
            .await
            .expect("eframe start failed");
    });
}
