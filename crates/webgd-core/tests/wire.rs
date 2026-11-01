use webgd_core::display::call_label;
use webgd_core::wire::{WireError, decode};
use webgd_core::*;

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/sample.wire");

#[test]
fn decodes_fixture() {
    let cap = decode(FIXTURE).unwrap();
    assert_eq!(cap.url, "http://fixture/");
    assert_eq!(cap.class_of(ObjId(1)), "WebGL2RenderingContext");
    assert_eq!(cap.class_of(ObjId(2)), "WebGLBuffer");
    assert_eq!(cap.objects[&ObjId(2)].parent, Some(ObjId(1)));
    assert_eq!(cap.objects[&ObjId(1)].parent, None);

    assert_eq!(cap.setup.len(), 1);
    assert!(cap.snapshot.is_empty());
    assert_eq!(cap.frame.len(), 2);

    let create = &cap.setup[0];
    assert_eq!(cap.str(create.method), "createBuffer");
    assert_eq!(create.result, Value::ObjRef(ObjId(2)));

    let bd = &cap.frame[0];
    assert_eq!(cap.str(bd.method), "bufferData");
    assert_eq!(bd.args[0], Value::Number(34962.0));
    let Value::Typed { kind: TypedKind::Float32, blob } = bd.args[1] else { panic!("{:?}", bd.args[1]) };
    assert_eq!(cap.blob(blob), &[0, 0, 128, 63, 0, 0, 0, 64, 0, 0, 64, 64]);
    assert_eq!(bd.result, Value::Undefined);

    let mk = &cap.frame[1];
    assert_eq!(cap.str(mk.method), "webgd:pushMarker");
    let a = cap.dict_get(&mk.args[1], "a").unwrap();
    assert_eq!(a, &Value::Array(vec![Value::Bool(true), Value::Null]));

    assert_eq!(call_label(&cap, create), "WebGL2RenderingContext#1.createBuffer()");
    assert_eq!(
        call_label(&cap, bd),
        "WebGL2RenderingContext#1.bufferData(34962, Float32Array[3], 35044)"
    );
    assert_eq!(call_label(&cap, mk), "WebGL2RenderingContext#1.webgd:pushMarker(\"pass\", {…})");
}

#[test]
fn every_prefix_is_truncated() {
    // The 4-byte magic prefix reports BadMagic; everything after must be Truncated.
    for n in 0..FIXTURE.len() {
        match decode(&FIXTURE[..n]) {
            Err(WireError::Truncated { .. }) => {}
            Err(WireError::BadMagic) if n < 4 => {}
            other => panic!("prefix {n}: {other:?}"),
        }
    }
}

#[test]
fn rejects_bad_header_and_dangling_refs() {
    assert_eq!(decode(b"NOPE\x01\0\0\0").unwrap_err(), WireError::BadMagic);
    let mut v = FIXTURE.to_vec();
    v[4] = 2;
    assert_eq!(decode(&v).unwrap_err(), WireError::UnsupportedVersion(2));
    // Header + call referencing undefined string 7.
    let mut b = b"WGDW\x01\0\0\0\0\0\0\0".to_vec();
    b.extend_from_slice(&[0x04, 2, 0, 0, 0, 0, 7, 0, 0, 0]);
    assert!(matches!(decode(&b), Err(WireError::DanglingRef { offset: 18 })));
}
