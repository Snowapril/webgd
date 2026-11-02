use webgd_core::track::revisions::{Access, Revision, all, revision_at};
use webgd_core::*;

/// Hand-built capture helper.
#[derive(Default)]
struct B {
    cap: Capture,
}

impl B {
    fn sym(&mut self, s: &str) -> Sym {
        if let Some(i) = self.cap.strings.iter().position(|x| x == s) {
            return Sym(i as u32);
        }
        self.cap.strings.push(s.to_owned());
        Sym(self.cap.strings.len() as u32 - 1)
    }
    fn obj(&mut self, id: u32, class: &str, parent: u32) -> ObjId {
        let class = self.sym(class);
        let id = ObjId(id);
        let parent = (parent != 0).then_some(ObjId(parent));
        self.cap
            .objects
            .insert(id, ObjectInfo { id, class, parent });
        id
    }
    fn s(&mut self, s: &str) -> Value {
        Value::String(self.sym(s))
    }
    fn dict(&mut self, kv: Vec<(&str, Value)>) -> Value {
        Value::Dict(kv.into_iter().map(|(k, v)| (self.sym(k), v)).collect())
    }
    fn typed(&mut self, kind: TypedKind, bytes: usize) -> Value {
        self.cap.blobs.push(vec![0; bytes]);
        Value::Typed {
            kind,
            blob: self.cap.blobs.len() as u32 - 1,
        }
    }
    fn call(&mut self, sec: Section, this: u32, m: &str, args: Vec<Value>, result: Value) {
        let c = Call {
            this: ObjId(this),
            method: self.sym(m),
            args,
            result,
        };
        match sec {
            Section::Setup => self.cap.setup.push(c),
            Section::Snapshot => self.cap.snapshot.push(c),
            Section::Frame => self.cap.frame.push(c),
        }
    }
    fn setup(&mut self, this: u32, m: &str, args: Vec<Value>, result: Value) {
        self.call(Section::Setup, this, m, args, result);
    }
    fn snap(&mut self, this: u32, m: &str, args: Vec<Value>) {
        self.call(Section::Snapshot, this, m, args, Value::Undefined);
    }
    fn f(&mut self, this: u32, m: &str, args: Vec<Value>) {
        self.call(Section::Frame, this, m, args, Value::Undefined);
    }
    fn fr(&mut self, this: u32, m: &str, args: Vec<Value>, result: Value) {
        self.call(Section::Frame, this, m, args, result);
    }
}

fn n(v: f64) -> Value {
    Value::Number(v)
}
fn o(id: u32) -> Value {
    Value::ObjRef(ObjId(id))
}

/// (section, index, access, revision) of each event, for compact assertions.
fn events(h: &[Revision]) -> Vec<(Section, usize, Access, u32)> {
    h.iter()
        .map(|r| (r.section, r.index, r.access, r.revision))
        .collect()
}

use webgd_core::Section::{Frame, Setup};
use webgd_core::track::revisions::Access::{Create, Destroy, Read, Write};

