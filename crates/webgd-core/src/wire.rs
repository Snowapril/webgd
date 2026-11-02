//! Decoder for the `hook.js` wire format (see `WireWriter` in extension/hook.js).
use crate::capture::*;

pub const MAGIC: &[u8; 4] = b"WGDW";
pub const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("bad magic: not a webgd capture")]
    BadMagic,
    #[error("unsupported wire version {0}")]
    UnsupportedVersion(u32),
    #[error("truncated at byte {offset}")]
    Truncated { offset: usize },
    #[error("unknown tag {tag:#x} at byte {offset}")]
    UnknownTag { offset: usize, tag: u8 },
    #[error("dangling reference at byte {offset}")]
    DanglingRef { offset: usize },
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len());
        match end {
            Some(end) => {
                let s = &self.b[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            None => Err(WireError::Truncated {
                offset: self.b.len(),
            }),
        }
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, WireError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, WireError> {
        let len = self.u32()? as usize;
        let at = self.pos;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::UnknownTag { offset: at, tag: 0 })
    }
}

struct Decoder<'a> {
    r: Reader<'a>,
    cap: Capture,
    str_ok: Vec<bool>,
    blob_ok: Vec<bool>,
}

impl Decoder<'_> {
    fn sym(&mut self) -> Result<Sym, WireError> {
        let at = self.r.pos;
        let id = self.r.u32()?;
        if self.str_ok.get(id as usize).copied().unwrap_or(false) {
            Ok(Sym(id))
        } else {
            Err(WireError::DanglingRef { offset: at })
        }
    }
    fn blob_ref(&mut self) -> Result<u32, WireError> {
        let at = self.r.pos;
        let id = self.r.u32()?;
        if self.blob_ok.get(id as usize).copied().unwrap_or(false) {
            Ok(id)
        } else {
            Err(WireError::DanglingRef { offset: at })
        }
    }
    /// Object reference; 0 is allowed only when `allow_zero`.
    fn obj(&mut self, allow_zero: bool) -> Result<ObjId, WireError> {
        let at = self.r.pos;
        let id = ObjId(self.r.u32()?);
        if (allow_zero && id.0 == 0) || self.cap.objects.contains_key(&id) {
            Ok(id)
        } else {
            Err(WireError::DanglingRef { offset: at })
        }
    }

    fn value(&mut self) -> Result<Value, WireError> {
        let at = self.r.pos;
        let tag = self.r.u8()?;
        Ok(match tag {
            0 => Value::Undefined,
            1 => Value::Null,
            2 => Value::Bool(false),
            3 => Value::Bool(true),
            4 => Value::Number(self.r.f64()?),
            5 => Value::String(self.sym()?),
            6 => Value::ObjRef(self.obj(false)?),
            7 => {
                let kat = self.r.pos;
                let k = self.r.u8()?;
                let kind = TypedKind::from_u8(k).ok_or(WireError::UnknownTag {
                    offset: kat,
                    tag: k,
                })?;
                Value::Typed {
                    kind,
                    blob: self.blob_ref()?,
                }
            }
            8 => {
                let n = self.r.u32()?;
                let mut v = Vec::with_capacity((n as usize).min(1 << 16));
                for _ in 0..n {
                    v.push(self.value()?);
                }
                Value::Array(v)
            }
            9 => {
                let n = self.r.u32()?;
                let mut v = Vec::with_capacity((n as usize).min(1 << 16));
                for _ in 0..n {
                    let k = self.sym()?;
                    v.push((k, self.value()?));
                }
                Value::Dict(v)
            }
            10 => {
                let width = self.r.u32()?;
                let height = self.r.u32()?;
                Value::Image {
                    width,
                    height,
                    blob: self.blob_ref()?,
                }
            }
            11 => Value::Pending(self.r.u32()?),
            12 => Value::Opaque(self.sym()?),
            tag => return Err(WireError::UnknownTag { offset: at, tag }),
        })
    }

    fn run(mut self) -> Result<Capture, WireError> {
        if self.r.take(4)? != MAGIC {
            return Err(WireError::BadMagic);
        }
        let v = self.r.u32()?;
        if v != VERSION {
            return Err(WireError::UnsupportedVersion(v));
        }
        self.cap.url = self.r.string()?;
        loop {
            let at = self.r.pos;
            match self.r.u8()? {
                0x01 => {
                    let id = self.r.u32()? as usize;
                    let s = self.r.string()?;
                    if self.cap.strings.len() <= id {
                        self.cap.strings.resize(id + 1, String::new());
                        self.str_ok.resize(id + 1, false);
                    }
                    self.cap.strings[id] = s;
                    self.str_ok[id] = true;
                }
                0x02 => {
                    let id = ObjId(self.r.u32()?);
                    let class = self.sym()?;
                    let parent = self.obj(true)?;
                    let parent = (parent.0 != 0).then_some(parent);
                    self.cap
                        .objects
                        .insert(id, ObjectInfo { id, class, parent });
                }
                0x03 => {
                    let id = self.r.u32()? as usize;
                    let len = self.r.u32()? as usize;
                    let data = self.r.take(len)?.to_vec();
                    if self.cap.blobs.len() <= id {
                        self.cap.blobs.resize(id + 1, Vec::new());
                        self.blob_ok.resize(id + 1, false);
                    }
                    self.cap.blobs[id] = data;
                    self.blob_ok[id] = true;
                }
                0x04 => {
                    let sat = self.r.pos;
                    let section = self.r.u8()?;
                    let this = self.obj(true)?;
                    let method = self.sym()?;
                    let argc = self.r.u32()?;
                    let mut args = Vec::with_capacity((argc as usize).min(64));
                    for _ in 0..argc {
                        args.push(self.value()?);
                    }
                    let result = self.value()?;
                    let call = Call {
                        this,
                        method,
                        args,
                        result,
                    };
                    match section {
                        0 => self.cap.setup.push(call),
                        1 => self.cap.snapshot.push(call),
                        2 => self.cap.frame.push(call),
                        tag => return Err(WireError::UnknownTag { offset: sat, tag }),
                    }
                }
                0xFF => return Ok(self.cap),
                tag => return Err(WireError::UnknownTag { offset: at, tag }),
            }
        }
    }
}

pub fn decode(bytes: &[u8]) -> Result<Capture, WireError> {
    Decoder {
        r: Reader { b: bytes, pos: 0 },
        cap: Capture::default(),
        str_ok: Vec::new(),
        blob_ok: Vec::new(),
    }
    .run()
}
