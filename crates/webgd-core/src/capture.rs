//! In-memory capture model decoded from the wire format.
use std::collections::BTreeMap;

/// Object id; `ObjId(0)` (the default) means "no object".
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct ObjId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Sym(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TypedKind {
    ArrayBuffer,
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
    DataView,
    BigInt64,
    BigUint64,
    Float16,
}

impl TypedKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        use TypedKind::*;
        Some(match v {
            0 => ArrayBuffer,
            1 => Int8,
            2 => Uint8,
            3 => Uint8Clamped,
            4 => Int16,
            5 => Uint16,
            6 => Int32,
            7 => Uint32,
            8 => Float32,
            9 => Float64,
            10 => DataView,
            11 => BigInt64,
            12 => BigUint64,
            13 => Float16,
            _ => return None,
        })
    }

    /// Bytes per element (1 for ArrayBuffer/DataView).
    pub fn element_size(self) -> usize {
        use TypedKind::*;
        match self {
            ArrayBuffer | Int8 | Uint8 | Uint8Clamped | DataView => 1,
            Int16 | Uint16 | Float16 => 2,
            Int32 | Uint32 | Float32 => 4,
            Float64 | BigInt64 | BigUint64 => 8,
        }
    }

    /// JS global constructor name.
    pub fn js_name(self) -> &'static str {
        use TypedKind::*;
        match self {
            ArrayBuffer => "ArrayBuffer",
            Int8 => "Int8Array",
            Uint8 => "Uint8Array",
            Uint8Clamped => "Uint8ClampedArray",
            Int16 => "Int16Array",
            Uint16 => "Uint16Array",
            Int32 => "Int32Array",
            Uint32 => "Uint32Array",
            Float32 => "Float32Array",
            Float64 => "Float64Array",
            DataView => "DataView",
            BigInt64 => "BigInt64Array",
            BigUint64 => "BigUint64Array",
            Float16 => "Float16Array",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Sym),
    ObjRef(ObjId),
    Typed { kind: TypedKind, blob: u32 },
    Array(Vec<Value>),
    Dict(Vec<(Sym, Value)>),
    Image { width: u32, height: u32, blob: u32 },
    Pending(u32),
    Opaque(Sym),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<ObjId> {
        match self {
            Value::ObjRef(id) => Some(*id),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Null | Value::Undefined)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Section {
    Setup,
    Snapshot,
    Frame,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Call {
    pub this: ObjId,
    pub method: Sym,
    pub args: Vec<Value>,
    pub result: Value,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ObjectInfo {
    pub id: ObjId,
    pub class: Sym,
    pub parent: Option<ObjId>,
}

#[derive(Clone, Default, Debug)]
pub struct Capture {
    pub url: String,
    pub strings: Vec<String>,
    pub objects: BTreeMap<ObjId, ObjectInfo>,
    pub blobs: Vec<Vec<u8>>,
    pub setup: Vec<Call>,
    pub snapshot: Vec<Call>,
    pub frame: Vec<Call>,
}

impl Capture {
    pub fn str(&self, s: Sym) -> &str {
        self.strings
            .get(s.0 as usize)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Class name of an object; "" for unknown ids.
    pub fn class_of(&self, id: ObjId) -> &str {
        self.objects
            .get(&id)
            .map(|o| self.str(o.class))
            .unwrap_or("")
    }

    pub fn blob(&self, id: u32) -> &[u8] {
        self.blobs
            .get(id as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn dict_get<'a>(&self, v: &'a Value, key: &str) -> Option<&'a Value> {
        match v {
            Value::Dict(entries) => entries
                .iter()
                .find(|(k, _)| self.str(*k) == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// Looks up an interned string; `None` if never interned.
    pub fn sym(&self, s: &str) -> Option<Sym> {
        self.strings
            .iter()
            .position(|x| x == s)
            .map(|i| Sym(i as u32))
    }

    pub fn method(&self, call: &Call) -> &str {
        self.str(call.method)
    }

    pub fn section(&self, s: Section) -> &[Call] {
        match s {
            Section::Setup => &self.setup,
            Section::Snapshot => &self.snapshot,
            Section::Frame => &self.frame,
        }
    }
}
