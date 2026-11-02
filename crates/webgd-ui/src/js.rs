//! Small `js_sys::Reflect` helpers for untyped extension APIs.
use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};

pub fn get(obj: &JsValue, key: &str) -> JsValue {
    if obj.is_undefined() || obj.is_null() {
        return JsValue::UNDEFINED;
    }
    Reflect::get(obj, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

pub fn get_path(obj: &JsValue, path: &[&str]) -> JsValue {
    path.iter().fold(obj.clone(), |o, k| get(&o, k))
}

pub fn call(obj: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f: Function = get(obj, name)
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("{name} is not a function")))?;
    f.apply(obj, &args.iter().collect::<Array>())
}

pub fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in entries {
        let _ = Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

pub fn get_str(obj: &JsValue, key: &str) -> Option<String> {
    get(obj, key).as_string()
}

pub fn get_f64(obj: &JsValue, key: &str) -> Option<f64> {
    get(obj, key).as_f64()
}

pub fn error_text(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(m) = get_str(e, "message") {
        return m;
    }
    format!("{e:?}")
}

pub fn now_ms() -> f64 {
    js_sys::Date::now()
}
