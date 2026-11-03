//! Vertex/index decoding and primitive assembly for the Geometry viewer (pure, native-testable).
use crate::capture::ObjId;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Topology {
    PointList,
    LineList,
    LineStrip,
    LineLoop,
    TriangleList,
    TriangleStrip,
    TriangleFan,
}

impl Topology {
    /// WebGPU `primitive.topology` string ("point-list", "line-list", "line-strip", "triangle-list", "triangle-strip").
    pub fn from_gpu(s: &str) -> Option<Topology> {
        Some(match s {
            "point-list" => Topology::PointList,
            "line-list" => Topology::LineList,
            "line-strip" => Topology::LineStrip,
            "triangle-list" => Topology::TriangleList,
            "triangle-strip" => Topology::TriangleStrip,
            _ => return None,
        })
    }
    /// GL draw mode enum (POINTS 0 … TRIANGLE_FAN 6).
    pub fn from_gl(mode: u32) -> Option<Topology> {
        Some(match mode {
            0 => Topology::PointList,
            1 => Topology::LineList,
            2 => Topology::LineLoop,
            3 => Topology::LineStrip,
            4 => Topology::TriangleList,
            5 => Topology::TriangleStrip,
            6 => Topology::TriangleFan,
            _ => return None,
        })
    }
}

/// Encoding of one vertex attribute.
#[derive(Clone, PartialEq, Debug)]
pub enum AttribFormat {
    /// WebGPU vertex format string, e.g. "float32x3", "unorm8x4", "sint16x2", "float16x4", "unorm10-10-10-2".
    Gpu(String),
    /// WebGL2 attribute pointer: size 1..=4, type enum (BYTE…FLOAT, HALF_FLOAT, INT_2_10_10_10_REV, UNSIGNED_INT_2_10_10_10_REV),
    /// `normalized`, `integer` (= vertexAttribIPointer).
    Gl {
        size: u32,
        ty: u32,
        normalized: bool,
        integer: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScalarKind {
    Float,
    Sint,
    Uint,
}

/// Scalar storage type of one component.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scalar {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F16,
    F32,
}

impl Scalar {
    fn size(self) -> usize {
        match self {
            Scalar::U8 | Scalar::I8 => 1,
            Scalar::U16 | Scalar::I16 | Scalar::F16 => 2,
            Scalar::U32 | Scalar::I32 | Scalar::F32 => 4,
        }
    }
    fn bits(self) -> u32 {
        self.size() as u32 * 8
    }
    fn signed(self) -> bool {
        matches!(self, Scalar::I8 | Scalar::I16 | Scalar::I32)
    }
    fn read(self, b: &[u8]) -> f64 {
        match self {
            Scalar::U8 => b[0] as f64,
            Scalar::I8 => b[0] as i8 as f64,
            Scalar::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::F16 => f16_to_f64(u16::from_le_bytes([b[0], b[1]])),
            Scalar::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        }
    }
}

/// How raw component values are interpreted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Interp {
    /// Value as-is (floats, GL int→float conversion).
    Float,
    /// Normalized to [0,1] (unsigned) or [-1,1] (signed).
    Norm,
    /// Integer passed to the shader as integer.
    Int,
}

/// Decoded description of a format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Plain {
        scalar: Scalar,
        count: u32,
        interp: Interp,
        bgra: bool,
    },
    /// Packed 10/10/10/2 in a u32, x in the low bits.
    Packed1010102 { signed: bool, normalized: bool },
}

fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f64;
    match exp {
        0 => sign * man * 2f64.powi(-24),
        31 => {
            if man == 0.0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + man / 1024.0) * 2f64.powi(exp - 15),
    }
}

fn normalize(v: f64, bits: u32, signed: bool) -> f64 {
    if signed {
        (v / ((1u64 << (bits - 1)) - 1) as f64).max(-1.0)
    } else {
        v / ((1u64 << bits) - 1) as f64
    }
}

fn sign_extend(v: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v << shift) as i32) >> shift
}

const GL_BYTE: u32 = 0x1400;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_SHORT: u32 = 0x1402;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_INT: u32 = 0x1404;
const GL_UNSIGNED_INT: u32 = 0x1405;
const GL_FLOAT: u32 = 0x1406;
const GL_HALF_FLOAT: u32 = 0x140B;
const GL_INT_2_10_10_10_REV: u32 = 0x8D9F;
const GL_UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;