#[test]
fn gl_buffer_sub_data_through_binding() {
    let mut b = B::default();
    b.obj(1, "WebGL2RenderingContext", 0);
    b.obj(2, "WebGLBuffer", 1);
    b.obj(3, "WebGLBuffer", 1);
    b.obj(4, "WebGLVertexArrayObject", 1);
    b.setup(1, "createBuffer", vec![], o(2)); // S0
    b.setup(1, "createBuffer", vec![], o(3)); // S1
    b.setup(1, "createVertexArray", vec![], o(4)); // S2
    let state = b.dict(vec![("34964", o(2)), ("34229", o(4))]);
    b.snap(1, "webgd:glState", vec![state]);
    let vao = b.dict(vec![
        ("attribs", Value::Array(vec![])),
        ("elementBuffer", o(3)),
    ]);
    b.snap(1, "webgd:glVertexArray", vec![o(4), vao]);

    let data = b.typed(TypedKind::Float32, 16);
    b.f(1, "bufferSubData", vec![n(34962.0), n(16.0), data]); // 0: ARRAY_BUFFER → buffer 2
    let idx = b.typed(TypedKind::Uint16, 6);
    b.f(1, "bufferSubData", vec![n(34963.0), n(0.0), idx]); // 1: ELEMENT_ARRAY_BUFFER via VAO → buffer 3
    b.f(1, "bindBuffer", vec![n(34962.0), o(3)]); // 2
    b.f(1, "bufferData", vec![n(34962.0), n(64.0), n(35044.0)]); // 3 → buffer 3
    b.f(1, "bindVertexArray", vec![Value::Null]); // 4: default VAO has no element buffer
    let idx2 = b.typed(TypedKind::Uint16, 2);
    b.f(1, "bufferSubData", vec![n(34963.0), n(0.0), idx2]); // 5: unattributed

    let h = all(&b.cap);
    assert!(h.values().flatten().all(|r| r.section != Section::Snapshot));
    assert!(!h.contains_key(&ObjId(1)), "contexts are not resources");
    assert_eq!(
        events(&h[&ObjId(2)]),
        vec![(Setup, 0, Create, 0), (Frame, 0, Write, 1)]
    );
    assert_eq!(h[&ObjId(2)][0].detail, "createBuffer");
    assert_eq!(h[&ObjId(2)][1].detail, "bufferSubData 16 B @ 16");
    assert_eq!(
        events(&h[&ObjId(3)]),
        vec![
            (Setup, 1, Create, 0),
            (Frame, 1, Write, 1),
            (Frame, 3, Write, 2)
        ]
    );
    assert_eq!(h[&ObjId(3)][1].detail, "bufferSubData 6 B @ 0");
    assert_eq!(h[&ObjId(3)][2].detail, "bufferData 64 B");
}

#[test]
fn gl_draw_writes_fbo_attachment() {
    let mut b = B::default();
    b.obj(1, "WebGL2RenderingContext", 0);
    b.obj(5, "WebGLTexture", 1);
    b.obj(6, "WebGLFramebuffer", 1);
    b.obj(7, "WebGLRenderbuffer", 1);
    b.obj(8, "WebGLTexture", 1);
    b.obj(9, "WebGLProgram", 1);
    b.setup(1, "createTexture", vec![], o(5)); // S0
    b.setup(1, "createFramebuffer", vec![], o(6)); // S1
    b.setup(1, "createRenderbuffer", vec![], o(7)); // S2
    b.setup(1, "createTexture", vec![], o(8)); // S3
    b.setup(1, "createProgram", vec![], o(9)); // S4
    let depth = b.dict(vec![("objectType", n(36161.0)), ("object", o(7))]);
    let atts = b.dict(vec![("36096", depth)]);
    let fb = b.dict(vec![("attachments", atts)]);
    b.snap(1, "webgd:glFramebuffer", vec![o(6), fb]);
    let unit0 = b.dict(vec![("2d", o(8))]);
    let state = b.dict(vec![("units", Value::Array(vec![unit0]))]);
    b.snap(1, "webgd:glState", vec![state]);

    b.f(1, "bindFramebuffer", vec![n(36160.0), o(6)]); // 0
    b.f(
        1,
        "framebufferTexture2D",
        vec![n(36160.0), n(36064.0), n(3553.0), o(5), n(0.0)],
    ); // 1
    b.f(1, "useProgram", vec![o(9)]); // 2
    b.f(1, "clear", vec![n(16384.0)]); // 3: COLOR_BUFFER_BIT only
    b.f(1, "drawElements", vec![n(4.0), n(36.0), n(5123.0), n(0.0)]); // 4
    b.f(1, "bindFramebuffer", vec![n(36160.0), Value::Null]); // 5
    b.f(1, "drawArrays", vec![n(4.0), n(0.0), n(3.0)]); // 6: default framebuffer

    let h = all(&b.cap);
    let tex = &h[&ObjId(5)];
    assert_eq!(
        events(tex),
        vec![
            (Setup, 0, Create, 0),
            (Frame, 3, Write, 1),
            (Frame, 4, Write, 2)
        ]
    );
    assert_eq!(tex[1].detail, "color attachment 0 (clear)");
    assert_eq!(tex[2].detail, "color attachment 0 (drawElements)");

    let rb = &h[&ObjId(7)];
    assert_eq!(
        events(rb),
        vec![(Setup, 2, Create, 0), (Frame, 4, Write, 1)]
    );
    assert_eq!(rb[1].detail, "depth attachment (drawElements)");

    let fbh = &h[&ObjId(6)];
    assert_eq!(
        events(fbh),
        vec![(Setup, 1, Create, 0), (Frame, 1, Write, 1)]
    );

    let sampled = &h[&ObjId(8)];
    assert_eq!(
        events(sampled),
        vec![
            (Setup, 3, Create, 0),
            (Frame, 4, Read, 0),
            (Frame, 6, Read, 0)
        ]
    );
    assert_eq!(sampled[1].detail, "texture unit 0 (drawElements)");

    assert_eq!(
        events(&h[&ObjId(9)]),
        vec![
            (Setup, 4, Create, 0),
            (Frame, 2, Read, 0),
            (Frame, 4, Read, 0),
            (Frame, 6, Read, 0)
        ]
    );
}

