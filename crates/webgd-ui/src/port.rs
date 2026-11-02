//! Extension port client: accepts `webgd-target` ports from bridge.js, tracks live
//! targets and reassembles base64-chunked captures.
use crate::js::{call, error_text, get, get_f64, get_path, get_str, object};
use base64::Engine;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// Raw capture bytes delivered by a live target or a file.
pub struct Incoming {
    pub bytes: Vec<u8>,
    pub source: String,
}

pub type Inbox = Rc<RefCell<Vec<Incoming>>>;

#[derive(Default, Clone, Copy)]
pub struct Status {
    pub gl2: u32,
    pub webgl1: u32,
    pub gpu_devices: u32,
}

struct Assembly {
    total: usize,
    next_index: u32,
    bytes: Vec<u8>,
}

pub enum CaptureProgress {
    Idle,
    Armed,
    Receiving { received: usize, total: usize },
    Failed(String),
    Done,
}

pub struct Target {
    pub key: u32,
    port: JsValue,
    pub url: String,
    pub title: String,
    pub is_frame: bool,
    pub tab_id: Option<f64>,
    pub status: Status,
    pub progress: CaptureProgress,
    assembly: Option<Assembly>,
}

#[derive(Default)]
struct Inner {
    targets: Vec<Target>,
    next_key: u32,
}

pub struct PortClient {
    /// `chrome.runtime.id` absent: running outside the extension.
    pub standalone: bool,
    inner: Rc<RefCell<Inner>>,
}

fn chrome() -> JsValue {
    get(&js_sys::global(), "chrome")
}

fn read_status(v: &JsValue) -> Status {
    let n = |k| get_f64(v, k).unwrap_or(0.0) as u32;
    Status {
        gl2: n("gl2"),
        webgl1: n("webgl1"),
        gpu_devices: n("gpuDevices"),
    }
}

impl PortClient {
    pub fn new(ctx: egui::Context, inbox: Inbox) -> Self {
        let standalone = get_path(&chrome(), &["runtime", "id"])
            .as_string()
            .is_none();
        let client = PortClient {
            standalone,
            inner: Rc::default(),
        };
        if !standalone {
            client.listen(ctx, inbox);
            broadcast_hello();
        }
        client
    }

    fn listen(&self, ctx: egui::Context, inbox: Inbox) {
        let inner = self.inner.clone();
        let on_connect = Closure::<dyn FnMut(JsValue)>::new(move |port: JsValue| {
            if get_str(&port, "name").as_deref() != Some("webgd-target") {
                return;
            }
            let key = {
                let mut i = inner.borrow_mut();
                i.next_key += 1;
                let key = i.next_key;
                let tab_id = get_path(&port, &["sender", "tab", "id"]).as_f64();
                i.targets.push(Target {
                    key,
                    port: port.clone(),
                    url: get_path(&port, &["sender", "url"])
                        .as_string()
                        .unwrap_or_default(),
                    title: String::new(),
                    is_frame: get_path(&port, &["sender", "frameId"])
                        .as_f64()
                        .is_some_and(|f| f != 0.0),
                    tab_id,
                    status: Status::default(),
                    progress: CaptureProgress::Idle,
                    assembly: None,
                });
                key
            };
            let (inner_m, ctx_m, inbox_m) = (inner.clone(), ctx.clone(), inbox.clone());
            let on_msg = Closure::<dyn FnMut(JsValue)>::new(move |msg: JsValue| {
                handle_message(&inner_m, &inbox_m, key, &msg);
                ctx_m.request_repaint();
            });
            let _ = call(
                &get(&port, "onMessage"),
                "addListener",
                &[on_msg.as_ref().clone()],
            );
            on_msg.forget();
            let (inner_d, ctx_d) = (inner.clone(), ctx.clone());
            let on_disc = Closure::<dyn FnMut(JsValue)>::new(move |_p: JsValue| {
                inner_d.borrow_mut().targets.retain(|t| t.key != key);
                ctx_d.request_repaint();
            });
            let _ = call(
                &get(&port, "onDisconnect"),
                "addListener",
                &[on_disc.as_ref().clone()],
            );
            on_disc.forget();
            ctx.request_repaint();
        });
        let _ = call(
            &get_path(&chrome(), &["runtime", "onConnect"]),
            "addListener",
            &[on_connect.as_ref().clone()],
        );
        on_connect.forget();
    }

    /// Re-sends `webgd-ui-hello` to every tab so disconnected bridges reconnect.
    pub fn refresh(&self) {
        if !self.standalone {
            broadcast_hello();
        }
    }