fn gl_type_name(ty: u32) -> String {
    match ty {
        GL_BYTE => "BYTE".into(),
        GL_UNSIGNED_BYTE => "UNSIGNED_BYTE".into(),
        GL_SHORT => "SHORT".into(),
        GL_UNSIGNED_SHORT => "UNSIGNED_SHORT".into(),
        GL_INT => "INT".into(),
        GL_UNSIGNED_INT => "UNSIGNED_INT".into(),
        GL_FLOAT => "FLOAT".into(),
        GL_HALF_FLOAT => "HALF_FLOAT".into(),
        GL_INT_2_10_10_10_REV => "INT_2_10_10_10_REV".into(),
        GL_UNSIGNED_INT_2_10_10_10_REV => "UNSIGNED_INT_2_10_10_10_REV".into(),
        other => format!("0x{other:04X}"),
    }
}

impl AttribFormat {
    fn layout(&self) -> Option<Layout> {
        match self {
            AttribFormat::Gpu(s) => {
                match s.as_str() {
                    "unorm10-10-10-2" => {
                        return Some(Layout::Packed1010102 {
                            signed: false,
                            normalized: true,
                        });
                    }
                    "unorm8x4-bgra" => {
                        return Some(Layout::Plain {
                            scalar: Scalar::U8,
                            count: 4,
                            interp: Interp::Norm,
                            bgra: true,
                        });
                    }
                    _ => {}
                }
                let (base, count) = match s.split_once('x') {
                    Some((b, n)) => (b, n.parse::<u32>().ok()?),
                    None => (s.as_str(), 1),
                };
                if !(1..=4).contains(&count) {
                    return None;
                }
                let (scalar, interp) = match base {
                    "uint8" => (Scalar::U8, Interp::Int),
                    "sint8" => (Scalar::I8, Interp::Int),
                    "unorm8" => (Scalar::U8, Interp::Norm),
                    "snorm8" => (Scalar::I8, Interp::Norm),
                    "uint16" => (Scalar::U16, Interp::Int),
                    "sint16" => (Scalar::I16, Interp::Int),
                    "unorm16" => (Scalar::U16, Interp::Norm),
                    "snorm16" => (Scalar::I16, Interp::Norm),
                    "float16" => (Scalar::F16, Interp::Float),
                    "float32" => (Scalar::F32, Interp::Float),
                    "uint32" => (Scalar::U32, Interp::Int),
                    "sint32" => (Scalar::I32, Interp::Int),
                    _ => return None,
                };
                Some(Layout::Plain {
                    scalar,
                    count,
                    interp,
                    bgra: false,
                })
            }
            &AttribFormat::Gl {
                size,
                ty,
                normalized,
                integer,
            } => {
                if !(1..=4).contains(&size) {
                    return None;
                }
                match ty {
                    GL_INT_2_10_10_10_REV | GL_UNSIGNED_INT_2_10_10_10_REV => {
                        if size != 4 || integer {
                            return None;
                        }
                        return Some(Layout::Packed1010102 {
                            signed: ty == GL_INT_2_10_10_10_REV,
                            normalized,
                        });
                    }
                    _ => {}
                }
                let scalar = match ty {
                    GL_BYTE => Scalar::I8,
                    GL_UNSIGNED_BYTE => Scalar::U8,
                    GL_SHORT => Scalar::I16,
                    GL_UNSIGNED_SHORT => Scalar::U16,
                    GL_INT => Scalar::I32,
                    GL_UNSIGNED_INT => Scalar::U32,
                    GL_FLOAT => Scalar::F32,
                    GL_HALF_FLOAT => Scalar::F16,
                    _ => return None,
                };
                let is_float = matches!(scalar, Scalar::F32 | Scalar::F16);
                let interp = if integer {
                    if is_float {
                        return None;
                    }
                    Interp::Int
                } else if normalized && !is_float {
                    Interp::Norm
                } else {
                    Interp::Float
                };
                Some(Layout::Plain {
                    scalar,
                    count: size,
                    interp,
                    bgra: false,
                })
            }
        }
    }

