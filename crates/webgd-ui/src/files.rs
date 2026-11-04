//! `.webgd` open (hidden file input) and save / text export (Blob + anchor download).
use crate::port::{Inbox, Incoming};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// Opens the browser file picker; the chosen file's bytes land in `inbox`.
pub fn open_file(ctx: egui::Context, inbox: Inbox) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let input: web_sys::HtmlInputElement = doc
        .create_element("input")
        .map_err(|_| "create input")?
        .dyn_into()
        .map_err(|_| "input cast")?;
    input.set_type("file");
    input.set_accept(".webgd,application/octet-stream");
    let _ = input.set_attribute("style", "display:none");
    let body = doc.body().ok_or("no body")?;
    let _ = body.append_child(&input);
    let input2 = input.clone();
    let onchange = Closure::once(move || {
        let file = input2.files().and_then(|f| f.get(0));
        input2.remove();
        let Some(file) = file else { return };
        wasm_bindgen_futures::spawn_local(async move {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buf) => {
                    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                    inbox.borrow_mut().push(Incoming {
                        bytes,
                        source: name,
                    });
                }
                Err(e) => log::error!("reading {name} failed: {}", crate::js::error_text(&e)),
            }
            ctx.request_repaint();
        });
    });
    input.set_onchange(Some(onchange.as_ref().unchecked_ref()));
    onchange.forget();
    input.click();
    Ok(())
}

/// Downloads `bytes` as `capture-<unix-ms>.webgd`.
pub fn save_file(bytes: &[u8]) -> Result<String, String> {
    let name = format!("capture-{}.webgd", crate::js::now_ms() as u64);
    download(
        &js_sys::Uint8Array::from(bytes),
        "application/octet-stream",
        &name,
    )?;
    Ok(name)
}

/// Downloads `text` as the Markdown file `name`; returns `name`.
pub fn save_text(name: &str, text: &str) -> Result<String, String> {
    download(
        &wasm_bindgen::JsValue::from_str(text),
        "text/markdown;charset=utf-8",
        name,
    )?;
    Ok(name.to_string())
}

/// Wraps `part` in a Blob of type `mime` and clicks a download anchor named `name`.
fn download(part: &wasm_bindgen::JsValue, mime: &str, name: &str) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let arr = js_sys::Array::of1(part);
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&arr, &opts)
        .map_err(|e| crate::js::error_text(&e))?;
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|e| crate::js::error_text(&e))?;
    let a: web_sys::HtmlAnchorElement = doc
        .create_element("a")
        .map_err(|_| "create anchor")?
        .dyn_into()
        .map_err(|_| "anchor cast")?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}