#[test]
fn gpu_write_buffer_and_copy() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(2, "GPUQueue", 1);
    b.obj(3, "GPUCommandEncoder", 1);
    b.obj(4, "GPUBuffer", 1);
    b.obj(5, "GPUBuffer", 1);
    let d4 = b.dict(vec![("size", n(80.0)), ("usage", n(12.0))]);
    b.setup(1, "createBuffer", vec![d4], o(4)); // S0
    let d5 = b.dict(vec![("size", n(80.0)), ("usage", n(72.0))]);
    b.setup(1, "createBuffer", vec![d5], o(5)); // S1

    let data = b.typed(TypedKind::Float32, 16);
    b.f(2, "writeBuffer", vec![o(4), n(0.0), data]); // 0
    b.fr(1, "createCommandEncoder", vec![], o(3)); // 1
    b.f(
        3,
        "copyBufferToBuffer",
        vec![o(4), n(0.0), o(5), n(0.0), n(80.0)],
    ); // 2

    let h = all(&b.cap);
    assert!(!h.contains_key(&ObjId(3)), "encoders are not resources");
    let src = &h[&ObjId(4)];
    assert_eq!(
        events(src),
        vec![
            (Setup, 0, Create, 0),
            (Frame, 0, Write, 1),
            (Frame, 2, Read, 1)
        ]
    );
    assert_eq!(src[0].detail, "createBuffer 80 B COPY_SRC|COPY_DST");
    assert_eq!(src[1].detail, "writeBuffer 16 B @ 0");
    assert_eq!(src[2].detail, "copy source (copyBufferToBuffer)");
    let dst = &h[&ObjId(5)];
    assert_eq!(
        events(dst),
        vec![(Setup, 1, Create, 0), (Frame, 2, Write, 1)]
    );
    assert_eq!(dst[1].detail, "copyBufferToBuffer 80 B");
}

#[test]
fn render_pass_clear_writes_view_parent_texture() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(3, "GPUCommandEncoder", 1);
    b.obj(10, "GPUTexture", 1);
    b.obj(11, "GPUTextureView", 10);
    b.obj(12, "GPURenderPassEncoder", 3);
    b.obj(13, "GPURenderPipeline", 1);
    let fmt = b.s("rgba8unorm");
    let td = b.dict(vec![
        ("format", fmt),
        ("size", Value::Array(vec![n(256.0), n(256.0)])),
        ("usage", n(17.0)),
    ]);
    b.setup(1, "createTexture", vec![td], o(10)); // S0
    b.setup(10, "createView", vec![], o(11)); // S1

    b.fr(1, "createCommandEncoder", vec![], o(3)); // 0
    let clear = b.s("clear");
    let store = b.s("store");
    let col = b.dict(vec![("view", o(11)), ("loadOp", clear), ("storeOp", store)]);
    let desc = b.dict(vec![("colorAttachments", Value::Array(vec![col]))]);
    b.fr(3, "beginRenderPass", vec![desc], o(12)); // 1
    b.f(12, "setPipeline", vec![o(13)]); // 2
    b.f(12, "draw", vec![n(3.0)]); // 3
    b.f(12, "end", vec![]); // 4
    b.f(10, "destroy", vec![]); // 5

    let h = all(&b.cap);
    let tex = &h[&ObjId(10)];
    assert_eq!(
        events(tex),
        vec![
            (Setup, 0, Create, 0),
            (Frame, 1, Write, 1),
            (Frame, 3, Write, 2),
            (Frame, 5, Destroy, 2)
        ]
    );
    assert_eq!(tex[0].detail, "createTexture rgba8unorm 256×256");
    assert_eq!(tex[1].detail, "color attachment 0 (clear)");
    assert_eq!(tex[2].detail, "color attachment 0 (draw)");

    let view = &h[&ObjId(11)];
    assert_eq!(
        events(view),
        vec![
            (Setup, 1, Create, 0),
            (Frame, 1, Write, 1),
            (Frame, 3, Write, 2)
        ]
    );
    assert_eq!(view[0].detail, "createView");

    // Pipeline first seen without a Create.
    assert_eq!(events(&h[&ObjId(13)]), vec![(Frame, 2, Read, 0)]);
}