    pub fn with_targets<R>(&self, f: impl FnOnce(&[Target]) -> R) -> R {
        f(&self.inner.borrow().targets)
    }

    pub fn request_capture(&self, key: u32) {
        let mut inner = self.inner.borrow_mut();
        let Some(t) = inner.targets.iter_mut().find(|t| t.key == key) else {
            return;
        };
        match call(
            &t.port,
            "postMessage",
            &[object(&[("type", "capture".into())])],
        ) {
            Ok(_) => t.progress = CaptureProgress::Armed,
            Err(e) => t.progress = CaptureProgress::Failed(error_text(&e)),
        }
    }
}

fn handle_message(inner: &Rc<RefCell<Inner>>, inbox: &Inbox, key: u32, msg: &JsValue) {
    let mut inner = inner.borrow_mut();
    let Some(t) = inner.targets.iter_mut().find(|t| t.key == key) else {
        return;
    };
    match get_str(msg, "type").as_deref() {
        Some("hello") => {
            if let Some(u) = get_str(msg, "url") {
                t.url = u;
            }
            t.title = get_str(msg, "title").unwrap_or_default();
            t.is_frame = get(msg, "isFrame").as_bool().unwrap_or(t.is_frame);
            let st = get(msg, "status");
            t.status = read_status(if st.is_object() { &st } else { msg });
        }
        Some("status") => {
            let st = get(msg, "status");
            t.status = read_status(if st.is_object() { &st } else { msg });
        }
        Some("captureBegin") => {
            let total = get_f64(msg, "total").unwrap_or(0.0) as usize;
            t.assembly = Some(Assembly {
                total,
                next_index: 0,
                bytes: Vec::with_capacity(total),
            });
            t.progress = CaptureProgress::Receiving { received: 0, total };
        }
        Some("chunk") => {
            let Some(a) = t.assembly.as_mut() else {
                t.progress = CaptureProgress::Failed("chunk without captureBegin".into());
                return;
            };
            let index = get_f64(msg, "index").unwrap_or(-1.0);
            if index != a.next_index as f64 {
                t.progress = CaptureProgress::Failed(format!(
                    "chunk {index} out of order (expected {})",
                    a.next_index
                ));
                t.assembly = None;
                return;
            }
            a.next_index += 1;
            let b64 = get_str(msg, "b64").unwrap_or_default();
            if let Err(e) =
                base64::engine::general_purpose::STANDARD.decode_vec(b64.as_bytes(), &mut a.bytes)
            {
                t.progress = CaptureProgress::Failed(format!("chunk {index}: bad base64: {e}"));
                t.assembly = None;
                return;
            }
            t.progress = CaptureProgress::Receiving {
                received: a.bytes.len(),
                total: a.total,
            };
        }
        Some("captureEnd") => {
            let Some(a) = t.assembly.take() else {
                t.progress = CaptureProgress::Failed("captureEnd without captureBegin".into());
                return;
            };
            if a.bytes.len() != a.total {
                t.progress = CaptureProgress::Failed(format!(
                    "capture length {} != announced {}",
                    a.bytes.len(),
                    a.total
                ));
                return;
            }
            t.progress = CaptureProgress::Done;
            let source = if t.url.is_empty() {
                "live capture".to_owned()
            } else {
                t.url.clone()
            };
            inbox.borrow_mut().push(Incoming {
                bytes: a.bytes,
                source,
            });
        }
        Some("captureError") => {
            t.assembly = None;
            t.progress = CaptureProgress::Failed(
                get_str(msg, "message").unwrap_or_else(|| "capture failed".into()),
            );
        }
        _ => {}
    }
}

/// `chrome.tabs.query({})` then `chrome.tabs.sendMessage(id, {type:'webgd-ui-hello'})` per tab; errors ignored.
fn broadcast_hello() {
    wasm_bindgen_futures::spawn_local(async {
        let tabs_api = get(&chrome(), "tabs");
        let Ok(p) = call(&tabs_api, "query", &[object(&[])]) else {
            return;
        };
        let Ok(tabs) = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(p)).await else {
            return;
        };
        for tab in js_sys::Array::from(&tabs).iter() {
            let Some(id) = get_f64(&tab, "id") else {
                continue;
            };
            let msg = object(&[("type", "webgd-ui-hello".into())]);
            if let Ok(p) = call(&tabs_api, "sendMessage", &[JsValue::from_f64(id), msg])
                && p.is_instance_of::<js_sys::Promise>()
            {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(p)).await;
                });
            }
        }
    });
}
