use webgd_core::track::commands::{CmdKind, build_tree};
use webgd_core::track::gl::{TEXTURE_2D, TEXTURE_CUBE_MAP, texture_dims};
use webgd_core::track::gpu::{creation_call, state_at};
use webgd_core::track::resources::{Origin, list};
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
    fn blob(&mut self, bytes: Vec<u8>) -> u32 {
        self.cap.blobs.push(bytes);
        self.cap.blobs.len() as u32 - 1
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
    fn f(&mut self, this: u32, m: &str, args: Vec<Value>) {
        self.call(Section::Frame, this, m, args, Value::Undefined);
    }
}

fn n(v: f64) -> Value {
    Value::Number(v)
}
fn o(id: u32) -> Value {
    Value::ObjRef(ObjId(id))
}

#[test]
fn command_tree() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(2, "GPUQueue", 1);
    b.obj(3, "GPUCommandEncoder", 1);
    b.obj(4, "GPURenderPassEncoder", 3);
    b.obj(5, "GPUCommandBuffer", 3);
    b.obj(6, "WebGL2RenderingContext", 0);
    b.obj(7, "GPUComputePassEncoder", 3);
    let grp = b.s("Offscreen");
    b.f(3, "pushDebugGroup", vec![grp]); // 0
    b.call(
        Section::Frame,
        3,
        "beginRenderPass",
        vec![Value::Dict(vec![])],
        o(4),
    ); // 1
    b.f(4, "setPipeline", vec![o(9)]); // 2
    b.f(4, "drawIndexed", vec![n(36.0)]); // 3
    b.f(4, "end", vec![]); // 4
    b.f(3, "popDebugGroup", vec![]); // 5
    b.f(3, "popDebugGroup", vec![]); // 6 unbalanced
    b.f(3, "copyBufferToBuffer", vec![]); // 7
    b.call(Section::Frame, 3, "finish", vec![], o(5)); // 8
    b.f(2, "submit", vec![Value::Array(vec![o(5)])]); // 9
    let m = b.s("Composite");
    b.f(6, "webgd:pushMarker", vec![m]); // 10
    b.f(6, "clear", vec![n(16384.0)]); // 11
    b.f(6, "drawArrays", vec![n(4.0), n(0.0), n(3.0)]); // 12
    b.f(6, "drawBuffers", vec![]); // 13
    b.f(6, "blitFramebuffer", vec![]); // 14
    b.call(Section::Frame, 3, "beginComputePass", vec![], o(7)); // 15
    b.f(7, "dispatchWorkgroups", vec![n(4.0)]); // 16 (pass and marker never closed)
    let t = build_tree(&b.cap);
    let got: Vec<(u16, CmdKind, usize)> = t
        .iter()
        .map(|n| (n.depth, n.kind.clone(), n.span_end))
        .collect();
    use CmdKind::*;
    assert_eq!(
        got,
        vec![
            (0, GroupBegin("Offscreen".into()), 5),
            (1, PassBegin, 4),
            (2, Call, 2),
            (2, Draw, 3),
            (1, PassEnd, 4),
            (0, GroupEnd, 5),
            (0, Call, 6),
            (0, Copy, 7),
            (0, Call, 8),
            (0, Submit, 9),
            (0, GroupBegin("Composite".into()), 16),
            (1, Clear, 11),
            (1, Draw, 12),
            (1, Call, 13),
            (1, Clear, 14),
            (1, PassBegin, 16),
            (2, Dispatch, 16),
        ]
    );
    assert!(t.iter().enumerate().all(|(i, n)| n.index == i));
}

#[test]
fn command_tree_interleaved_close() {
    // Pass ended while a debug group pushed on the pass is still open: both close at `end`.
    let mut b = B::default();
    b.obj(3, "GPUCommandEncoder", 0);
    b.obj(4, "GPURenderPassEncoder", 3);
    b.call(Section::Frame, 3, "beginRenderPass", vec![], o(4));
    let g = b.s("g");
    b.f(4, "pushDebugGroup", vec![g]);
    b.f(4, "draw", vec![n(3.0)]);
    b.f(4, "end", vec![]);
    b.f(4, "popDebugGroup", vec![]);
    let t = build_tree(&b.cap);
    assert_eq!(t[0].span_end, 3);
    assert_eq!(t[1].span_end, 3);
    assert_eq!((t[2].depth, &t[2].kind), (2, &CmdKind::Draw));
    assert_eq!((t[3].depth, &t[3].kind), (0, &CmdKind::PassEnd));
    assert_eq!((t[4].depth, &t[4].kind), (0, &CmdKind::Call));
}