#[test]
fn dispatch_writes_storage_bindings() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(3, "GPUCommandEncoder", 1);
    for id in [20, 21, 25, 26] {
        b.obj(id, "GPUBuffer", 1);
    }
    b.obj(22, "GPUBindGroupLayout", 1);
    b.obj(23, "GPUBindGroup", 1);
    b.obj(24, "GPUBindGroup", 1);
    b.obj(27, "GPUComputePassEncoder", 3);
    b.obj(28, "GPUComputePipeline", 1);
    b.obj(29, "GPUBindGroupLayout", 28);

    for (id, usage) in [(20, 0x84), (21, 0x48), (25, 0x84), (26, 0x48)] {
        let d = b.dict(vec![("size", n(4096.0)), ("usage", n(f64::from(usage)))]);
        b.setup(1, "createBuffer", vec![d], o(id)); // S0..S3
    }
    let storage = b.s("storage");
    let sb = b.dict(vec![("type", storage)]);
    let e0 = b.dict(vec![("binding", n(0.0)), ("buffer", sb)]);
    let ub = b.dict(vec![]);
    let e1 = b.dict(vec![("binding", n(1.0)), ("buffer", ub)]);
    let ld = b.dict(vec![("entries", Value::Array(vec![e0, e1]))]);
    b.setup(1, "createBindGroupLayout", vec![ld], o(22)); // S4
    let r0 = b.dict(vec![("buffer", o(20))]);
    let g0 = b.dict(vec![("binding", n(0.0)), ("resource", r0)]);
    let r1 = b.dict(vec![("buffer", o(21))]);
    let g1 = b.dict(vec![("binding", n(1.0)), ("resource", r1)]);
    let gd = b.dict(vec![
        ("layout", o(22)),
        ("entries", Value::Array(vec![g0, g1])),
    ]);
    b.setup(1, "createBindGroup", vec![gd], o(23)); // S5
    b.setup(1, "createComputePipeline", vec![], o(28)); // S6
    b.setup(28, "getBindGroupLayout", vec![n(0.0)], o(29)); // S7
    let a0 = b.dict(vec![("binding", n(0.0)), ("resource", o(25))]);
    let r3 = b.dict(vec![("buffer", o(26))]);
    let a1 = b.dict(vec![("binding", n(1.0)), ("resource", r3)]);
    let ad = b.dict(vec![
        ("layout", o(29)),
        ("entries", Value::Array(vec![a0, a1])),
    ]);
    b.setup(1, "createBindGroup", vec![ad], o(24)); // S8

    b.fr(1, "createCommandEncoder", vec![], o(3)); // 0
    b.fr(3, "beginComputePass", vec![], o(27)); // 1
    b.f(27, "setPipeline", vec![o(28)]); // 2
    b.f(27, "setBindGroup", vec![n(0.0), o(23)]); // 3
    b.f(27, "setBindGroup", vec![n(1.0), o(24)]); // 4
    b.f(27, "dispatchWorkgroups", vec![n(4.0)]); // 5
    b.f(27, "end", vec![]); // 6

    let h = all(&b.cap);
    // Explicit layout: storage → Write, uniform → Read.
    assert_eq!(
        events(&h[&ObjId(20)]),
        vec![(Setup, 0, Create, 0), (Frame, 5, Write, 1)]
    );
    assert_eq!(
        h[&ObjId(20)][1].detail,
        "bind group 0 binding 0 (dispatchWorkgroups)"
    );
    assert_eq!(
        events(&h[&ObjId(21)]),
        vec![(Setup, 1, Create, 0), (Frame, 5, Read, 0)]
    );
    // Auto layout: STORAGE usage → Write, otherwise Read.
    assert_eq!(
        events(&h[&ObjId(25)]),
        vec![(Setup, 2, Create, 0), (Frame, 5, Write, 1)]
    );
    assert_eq!(
        events(&h[&ObjId(26)]),
        vec![(Setup, 3, Create, 0), (Frame, 5, Read, 0)]
    );
    assert_eq!(
        events(&h[&ObjId(23)]),
        vec![(Setup, 5, Create, 0), (Frame, 3, Read, 0)]
    );
    assert_eq!(h[&ObjId(23)][1].detail, "setBindGroup 0");
    assert_eq!(events(&h[&ObjId(29)]), vec![(Setup, 7, Create, 0)]);
}