    pub fn components(&self) -> u32 {
        match self.layout() {
            Some(Layout::Plain { count, .. }) => count,
            Some(Layout::Packed1010102 { .. }) => 4,
            None => 0,
        }
    }
    pub fn byte_size(&self) -> u32 {
        match self.layout() {
            Some(Layout::Plain { scalar, count, .. }) => scalar.size() as u32 * count,
            Some(Layout::Packed1010102 { .. }) => 4,
            None => 0,
        }
    }
    /// Kind of the value the shader sees (normalized/float formats → Float).
    pub fn kind(&self) -> ScalarKind {
        match self.layout() {
            Some(Layout::Plain {
                scalar,
                interp: Interp::Int,
                ..
            }) => {
                if scalar.signed() {
                    ScalarKind::Sint
                } else {
                    ScalarKind::Uint
                }
            }
            _ => ScalarKind::Float,
        }
    }
    /// Short display label ("float32x3", "UNSIGNED_BYTE×4 norm").
    pub fn label(&self) -> String {
        match self {
            AttribFormat::Gpu(s) => s.clone(),
            AttribFormat::Gl {
                size,
                ty,
                normalized,
                integer,
            } => {
                let mut l = format!("{}×{}", gl_type_name(*ty), size);
                if *integer {
                    l.push_str(" int");
                } else if *normalized {
                    l.push_str(" norm");
                }
                l
            }
        }
    }
    /// Decodes one element from `bytes` (len >= byte_size). Missing components default to (0, 0, 0, 1).
    /// Returns None if `bytes` is too short or the format is unknown.
    pub fn decode(&self, bytes: &[u8]) -> Option<[f64; 4]> {
        let layout = self.layout()?;
        if bytes.len() < self.byte_size() as usize {
            return None;
        }
        let mut out = [0.0, 0.0, 0.0, 1.0];
        match layout {
            Layout::Plain {
                scalar,
                count,
                interp,
                bgra,
            } => {
                let sz = scalar.size();
                for i in 0..count as usize {
                    let raw = scalar.read(&bytes[i * sz..]);
                    out[i] = match interp {
                        Interp::Norm => normalize(raw, scalar.bits(), scalar.signed()),
                        Interp::Float | Interp::Int => raw,
                    };
                }
                if bgra {
                    out.swap(0, 2);
                }
            }
            Layout::Packed1010102 { signed, normalized } => {
                let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let fields = [
                    (v & 0x3ff, 10),
                    ((v >> 10) & 0x3ff, 10),
                    ((v >> 20) & 0x3ff, 10),
                    (v >> 30, 2),
                ];
                for (i, (f, bits)) in fields.into_iter().enumerate() {
                    let raw = if signed {
                        sign_extend(f, bits) as f64
                    } else {
                        f as f64
                    };
                    out[i] = if normalized {
                        normalize(raw, bits, signed)
                    } else {
                        raw
                    };
                }
            }
        }
        Some(out)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct LayoutAttrib {
    /// Shader location (WebGPU `shaderLocation`, GL attribute index).
    pub location: u32,
    /// Attribute name when known (GL `getActiveAttrib`), else "".
    pub name: String,
    pub format: AttribFormat,
    /// Byte offset inside the vertex record.
    pub offset: u64,
}

/// One vertex buffer slot (WebGPU `vertex.buffers[slot]` / GL: one entry per enabled attribute's buffer binding).
#[derive(Clone, PartialEq, Debug)]
pub struct LayoutBuffer {
    pub slot: u32,
    pub buffer: Option<ObjId>,
    /// Byte offset of the binding inside the buffer (setVertexBuffer offset / GL pointer offset base).
    pub buffer_offset: u64,
    pub stride: u64,
    /// Step per instance (WebGPU stepMode "instance" / GL divisor > 0).
    pub per_instance: bool,
    /// GL divisor (1 for WebGPU instance step mode, 0 for per-vertex).
    pub divisor: u32,
    pub attributes: Vec<LayoutAttrib>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IndexFormat {
    U8,
    U16,
    U32,
}

/// Values of one attribute (pre-VS) or one shader output (post-VS) per vertex.
#[derive(Clone, PartialEq, Debug)]
pub struct AttribStream {
    pub name: String,
    pub location: Option<u32>,
    pub slot: Option<u32>,
    pub format: String,
    pub components: u32,
    pub kind: ScalarKind,
    /// One entry per requested vertex id (None = out of buffer bounds / unavailable).
    pub values: Vec<Option<[f64; 4]>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Primitive {
    Point(u32),
    Line(u32, u32),
    Triangle(u32, u32, u32),
}

/// Decodes `count` indices of `format` starting at byte `offset` of `bytes` (truncated at the end of `bytes`).
pub fn decode_indices(bytes: &[u8], format: IndexFormat, offset: u64, count: u32) -> Vec<u32> {
    let size = match format {
        IndexFormat::U8 => 1usize,
        IndexFormat::U16 => 2,
        IndexFormat::U32 => 4,
    };
    let Ok(start) = usize::try_from(offset) else {
        return Vec::new();
    };
    let Some(tail) = bytes.get(start..) else {
        return Vec::new();
    };
    tail.chunks_exact(size)
        .take(count as usize)
        .map(|c| match format {
            IndexFormat::U8 => c[0] as u32,
            IndexFormat::U16 => u16::from_le_bytes([c[0], c[1]]) as u32,
            IndexFormat::U32 => u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
        })
        .collect()
}

/// Vertex ids in submission order: `first..first+count` for non-indexed draws, or `indices[i] + base_vertex`
/// (restart values kept as-is, without base_vertex) for indexed draws.
///
/// For indexed draws `indices` is the already-sliced index list (see [`decode_indices`]); `first`/`count`
/// are ignored. Ids that would leave the u32 range wrap (two's-complement), matching GPU address arithmetic.
pub fn draw_vertex_ids(
    indices: Option<&[u32]>,
    first: u32,
    count: u32,
    base_vertex: i64,
    restart: Option<u32>,
) -> Vec<u32> {
    match indices {
        None => (0..count).map(|i| first.wrapping_add(i)).collect(),
        Some(idx) => idx
            .iter()
            .map(|&i| {
                if Some(i) == restart {
                    i
                } else {
                    (i as i64).wrapping_add(base_vertex) as u32
                }
            })
            .collect(),
    }
}

/// Fetches every attribute of `layout` for each id in `vertex_ids` (per-instance buffers use
/// `instance / divisor`). `buffers` maps slot → full buffer bytes.
///
/// Element address = `buffer_offset + stride * (id | instance / divisor) + attr.offset`.
/// `stride` is used literally (WebGPU semantics: 0 means every element reads the same record); callers
/// building GL layouts must normalize GL stride 0 to the packed attribute size first. A per-instance
/// buffer with divisor 0 steps once per instance. Missing buffers and out-of-bounds reads yield `None`.
pub fn fetch_vertices(
    layout: &[LayoutBuffer],
    buffers: &HashMap<u32, Vec<u8>>,
    vertex_ids: &[u32],
    instance: u32,
) -> Vec<AttribStream> {
    let mut out = Vec::new();
    for lb in layout {
        let data = buffers.get(&lb.slot);
        for attr in &lb.attributes {
            let size = attr.format.byte_size() as u64;
            let values = vertex_ids
                .iter()
                .map(|&id| {
                    let data = data?;
                    let element = if lb.per_instance {
                        (instance / lb.divisor.max(1)) as u64
                    } else {
                        id as u64
                    };
                    let addr = lb
                        .buffer_offset
                        .checked_add(lb.stride.checked_mul(element)?)?
                        .checked_add(attr.offset)?;
                    let end = addr.checked_add(size)?;
                    if size == 0 || end > data.len() as u64 {
                        return None;
                    }
                    attr.format.decode(&data[addr as usize..end as usize])
                })
                .collect();
            out.push(AttribStream {
                name: attr.name.clone(),
                location: Some(attr.location),
                slot: Some(lb.slot),
                format: attr.format.label(),
                components: attr.format.components(),
                kind: attr.format.kind(),
                values,
            });
        }
    }
    out
}

/// Assembles primitives from vertex ids in submission order; `restart` splits strips/fans/loops.
///
/// Restart also splits list topologies (an incomplete trailing primitive of each segment is dropped).
/// Odd triangle-strip triangles swap their first two vertices so all triangles keep the winding of the first.
pub fn assemble(topology: Topology, order: &[u32], restart: Option<u32>) -> Vec<Primitive> {
    let mut out = Vec::new();
    let segments: Vec<&[u32]> = match restart {
        Some(r) => order.split(|&v| v == r).collect(),
        None => vec![order],
    };
    for seg in segments {
        match topology {
            Topology::PointList => out.extend(seg.iter().map(|&v| Primitive::Point(v))),
            Topology::LineList => out.extend(
                seg.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| Primitive::Line(c[0], c[1])),
            ),
            Topology::LineStrip => out.extend(seg.windows(2).map(|w| Primitive::Line(w[0], w[1]))),
            Topology::LineLoop => {
                out.extend(seg.windows(2).map(|w| Primitive::Line(w[0], w[1])));
                if seg.len() >= 2 {
                    out.push(Primitive::Line(seg[seg.len() - 1], seg[0]));
                }
            }
            Topology::TriangleList => out.extend(
                seg.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| Primitive::Triangle(c[0], c[1], c[2])),
            ),
            Topology::TriangleStrip => out.extend(seg.windows(3).enumerate().map(|(i, w)| {
                if i % 2 == 0 {
                    Primitive::Triangle(w[0], w[1], w[2])
                } else {
                    Primitive::Triangle(w[1], w[0], w[2])
                }
            })),
            Topology::TriangleFan => {
                if seg.len() >= 3 {
                    out.extend(
                        seg[1..]
                            .windows(2)
                            .map(|w| Primitive::Triangle(seg[0], w[0], w[1])),
                    );
                }
            }
        }
    }
    out
}
