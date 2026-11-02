//! Untyped JS access helpers. All WebGL2/WebGPU access in this crate goes through these.
use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

pub fn global() -> JsValue {
    js_sys::global().into()
}

pub fn get(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

pub fn get_idx(obj: &JsValue, i: u32) -> JsValue {
    Reflect::get_u32(obj, i).unwrap_or(JsValue::UNDEFINED)
}

pub fn set(obj: &JsValue, key: &str, v: &JsValue) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), v);
}

/// `obj[name](...args)`; a missing method is reported as an error.
pub fn call(obj: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f = get(obj, name);
    let f: Function = f
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("{name} is not a function")))?;
    let arr: Array = args.iter().collect();
    f.apply(obj, &arr)
}

/// `new globalThis[name](...args)`.
pub fn construct(name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let ctor: Function = get(&global(), name)
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("{name} is not a constructor")))?;
    let arr: Array = args.iter().collect();
    Reflect::construct(&ctor, &arr)}

pub fn obj(entries: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in entries {
        let _ = Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o.into()
}

pub fn arr(items: &[JsValue]) -> JsValue {
    items.iter().collect::<Array>().into()
}

pub fn num(v: f64) -> JsValue {
    JsValue::from_f64(v)
}

pub fn as_f64(v: &JsValue) -> Option<f64> {
    if let Some(b) = v.as_bool() {
        return Some(b as u8 as f64);
    }
    v.as_f64()
}

pub fn f(v: &JsValue) -> f64 {
    as_f64(v).unwrap_or(0.0)
}

pub fn u(v: &JsValue) -> u32 {
    as_f64(v).unwrap_or(0.0) as u32
}

pub fn truthy(v: &JsValue) -> bool {
    v.is_truthy()
}

pub fn is_obj(v: &JsValue) -> bool {
    v.is_object() || v.is_function()
}

/// Human-readable message of a thrown JS value.
pub fn err_msg(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    let name = get(e, "name").as_string();
    let msg = get(e, "message").as_string();
    match (name, msg) {
        (Some(n), Some(m)) => format!("{n}: {m}"),
        (None, Some(m)) => m,
        _ => js_sys::JSON::stringify(e)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_else(|| format!("{e:?}")),
    }
}

/// Shallow copy of a JS object (own enumerable properties).
pub fn clone_obj(v: &JsValue) -> JsValue {
    let o = Object::new();
    if is_obj(v) {
        Object::assign(&o, v.unchecked_ref());
    }
    o.into()
}

pub fn array_items(v: &JsValue) -> Vec<JsValue> {
    if Array::is_array(v) {
        Array::from(v).iter().collect()
    } else {
        Vec::new()
    }
}

/// Promise that resolves to `undefined` after `ms`.
pub fn timeout_promise(ms: f64) -> Promise {
    Promise::new(&mut |resolve, _| {
        if let Ok(st) = get(&global(), "setTimeout").dyn_into::<Function>() {
            let _ = st.call2(&JsValue::NULL, &resolve, &JsValue::from_f64(ms));
        }
    })
}

pub async fn sleep(ms: f64) {
    let _ = JsFuture::from(timeout_promise(ms)).await;
}

pub enum AwaitError {
    Timeout,
    Rejected(JsValue),
}

/// Awaits `p` (any thenable or value) with a timeout.
pub async fn await_promise(p: &JsValue, timeout_ms: f64) -> Result<JsValue, AwaitError> {
    let sentinel: JsValue = Object::new().into();
    let sentinel2 = sentinel.clone();
    let timer = Promise::new(&mut |resolve, _| {
        if let Ok(st) = get(&global(), "setTimeout").dyn_into::<Function>() {
            let cb = Closure::once_into_js({
                let resolve = resolve.clone();
                let s = sentinel2.clone();
                move || {
                    let _ = resolve.call1(&JsValue::NULL, &s);
                }
            });
            let _ = st.call2(&JsValue::NULL, &cb, &JsValue::from_f64(timeout_ms));
        }
    });
    let race = Promise::race(&arr(&[p.clone(), timer.into()]));
    match JsFuture::from(race).await {
        Ok(v) if v == sentinel => Err(AwaitError::Timeout),
        Ok(v) => Ok(v),
        Err(e) => Err(AwaitError::Rejected(e)),
    }
}

pub fn u8_array(bytes: &[u8]) -> js_sys::Uint8Array {
    js_sys::Uint8Array::from(bytes)
}

/// Copies any ArrayBuffer / ArrayBufferView into a Vec.
pub fn bytes_of(v: &JsValue) -> Vec<u8> {
    if v.is_instance_of::<js_sys::ArrayBuffer>() {
        return js_sys::Uint8Array::new(v).to_vec();
    }
    let buf = get(v, "buffer");
    if buf.is_undefined() {
        return Vec::new();
    }
    let off = f(&get(v, "byteOffset"));
    let len = f(&get(v, "byteLength"));
    js_sys::Uint8Array::new_with_byte_offset_and_length(&buf, off as u32, len as u32).to_vec()
}
