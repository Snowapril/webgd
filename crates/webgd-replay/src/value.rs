//! Capture `Value` → live `JsValue`.
use crate::js;
use crate::{ObjMap, ReplayError};
use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;
use webgd_core::{Capture, TypedKind, Value};

pub fn typed_js(kind: TypedKind, bytes: &[u8]) -> Result<JsValue, JsValue> {
    let u8a = js::u8_array(bytes);
    match kind {
        TypedKind::Uint8 => Ok(u8a.into()),
        TypedKind::ArrayBuffer => Ok(u8a.buffer().into()),
        _ => js::construct(kind.js_name(), &[u8a.buffer().into()]),
    }
}

pub fn image_js(width: u32, height: u32, bytes: &[u8]) -> Result<JsValue, JsValue> {
    let data = js::construct("Uint8ClampedArray", &[js::u8_array(bytes).buffer().into()])?;
    js::construct("ImageData", &[data, js::num(width as f64), js::num(height as f64)])
}

pub fn to_js(cap: &Capture, v: &Value, objs: &ObjMap) -> Result<JsValue, ReplayError> {
    let js_err = |e: JsValue| ReplayError::Js { index: usize::MAX, message: js::err_msg(&e) };
    Ok(match v {
        Value::Undefined | Value::Pending(_) => JsValue::UNDEFINED,
        Value::Null => JsValue::NULL,
        Value::Bool(b) => JsValue::from_bool(*b),
        Value::Number(n) => JsValue::from_f64(*n),
        Value::String(s) => JsValue::from_str(cap.str(*s)),
        Value::ObjRef(id) => objs.fwd.get(id).cloned().ok_or(ReplayError::Unmapped(*id))?,
        Value::Typed { kind, blob } => typed_js(*kind, cap.blob(*blob)).map_err(js_err)?,
        Value::Array(items) => {
            let a = Array::new();
            for it in items {
                a.push(&to_js(cap, it, objs)?);
            }
            a.into()
        }
        Value::Dict(entries) => {
            let o = Object::new();
            for (k, it) in entries {
                let _ = Reflect::set(&o, &JsValue::from_str(cap.str(*k)), &to_js(cap, it, objs)?);
            }
            o.into()
        }
        Value::Image { width, height, blob } => image_js(*width, *height, cap.blob(*blob)).map_err(js_err)?,
        Value::Opaque(_) => JsValue::UNDEFINED,
    })
}
