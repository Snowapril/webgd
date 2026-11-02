//! Applies `webgd:gpuBuffer` / `webgd:gpuTexture` snapshot records.
use super::buf_usage;
use crate::engine::{Engine, jserr};
use crate::js;
use wasm_bindgen::JsValue;
use webgd_core::{Call, Value};

fn note_text(e: &Engine, v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => e.cap.str(*s).to_string(),
        _ => "not captured".into(),
    }
}

pub fn apply(e: &mut Engine, call: &Call, method: &str) -> Result<(), String> {
    let obj = e.obj(call.this)?;
    let device = e.device_of(call.this).ok_or("no device")?;
    let queue = js::get(&device, "queue");
    let data = call.args.first().unwrap_or(&Value::Null);
    if data.is_nullish() {
        let note = format!(
            "pre-frame contents not captured ({})",
            note_text(e, call.args.get(1))
        );
        e.st.notes.entry(call.this).or_default().push(note);
        return Ok(());
    }
    if method == "webgd:gpuBuffer" {
        let bytes: JsValue = e.arg(data)?;
        let len = js::f(&js::get(&bytes, "byteLength")) as u32;
        if js::get(&obj, "mapState").as_string().as_deref() == Some("mapped") {
            // Still mapped from setup mappedAtCreation: write through the mapping, then unmap.
            let ab = js::call(&obj, "getMappedRange", &[]).map_err(jserr)?;
            js::call(
                &js::construct("Uint8Array", &[ab]).map_err(jserr)?,
                "set",
                &[bytes],
            )
            .map_err(jserr)?;
            js::call(&obj, "unmap", &[]).map_err(jserr)?;
            return Ok(());
        }
        let usage = js::u(&js::get(&obj, "usage"));
        if usage & buf_usage::COPY_DST == 0 {
            e.st.notes
                .entry(call.this)
                .or_default()
                .push("pre-frame contents not restored (mappable buffer)".into());
            return Ok(());
        }
        let n = len & !3;
        if n > 0 {
            js::call(
                &queue,
                "writeBuffer",
                &[obj, js::num(0.0), bytes, js::num(0.0), js::num(n as f64)],
            )
            .map_err(jserr)?;
        }
        return Ok(());
    }
    // webgd:gpuTexture
    let Value::Array(subs) = data else {
        return Err("bad gpuTexture record".into());
    };
    for sub in subs {
        let g = |k: &str| {
            e.cap
                .dict_get(sub, k)
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        };
        let Some(bytes) = e.cap.dict_get(sub, "data") else {
            continue;
        };
        if bytes.is_nullish() {
            continue;
        }
        let bytes = e.arg(bytes)?;
        let depth = g("depthOrLayers").max(1.0);
        js::call(
            &queue,
            "writeTexture",
            &[
                js::obj(&[
                    ("texture", obj.clone()),
                    ("mipLevel", js::num(g("mip"))),
                    (
                        "origin",
                        js::obj(&[
                            ("x", js::num(0.0)),
                            ("y", js::num(0.0)),
                            ("z", js::num(g("layer"))),
                        ]),
                    ),
                ]),
                bytes,
                js::obj(&[
                    ("offset", js::num(0.0)),
                    ("bytesPerRow", js::num(g("bytesPerRow"))),
                    ("rowsPerImage", js::num(g("rowsPerImage"))),
                ]),
                js::obj(&[
                    ("width", js::num(g("width"))),
                    ("height", js::num(g("height"))),
                    ("depthOrArrayLayers", js::num(depth)),
                ]),
            ],
        )
        .map_err(jserr)?;
    }
    Ok(())
}
