//! Capture-side helpers shared by snapshot, inspection and trace code.
use crate::StateNode;
use webgd_core::display::{obj_label, value_label};
use webgd_core::{Call, Capture, ObjId, TypedKind, Value};

/// Numbers of a Typed / Array / scalar value.
pub fn num_list(cap: &Capture, v: &Value) -> Vec<f64> {
    match v {
        Value::Number(_) | Value::Bool(_) => vec![v.as_f64().unwrap_or(0.0)],
        Value::Array(a) => a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect(),
        Value::Typed { kind, blob } => {
            let b = cap.blob(*blob);
            let es = kind.element_size();
            b.chunks_exact(es)
                .map(|c| match kind {
                    TypedKind::Int8 => c[0] as i8 as f64,
                    TypedKind::Uint8 | TypedKind::Uint8Clamped | TypedKind::ArrayBuffer | TypedKind::DataView => c[0] as f64,
                    TypedKind::Int16 => i16::from_le_bytes([c[0], c[1]]) as f64,
                    TypedKind::Uint16 => u16::from_le_bytes([c[0], c[1]]) as f64,
                    TypedKind::Int32 => i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                    TypedKind::Uint32 => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                    TypedKind::Float32 => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                    TypedKind::Float64 => f64::from_le_bytes(c.try_into().unwrap_or([0; 8])),
                    TypedKind::BigInt64 => i64::from_le_bytes(c.try_into().unwrap_or([0; 8])) as f64,
                    TypedKind::BigUint64 => u64::from_le_bytes(c.try_into().unwrap_or([0; 8])) as f64,
                    TypedKind::Float16 => f16_to_f64(u16::from_le_bytes([c[0], c[1]])),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn f16_to_f64(h: u16) -> f64 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f64;
    s * match e {
        0 => m * 2f64.powi(-24),
        31 => {
            if m == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => (1.0 + m / 1024.0) * 2f64.powi(e - 15),
    }
}

pub fn get_num(cap: &Capture, v: &Value, key: &str) -> Option<f64> {
    cap.dict_get(v, key).and_then(Value::as_f64)
}

pub fn get_str<'a>(cap: &'a Capture, v: &Value, key: &str) -> Option<&'a str> {
    match cap.dict_get(v, key) {
        Some(Value::String(s)) => Some(cap.str(*s)),
        _ => None,
    }
}

/// The call that created `id`: a call whose result is `ObjRef(id)`, or the promise-returning call
/// whose token is resolved to `id` by `webgd:resolved`. Searches setup then frame.
pub fn creation_call(cap: &Capture, id: ObjId) -> Option<&Call> {
    for sec in [&cap.setup, &cap.frame] {
        let mut token = None;
        for c in sec.iter() {
            if c.result == Value::ObjRef(id) {
                return Some(c);
            }
            if cap.method(c) == "webgd:resolved" && c.args.get(1) == Some(&Value::ObjRef(id)) {
                token = c.args.first().and_then(Value::as_f64).map(|t| t as u32);
                break;
            }
        }
        if let Some(t) = token {
            for s2 in [&cap.setup, &cap.frame] {
                if let Some(c) = s2.iter().find(|c| c.result == Value::Pending(t)) {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// First argument (descriptor) of the creating call.
pub fn creation_desc(cap: &Capture, id: ObjId) -> Option<&Value> {
    creation_call(cap, id).and_then(|c| c.args.first())
}

/// Generic tree of a descriptor value: Dict → Group, Array → Group with `[i]`, leaves via `value_label`.
pub fn value_tree(cap: &Capture, name: &str, v: &Value) -> StateNode {
    match v {
        Value::Dict(entries) => StateNode::Group {
            name: name.to_string(),
            children: entries.iter().map(|(k, x)| value_tree(cap, cap.str(*k), x)).collect(),
        },
        Value::Array(items) => StateNode::Group {
            name: name.to_string(),
            children: items.iter().enumerate().map(|(i, x)| value_tree(cap, &format!("[{i}]"), x)).collect(),
        },
        Value::String(s) => StateNode::Leaf { name: name.to_string(), value: cap.str(*s).to_string() },
        Value::ObjRef(id) => StateNode::Leaf { name: name.to_string(), value: obj_label(cap, *id) },
        _ => StateNode::Leaf { name: name.to_string(), value: value_label(cap, v) },
    }
}

/// Walks `id`'s parent chain (including `id`) to the first object whose class satisfies `pred`.
pub fn ancestor(cap: &Capture, id: ObjId, pred: impl Fn(&str) -> bool) -> Option<ObjId> {
    let mut cur = Some(id);
    let mut guard = 0;
    while let Some(c) = cur {
        if pred(cap.class_of(c)) {
            return Some(c);
        }
        cur = cap.objects.get(&c).and_then(|o| o.parent);
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    None
}
