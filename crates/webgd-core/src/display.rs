//! Human-readable labels for calls and values.
use crate::capture::*;

pub fn fmt_number(n: f64) -> String {
    format!("{n}")
}

pub fn obj_label(cap: &Capture, id: ObjId) -> String {
    format!("{}#{}", cap.class_of(id), id.0)
}

pub fn value_label(cap: &Capture, v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => fmt_number(*n),
        Value::String(s) => {
            let s = cap.str(*s);
            if s.chars().count() > 40 {
                format!("\"{}…\"", s.chars().take(40).collect::<String>())
            } else {
                format!("\"{s}\"")
            }
        }
        Value::ObjRef(id) => obj_label(cap, *id),
        Value::Typed { kind, blob } => {
            format!("{}[{}]", kind.js_name(), cap.blob(*blob).len() / kind.element_size())
        }
        Value::Array(a) => format!("[{}]", a.len()),
        Value::Dict(_) => "{…}".into(),
        Value::Image { width, height, .. } => format!("Image {width}×{height}"),
        Value::Pending(_) => "Promise".into(),
        Value::Opaque(c) => cap.str(*c).to_string(),
    }
}

/// `Class#this.method(args)`.
pub fn call_label(cap: &Capture, call: &Call) -> String {
    let args: Vec<String> = call.args.iter().map(|a| value_label(cap, a)).collect();
    format!("{}.{}({})", obj_label(cap, call.this), cap.str(call.method), args.join(", "))
}