#[test]
fn mapped_write_counts_once() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(4, "GPUBuffer", 1);
    b.obj(5, "GPUBuffer", 1);
    let t = Value::Bool(true);
    let d = b.dict(vec![
        ("size", n(16.0)),
        ("usage", n(6.0)),
        ("mappedAtCreation", t),
    ]);
    b.fr(1, "createBuffer", vec![d.clone()], o(4)); // 0
    let bytes = b.typed(TypedKind::Uint8, 16);
    b.f(4, "webgd:mappedWrite", vec![n(0.0), bytes]); // 1
    b.f(4, "unmap", vec![]); // 2
    b.fr(1, "createBuffer", vec![d], o(5)); // 3
    b.f(5, "unmap", vec![]); // 4: no recorded bytes → unmap is the write

    let h = all(&b.cap);
    assert_eq!(
        events(&h[&ObjId(4)]),
        vec![(Frame, 0, Create, 0), (Frame, 1, Write, 1)]
    );
    assert_eq!(h[&ObjId(4)][1].detail, "mapped write 16 B @ 0");
    assert_eq!(
        events(&h[&ObjId(5)]),
        vec![(Frame, 3, Create, 0), (Frame, 4, Write, 1)]
    );
}

#[test]
fn revision_at_points() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(2, "GPUQueue", 1);
    b.obj(4, "GPUBuffer", 1);
    b.obj(6, "GPUBuffer", 1);
    b.obj(7, "GPUBuffer", 1);
    let d = b.dict(vec![("size", n(64.0)), ("usage", n(8.0))]);
    b.setup(1, "createBuffer", vec![d.clone()], o(4)); // S0
    let data = b.typed(TypedKind::Uint8, 4);

    b.f(2, "submit", vec![Value::Array(vec![])]); // 0
    b.fr(1, "createBuffer", vec![d], o(6)); // 1
    b.f(2, "writeBuffer", vec![o(4), n(0.0), data.clone()]); // 2
    b.f(2, "writeBuffer", vec![o(6), n(0.0), data.clone()]); // 3
    b.f(2, "writeBuffer", vec![o(4), n(4.0), data.clone()]); // 4
    b.f(6, "destroy", vec![]); // 5
    b.f(2, "writeBuffer", vec![o(7), n(0.0), data]); // 6

    let h = all(&b.cap);
    let h4 = &h[&ObjId(4)];
    assert_eq!(
        events(h4),
        vec![
            (Setup, 0, Create, 0),
            (Frame, 2, Write, 1),
            (Frame, 4, Write, 2)
        ]
    );
    assert_eq!(h4[2].detail, "writeBuffer 4 B @ 4");
    let at = |i| revision_at(h4, i);
    assert_eq!(
        (at(0), at(1), at(2), at(3), at(4), at(100)),
        (Some(0), Some(0), Some(1), Some(1), Some(2), Some(2))
    );

    // Created inside the frame: absent before its creation.
    let h6 = &h[&ObjId(6)];
    assert_eq!(
        events(h6),
        vec![
            (Frame, 1, Create, 0),
            (Frame, 3, Write, 1),
            (Frame, 5, Destroy, 1)
        ]
    );
    assert_eq!(revision_at(h6, 0), None);
    assert_eq!(revision_at(h6, 1), Some(0));
    assert_eq!(revision_at(h6, 3), Some(1));
    assert_eq!(revision_at(h6, 5), Some(1));

    // Pre-existing without a Create: revision 0 until the first write.
    let h7 = &h[&ObjId(7)];
    assert_eq!(events(h7), vec![(Frame, 6, Write, 1)]);
    assert_eq!(revision_at(h7, 0), Some(0));
    assert_eq!(revision_at(h7, 6), Some(1));

    assert_eq!(revision_at(&[], 0), None);
}
