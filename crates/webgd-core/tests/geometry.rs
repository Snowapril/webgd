// Packed 10-10-10-2 test inputs spell out every field, including zero ones, for readability.
#![allow(clippy::identity_op)]

use std::collections::HashMap;
use webgd_core::geometry::*;

fn gpu(s: &str) -> AttribFormat {
    AttribFormat::Gpu(s.to_string())
}
fn gl(size: u32, ty: u32, normalized: bool, integer: bool) -> AttribFormat {
    AttribFormat::Gl {
        size,
        ty,
        normalized,
        integer,
    }
}
fn close(a: [f64; 4], b: [f64; 4]) {
    for i in 0..4 {
        assert!((a[i] - b[i]).abs() < 1e-9, "{a:?} != {b:?}");
    }
}

#[test]
fn float32x3() {
    let f = gpu("float32x3");
    let mut b = Vec::new();
    for v in [1.0f32, -2.5, 3.0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(f.components(), 3);
    assert_eq!(f.byte_size(), 12);
    assert_eq!(f.kind(), ScalarKind::Float);
    close(f.decode(&b).unwrap(), [1.0, -2.5, 3.0, 1.0]);
    assert_eq!(f.decode(&b[..11]), None);
}

#[test]
fn unorm8x4_and_bgra() {
    close(
        gpu("unorm8x4").decode(&[0, 255, 51, 102]).unwrap(),
        [0.0, 1.0, 0.2, 0.4],
    );
    close(
        gpu("unorm8x4-bgra").decode(&[0, 255, 51, 102]).unwrap(),
        [0.2, 1.0, 0.0, 0.4],
    );
}

#[test]
fn snorm16x2() {
    let b = [0xFF, 0x7F, 0x00, 0x80]; // 32767, -32768
    let f = gpu("snorm16x2");
    assert_eq!(f.byte_size(), 4);
    close(f.decode(&b).unwrap(), [1.0, -1.0, 0.0, 1.0]);
}

#[test]
fn float16x2() {
    let b = [0x00, 0x3C, 0x00, 0xC0]; // 1.0, -2.0
    close(gpu("float16x2").decode(&b).unwrap(), [1.0, -2.0, 0.0, 1.0]);
}

#[test]
fn unorm10_10_10_2() {
    let v: u32 = 1023 | (0 << 10) | (511 << 20) | (3 << 30);
    let d = gpu("unorm10-10-10-2").decode(&v.to_le_bytes()).unwrap();
    close(d, [1.0, 0.0, 511.0 / 1023.0, 1.0]);
}

#[test]
fn integer_gpu_kinds() {
    assert_eq!(gpu("uint8").components(), 1);
    assert_eq!(gpu("sint32x4").kind(), ScalarKind::Sint);
    assert_eq!(gpu("uint16x2").kind(), ScalarKind::Uint);
    close(
        gpu("sint8x2").decode(&[0xFF, 5]).unwrap(),
        [-1.0, 5.0, 0.0, 1.0],
    );
    assert_eq!(gpu("bogus").decode(&[0; 16]), None);
}

#[test]
fn gl_unsigned_byte_variants() {
    let b = [255, 0, 51];
    let n = gl(3, 0x1401, true, false);
    close(n.decode(&b).unwrap(), [1.0, 0.0, 0.2, 1.0]);
    assert_eq!(n.kind(), ScalarKind::Float);
    assert_eq!(n.label(), "UNSIGNED_BYTE×3 norm");
    let u = gl(3, 0x1401, false, false);
    close(u.decode(&b).unwrap(), [255.0, 0.0, 51.0, 1.0]);
    assert_eq!(u.kind(), ScalarKind::Float);
    let i = gl(3, 0x1401, false, true);
    close(i.decode(&b).unwrap(), [255.0, 0.0, 51.0, 1.0]);
    assert_eq!(i.kind(), ScalarKind::Uint);
}

#[test]
fn gl_int_2_10_10_10_rev_normalized() {
    // x = 511, y = -512, z = 0, w = -1 (2-bit)
    let v: u32 = 511 | (0x200 << 10) | (0 << 20) | (0b11 << 30);
    let f = gl(4, 0x8D9F, true, false);
    assert_eq!(f.byte_size(), 4);
    close(f.decode(&v.to_le_bytes()).unwrap(), [1.0, -1.0, 0.0, -1.0]);
}

#[test]
fn indices() {
    let b = [1, 0, 2, 0, 3, 0, 4];
    assert_eq!(decode_indices(&b, IndexFormat::U16, 0, 10), vec![1, 2, 3]);
    assert_eq!(decode_indices(&b, IndexFormat::U16, 2, 2), vec![2, 3]);
    let b32 = [7, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 9];
    assert_eq!(
        decode_indices(&b32, IndexFormat::U32, 0, 3),
        vec![7, u32::MAX]
    );
    assert!(decode_indices(&b32, IndexFormat::U32, 100, 3).is_empty());
}

#[test]
fn vertex_ids() {
    assert_eq!(draw_vertex_ids(None, 5, 3, 0, None), vec![5, 6, 7]);
    let idx = [0, 1, 0xFFFF, 2];
    assert_eq!(
        draw_vertex_ids(Some(&idx), 0, 4, 10, Some(0xFFFF)),
        vec![10, 11, 0xFFFF, 12]
    );
    assert_eq!(draw_vertex_ids(Some(&[5]), 0, 1, -2, None), vec![3]);
}

#[test]
fn fetch_interleaved_instanced_oob() {
    // Slot 0: interleaved pos(float32x2) + color(unorm8x4), stride 12, 2 vertices.
    let mut v = Vec::new();
    for (x, y, c) in [
        (1.0f32, 2.0f32, [255u8, 0, 0, 255]),
        (3.0, 4.0, [0, 255, 0, 255]),
    ] {
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v.extend_from_slice(&c);
    }
    // Slot 1: per-instance float32, divisor 2.
    let mut inst = Vec::new();
    for f in [10.0f32, 20.0] {
        inst.extend_from_slice(&f.to_le_bytes());
    }
    let layout = vec![
        LayoutBuffer {
            slot: 0,
            buffer: None,
            buffer_offset: 0,
            stride: 12,
            per_instance: false,
            divisor: 0,
            attributes: vec![
                LayoutAttrib {
                    location: 0,
                    name: "pos".into(),
                    format: gpu("float32x2"),
                    offset: 0,
                },
                LayoutAttrib {
                    location: 1,
                    name: "col".into(),
                    format: gpu("unorm8x4"),
                    offset: 8,
                },
            ],
        },
        LayoutBuffer {
            slot: 1,
            buffer: None,
            buffer_offset: 0,
            stride: 4,
            per_instance: true,
            divisor: 2,
            attributes: vec![LayoutAttrib {
                location: 2,
                name: "s".into(),
                format: gpu("float32"),
                offset: 0,
            }],
        },
    ];
    let mut bufs = HashMap::new();
    bufs.insert(0, v);
    bufs.insert(1, inst);
    let s = fetch_vertices(&layout, &bufs, &[1, 0, 2], 3);
    assert_eq!(s.len(), 3);
    assert_eq!(s[0].name, "pos");
    assert_eq!(s[0].components, 2);
    assert_eq!(
        s[0].values,
        vec![Some([3.0, 4.0, 0.0, 1.0]), Some([1.0, 2.0, 0.0, 1.0]), None]
    );
    assert_eq!(s[1].values[0], Some([0.0, 1.0, 0.0, 1.0]));
    assert_eq!(s[1].values[2], None);
    // instance 3 / divisor 2 = element 1
    assert_eq!(s[2].values, vec![Some([20.0, 0.0, 0.0, 1.0]); 3]);
    let s = fetch_vertices(&layout, &bufs, &[0], 4);
    assert_eq!(s[2].values, vec![None]);
}

#[test]
fn topology_parse() {
    assert_eq!(
        Topology::from_gpu("triangle-strip"),
        Some(Topology::TriangleStrip)
    );
    assert_eq!(Topology::from_gpu("x"), None);
    assert_eq!(Topology::from_gl(2), Some(Topology::LineLoop));
    assert_eq!(Topology::from_gl(3), Some(Topology::LineStrip));
    assert_eq!(Topology::from_gl(6), Some(Topology::TriangleFan));
    assert_eq!(Topology::from_gl(7), None);
}

#[test]
fn assemble_all() {
    use Primitive::*;
    let o = [0, 1, 2, 3, 4];
    assert_eq!(
        assemble(Topology::PointList, &[0, 1], None),
        vec![Point(0), Point(1)]
    );
    assert_eq!(
        assemble(Topology::LineList, &o, None),
        vec![Line(0, 1), Line(2, 3)]
    );
    assert_eq!(
        assemble(Topology::LineStrip, &[0, 1, 2], None),
        vec![Line(0, 1), Line(1, 2)]
    );
    assert_eq!(
        assemble(Topology::LineLoop, &[0, 1, 2], None),
        vec![Line(0, 1), Line(1, 2), Line(2, 0)]
    );
    assert_eq!(
        assemble(Topology::TriangleList, &o, None),
        vec![Triangle(0, 1, 2)]
    );
    assert_eq!(
        assemble(Topology::TriangleStrip, &o, None),
        vec![Triangle(0, 1, 2), Triangle(2, 1, 3), Triangle(2, 3, 4)]
    );
    assert_eq!(
        assemble(Topology::TriangleFan, &[0, 1, 2, 3], None),
        vec![Triangle(0, 1, 2), Triangle(0, 2, 3)]
    );
    // restart
    let r = 0xFFFF;
    assert_eq!(
        assemble(Topology::TriangleStrip, &[0, 1, 2, 3, r, 4, 5, 6], Some(r)),
        vec![Triangle(0, 1, 2), Triangle(2, 1, 3), Triangle(4, 5, 6)]
    );
    assert_eq!(
        assemble(Topology::LineStrip, &[0, 1, r, 2, 3], Some(r)),
        vec![Line(0, 1), Line(2, 3)]
    );
    assert_eq!(
        assemble(Topology::TriangleFan, &[0, 1, 2, r, 3, 4], Some(r)),
        vec![Triangle(0, 1, 2)]
    );
    assert_eq!(
        assemble(Topology::TriangleList, &[0, 1, r, 2, 3, 4], Some(r)),
        vec![Triangle(2, 3, 4)]
    );
}