#[test]
fn gpu_state() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(3, "GPUCommandEncoder", 1);
    b.obj(4, "GPURenderPassEncoder", 3);
    b.obj(5, "GPURenderPipeline", 1);
    b.obj(6, "GPUBindGroup", 1);
    b.obj(7, "GPUBuffer", 1);
    b.obj(8, "GPUBuffer", 1);
    b.obj(9, "GPUBindGroup", 1);
    let label = b.s("pipe");
    let pdesc = b.dict(vec![("label", label)]);
    b.call(Section::Setup, 1, "createRenderPipeline", vec![pdesc], o(5));
    let desc = b.dict(vec![("colorAttachments", Value::Array(vec![]))]);
    b.call(
        Section::Frame,
        3,
        "beginRenderPass",
        vec![desc.clone()],
        o(4),
    ); // 0
    b.f(4, "setPipeline", vec![o(5)]); // 1
    let offs = b.blob(
        [16u32, 32, 48]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
    );
    b.f(
        4,
        "setBindGroup",
        vec![
            n(0.0),
            o(6),
            Value::Typed {
                kind: TypedKind::Uint32,
                blob: offs,
            },
            n(1.0),
            n(2.0),
        ],
    ); // 2
    b.f(
        4,
        "setBindGroup",
        vec![n(1.0), o(9), Value::Array(vec![n(256.0)])],
    ); // 3
    b.f(4, "setVertexBuffer", vec![n(0.0), o(7)]); // 4
    let fmt = b.s("uint16");
    b.f(4, "setIndexBuffer", vec![o(8), fmt, n(4.0), n(72.0)]); // 5
    b.f(
        4,
        "setViewport",
        vec![n(0.0), n(0.0), n(256.0), n(256.0), n(0.0), n(1.0)],
    ); // 6
    b.f(4, "setScissorRect", vec![n(1.0), n(2.0), n(3.0), n(4.0)]); // 7
    b.f(4, "setStencilReference", vec![n(7.0)]); // 8
    b.f(3, "pushDebugGroup", vec![]); // 9 (encoder call inside pass span)
    b.f(4, "setBindGroup", vec![n(1.0), Value::Null]); // 10
    b.f(4, "drawIndexed", vec![n(36.0)]); // 11
    b.f(4, "end", vec![]); // 12
    b.f(3, "popDebugGroup", vec![]); // 13

    assert!(state_at(&b.cap, 100).pass.is_none());
    let s = state_at(&b.cap, 1).pass.unwrap();
    assert_eq!(
        (s.encoder, s.pass, s.compute, s.pipeline),
        (ObjId(3), ObjId(4), false, Some(ObjId(5)))
    );
    assert_eq!(s.desc, desc);
    assert!(s.bind_groups.is_empty());

    let s = state_at(&b.cap, 0).pass.unwrap();
    assert_eq!(s.pipeline, None);

    let s = state_at(&b.cap, 9).pass.unwrap(); // non-pass call inside an open pass
    assert_eq!(s.bind_groups[&0], (ObjId(6), vec![32, 48]));
    assert_eq!(s.bind_groups[&1], (ObjId(9), vec![256]));
    assert_eq!(s.vertex_buffers[&0], (ObjId(7), 0.0, None));
    assert_eq!(
        s.index_buffer,
        Some((ObjId(8), "uint16".into(), 4.0, Some(72.0)))
    );
    assert_eq!(s.viewport, Some([0.0, 0.0, 256.0, 256.0, 0.0, 1.0]));
    assert_eq!(s.scissor, Some([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(s.stencil_ref, Some(7.0));

    let s = state_at(&b.cap, 12).pass.unwrap(); // `end` itself tracks its pass
    assert!(!s.bind_groups.contains_key(&1));
    assert!(state_at(&b.cap, 13).pass.is_none()); // after end

    assert_eq!(creation_call(&b.cap, ObjId(5)), Some((Section::Setup, 0)));
    assert_eq!(creation_call(&b.cap, ObjId(4)), Some((Section::Frame, 0)));
    assert_eq!(creation_call(&b.cap, ObjId(7)), None);
}

#[test]
fn gl_texture_dims() {
    let mut b = B::default();
    b.obj(1, "WebGL2RenderingContext", 0);
    b.obj(2, "WebGLTexture", 1);
    b.obj(3, "WebGLTexture", 1);
    b.obj(4, "WebGLTexture", 1);
    b.obj(5, "WebGLTexture", 1);
    b.call(Section::Setup, 1, "createTexture", vec![], o(2));
    b.call(Section::Setup, 1, "createTexture", vec![], o(3));
    b.call(Section::Setup, 1, "createTexture", vec![], o(5));
    // Setup shadow upload of a LUMINANCE 4x4 texture into tex 5.
    b.call(
        Section::Setup,
        1,
        "webgd:bindForUpload",
        vec![n(3553.0), o(5)],
        Value::Undefined,
    );
    b.call(
        Section::Setup,
        1,
        "texImage2D",
        vec![
            n(3553.0),
            n(0.0),
            n(6409.0),
            n(4.0),
            n(4.0),
            n(0.0),
            n(6409.0),
            n(5121.0),
            Value::Null,
        ],
        Value::Undefined,
    );

    // Snapshot: tex 2 is 64x64 RGBA8 with 2 levels; unit 1 has tex 2 bound on 2D and active unit is 1.
    let lv = |b: &mut B, l: f64, w: f64| {
        b.dict(vec![
            ("level", n(l)),
            ("face", n(0.0)),
            ("layer", n(0.0)),
            ("width", n(w)),
            ("height", n(w)),
            ("depth", n(1.0)),
        ])
    };
    let l0 = lv(&mut b, 0.0, 64.0);
    let l1 = lv(&mut b, 1.0, 32.0);
    let tdesc = b.dict(vec![
        ("target", n(3553.0)),
        ("internalformat", n(32856.0)),
        ("format", n(6408.0)),
        ("type", n(5121.0)),
        ("immutable", Value::Bool(false)),
        ("levels", Value::Array(vec![l0, l1])),
    ]);
    b.call(
        Section::Snapshot,
        1,
        "webgd:glTexture",
        vec![o(2), tdesc],
        Value::Undefined,
    );
    let u0 = b.dict(vec![("2d", Value::Null), ("cube", Value::Null)]);
    let u1 = b.dict(vec![("2d", o(2)), ("cube", Value::Null)]);
    let st = b.dict(vec![
        ("34016", n(33985.0)),
        ("units", Value::Array(vec![u0, u1])),
    ]);
    b.call(
        Section::Snapshot,
        1,
        "webgd:glState",
        vec![st],
        Value::Undefined,
    );

    // Frame: redefine level 0 of tex 2 (bound on unit 1) via the 6-arg source form.
    b.f(
        1,
        "texImage2D",
        vec![
            n(3553.0),
            n(0.0),
            n(32856.0),
            n(6408.0),
            n(5121.0),
            Value::Image {
                width: 128,
                height: 64,
                blob: 0,
            },
        ],
    ); // 0
    b.f(1, "activeTexture", vec![n(33984.0)]); // 1
    b.f(1, "bindTexture", vec![n(34067.0), o(3)]); // 2
    b.f(
        1,
        "texStorage2D",
        vec![n(34067.0), n(3.0), n(34842.0), n(16.0), n(16.0)],
    ); // 3
    b.f(
        1,
        "texImage2D",
        vec![
            n(34071.0),
            n(0.0),
            n(6408.0),
            n(8.0),
            n(8.0),
            n(0.0),
            n(6408.0),
            n(5121.0),
            Value::Null,
        ],
    ); // 4
    b.f(1, "bindTexture", vec![n(3553.0), o(4)]); // 5
    b.f(
        1,
        "copyTexImage2D",
        vec![
            n(3553.0),
            n(0.0),
            n(6408.0),
            n(0.0),
            n(0.0),
            n(10.0),
            n(6.0),
            n(0.0),
        ],
    ); // 6
    b.f(1, "generateMipmap", vec![n(3553.0)]); // 7

    let d = texture_dims(&b.cap, 0, ObjId(2)).unwrap();
    assert_eq!(d.levels[&(0, 0)], (128, 64, 1));
    assert_eq!(d.levels[&(1, 0)], (32, 32, 1));
    assert_eq!(
        (d.target, d.internalformat, d.format, d.ty),
        (TEXTURE_2D, 32856, Some(6408), Some(5121))
    );
    // Frame start: frame[0] redefines tex 2 as 128x64, but frame-start dims stay the snapshot's 64x64.
    let fs = webgd_core::track::gl::texture_dims_at_frame_start(&b.cap, ObjId(2)).unwrap();
    assert_eq!(fs.levels[&(0, 0)], (64, 64, 1));
    assert_eq!(fs.levels[&(1, 0)], (32, 32, 1));

    assert_eq!(texture_dims(&b.cap, 2, ObjId(3)), None);
    let c = texture_dims(&b.cap, 3, ObjId(3)).unwrap();
    assert_eq!(
        (c.target, c.immutable, c.format),
        (TEXTURE_CUBE_MAP, true, None)
    );
    assert_eq!(c.levels.len(), 18);
    assert_eq!(c.levels[&(2, 5)], (4, 4, 1));
    let c = texture_dims(&b.cap, 4, ObjId(3)).unwrap();
    assert_eq!(c.levels[&(0, 2)], (8, 8, 1));

    let g = texture_dims(&b.cap, usize::MAX, ObjId(4)).unwrap();
    assert_eq!(g.format, None);
    assert_eq!(
        g.levels.keys().map(|k| k.0).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(g.levels[&(3, 0)], (1, 1, 1));

    let s = texture_dims(&b.cap, 0, ObjId(5)).unwrap();
    assert_eq!((s.internalformat, s.levels[&(0, 0)]), (6409, (4, 4, 1)));
}

#[test]
fn resources_on_fixture() {
    let cap = wire::decode(include_bytes!("../../../tests/fixtures/sample.wire")).unwrap();
    let rows = list(&cap);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, ObjId(2));
    assert_eq!(rows[0].class, "WebGLBuffer");
    assert_eq!(rows[0].label, "");
    assert_eq!(rows[0].origin, Origin::PreFrame);
    // No ARRAY_BUFFER binding is known, so bufferData cannot be attributed.
    assert_eq!(rows[0].summary, "");
}

#[test]
fn resources_summaries() {
    let mut b = B::default();
    b.obj(1, "GPUDevice", 0);
    b.obj(2, "GPUBuffer", 1);
    b.obj(3, "GPUTexture", 1);
    b.obj(4, "GPUTextureView", 3);
    b.obj(5, "GPUShaderModule", 1);
    b.obj(6, "WebGL2RenderingContext", 0);
    b.obj(7, "WebGLBuffer", 6);
    b.obj(8, "WebGLShader", 6);
    b.obj(9, "GPUQueue", 1);
    b.obj(10, "GPUBuffer", 1); // dead: no creation, never referenced
    b.obj(11, "GPUBuffer", 1); // no creation call but referenced by the frame
    let lbl = b.s("verts");
    let bd = b.dict(vec![("label", lbl), ("size", n(256.0)), ("usage", n(40.0))]);
    b.call(Section::Setup, 1, "createBuffer", vec![bd], o(2));
    let fmt = b.s("rgba16float");
    let td = b.dict(vec![
        ("size", Value::Array(vec![n(256.0), n(128.0)])),
        ("format", fmt),
        ("usage", n(16.0)),
    ]);
    b.call(Section::Setup, 1, "createTexture", vec![td], o(3));
    let new_label = b.s("offscreen");
    b.call(
        Section::Setup,
        3,
        "set:label",
        vec![new_label],
        Value::Undefined,
    );
    b.call(Section::Setup, 6, "createBuffer", vec![], o(7));
    b.call(Section::Setup, 6, "createShader", vec![n(35633.0)], o(8));
    b.call(Section::Setup, 1, "get:queue", vec![], o(9));
    b.f(6, "bindBuffer", vec![n(34962.0), o(7)]); // 0
    let data = b.blob(vec![0; 48]);
    b.f(
        6,
        "bufferData",
        vec![
            n(34962.0),
            Value::Typed {
                kind: TypedKind::Float32,
                blob: data,
            },
            n(35044.0),
        ],
    ); // 1
    b.call(Section::Frame, 3, "createView", vec![], o(4)); // 2
    let code = b.s("@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { return vec4f(); }\n@compute @workgroup_size(64)\nfn  cs_main() {}");
    let sd = b.dict(vec![("code", code)]);
    b.call(Section::Frame, 1, "createShaderModule", vec![sd], o(5)); // 3
    let sub = b.dict(vec![("buffers", Value::Array(vec![o(11)]))]);
    b.f(9, "writeBuffer", vec![sub]); // 4

    let rows = list(&b.cap);
    let get = |id: u32| rows.iter().find(|r| r.id == ObjId(id)).unwrap();
    let ids: Vec<u32> = rows.iter().map(|r| r.id.0).collect();
    assert_eq!(ids, vec![2, 3, 4, 5, 7, 8, 11]);
    assert_eq!(get(11).origin, Origin::PreFrame);
    assert_eq!(
        (get(2).label.as_str(), get(2).summary.as_str()),
        ("verts", "256 B COPY_DST|VERTEX")
    );
    assert_eq!(
        (get(3).label.as_str(), get(3).summary.as_str()),
        ("offscreen", "rgba16float 256×128×1 1 mips")
    );
    assert_eq!(
        (get(4).summary.as_str(), get(4).origin),
        ("view of GPUTexture#3", Origin::Frame(2))
    );
    assert_eq!(
        (get(5).summary.as_str(), get(5).origin),
        ("vertex vs, compute cs_main", Origin::Frame(3))
    );
    assert_eq!(
        (get(7).summary.as_str(), get(7).origin),
        ("48 B", Origin::PreFrame)
    );
    assert_eq!(get(8).summary, "vertex");
}
