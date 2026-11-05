use webgd_core::edits::*;
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
    fn obj(&mut self, id: u32, class: &str) {
        let class = self.sym(class);
        let id = ObjId(id);
        self.cap.objects.insert(
            id,
            ObjectInfo {
                id,
                class,
                parent: None,
            },
        );
    }
    fn dict(&mut self, kv: Vec<(&str, Value)>) -> Value {
        Value::Dict(kv.into_iter().map(|(k, v)| (self.sym(k), v)).collect())
    }
    fn call(&mut self, sec: Section, this: u32, m: &str, args: Vec<Value>) {
        let c = Call {
            this: ObjId(this),
            method: self.sym(m),
            args,
            result: Value::Undefined,
        };
        match sec {
            Section::Setup => self.cap.setup.push(c),
            Section::Snapshot => self.cap.snapshot.push(c),
            Section::Frame => self.cap.frame.push(c),
        }
    }
}

fn n(v: f64) -> Value {
    Value::Number(v)
}

fn nums(args: &[Value]) -> Vec<f64> {
    args.iter().map(|v| v.as_f64().expect("number")).collect()
}

/// GL context #1 with a snapshot viewport of 0,0,640,480 and frame: viewport(0,0,256,256), drawArrays.
fn gl_capture() -> B {
    let mut b = B::default();
    b.obj(1, "WebGL2RenderingContext");
    let state = b.dict(vec![
        (
            "2978",
            Value::Array(vec![n(0.0), n(0.0), n(640.0), n(480.0)]),
        ),
        ("2929", Value::Bool(false)),
    ]);
    b.call(Section::Snapshot, 1, "webgd:glState", vec![state]);
    b.call(
        Section::Frame,
        1,
        "viewport",
        vec![n(0.0), n(0.0), n(256.0), n(256.0)],
    );
    b.call(
        Section::Frame,
        1,
        "drawArrays",
        vec![n(4.0), n(0.0), n(3.0)],
    );
    b
}

fn template(class: &str, label: &str) -> CommandTemplate {
    templates_for(class)
        .into_iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no template {label} for {class}"))
}

#[test]
fn apply_orders_insertions_and_tracks_origins() {
    let b = gl_capture();
    let orig = &b.cap;
    let mut e = CaptureEdits::default();
    let enable = orig.sym("viewport").unwrap();
    let mk = |args: Vec<Value>| Call {
        this: ObjId(1),
        method: enable,
        args,
        result: Value::Undefined,
    };
    e.inserted
        .insert(0, vec![mk(vec![n(1.0), n(1.0), n(1.0), n(1.0)])]);
    e.inserted.insert(
        1,
        vec![
            mk(vec![n(2.0), n(2.0), n(2.0), n(2.0)]),
            mk(vec![n(3.0), n(3.0), n(3.0), n(3.0)]),
        ],
    );
    e.inserted
        .insert(2, vec![mk(vec![n(4.0), n(4.0), n(4.0), n(4.0)])]);
    e.inserted
        .insert(9, vec![mk(vec![n(9.0), n(9.0), n(9.0), n(9.0)])]); // out of range: ignored
    e.modified
        .insert(0, vec![n(0.0), n(0.0), n(128.0), n(128.0)]);

    let ed = apply(orig, &e);
    assert_eq!(
        ed.origin,
        vec![
            EditOrigin::Inserted { before: 0, slot: 0 },
            EditOrigin::Modified(0),
            EditOrigin::Inserted { before: 1, slot: 0 },
            EditOrigin::Inserted { before: 1, slot: 1 },
            EditOrigin::Original(1),
            EditOrigin::Inserted { before: 2, slot: 0 },
        ]
    );
    assert_eq!(ed.cap.frame.len(), 6);
    assert_eq!(nums(&ed.cap.frame[1].args), vec![0.0, 0.0, 128.0, 128.0]);
    assert_eq!(nums(&ed.cap.frame[3].args), vec![3.0; 4]);
    assert_eq!(ed.cap.method(&ed.cap.frame[4]), "drawArrays");
    // Setup/snapshot untouched; original not mutated.
    assert_eq!(ed.cap.snapshot, orig.snapshot);
    assert_eq!(nums(&orig.frame[0].args), vec![0.0, 0.0, 256.0, 256.0]);

    assert_eq!(edited_index_of(&ed.origin, 0), Some(1));
    assert_eq!(edited_index_of(&ed.origin, 1), Some(4));
    assert_eq!(edited_index_of(&ed.origin, 5), None);
}

#[test]
fn unchanged_modification_stays_original() {
    let b = gl_capture();
    let mut e = CaptureEdits::default();
    e.modified.insert(0, b.cap.frame[0].args.clone());
    let ed = apply(&b.cap, &e);
    assert_eq!(
        ed.origin,
        vec![EditOrigin::Original(0), EditOrigin::Original(1)]
    );
}

#[test]
fn intern_reuses_strings_and_resolves_after_apply() {
    let b = gl_capture();
    let mut e = CaptureEdits::default();
    assert_eq!(e.intern(&b.cap, "viewport"), b.cap.sym("viewport").unwrap());
    let a = e.intern(&b.cap, "enable");
    let a2 = e.intern(&b.cap, "enable");
    assert_eq!(a, a2);
    assert_eq!(e.strings, vec!["enable".to_owned()]);
    assert_eq!(a.0 as usize, b.cap.strings.len());
    e.inserted.insert(
        1,
        vec![Call {
            this: ObjId(1),
            method: a,
            args: vec![n(2929.0)],
            result: Value::Undefined,
        }],
    );
    let ed = apply(&b.cap, &e);
    assert_eq!(ed.cap.method(&ed.cap.frame[1]), "enable");
}

#[test]
fn templates_and_params() {
    let gl = templates_for("WebGL2RenderingContext");
    assert!(
        gl.iter()
            .any(|t| t.label == "enable(DEPTH_TEST)" && t.method == "enable")
    );
    assert!(gl.iter().any(|t| t.method == "viewport"));
    let pass = templates_for("GPURenderPassEncoder");
    assert!(pass.iter().any(|t| t.method == "setViewport"));
    assert!(pass.iter().any(|t| t.method == "setScissorRect"));
    assert!(templates_for("GPUCommandEncoder").is_empty());

    let b = gl_capture();
    let p = params_for_call(&b.cap, &b.cap.frame[0]).expect("viewport params");
    assert_eq!(p.len(), 4);
    assert!(p.iter().all(|s| matches!(s.kind, ParamKind::Int { .. })));
    assert!(
        params_for_call(&b.cap, &b.cap.frame[1]).is_none(),
        "drawArrays has no typed template"
    );
}

#[test]
fn gl_defaults_follow_snapshot_and_frame_state() {
    let b = gl_capture();
    let mut e = CaptureEdits::default();
    let vp = template("WebGL2RenderingContext", "viewport");
    // Before frame[0]: snapshot viewport.
    assert_eq!(
        nums(&default_args(&b.cap, &mut e, &vp, ObjId(1), 0)),
        vec![0.0, 0.0, 640.0, 480.0]
    );
    // After frame[0] viewport(0,0,256,256).
    assert_eq!(
        nums(&default_args(&b.cap, &mut e, &vp, ObjId(1), 1)),
        vec![0.0, 0.0, 256.0, 256.0]
    );
    // Fixed-cap enable template defaults to its cap.
    let en = template("WebGL2RenderingContext", "enable(DEPTH_TEST)");
    assert_eq!(
        nums(&default_args(&b.cap, &mut e, &en, ObjId(1), 1)),
        vec![2929.0]
    );
}

#[test]
fn gpu_set_viewport_defaults_from_pass_state() {
    let mut b = B::default();
    b.obj(2, "GPUCommandEncoder");
    b.obj(3, "GPURenderPassEncoder");
    let desc = b.dict(vec![("colorAttachments", Value::Array(vec![]))]);
    let begin = b.sym("beginRenderPass");
    b.cap.frame.push(Call {
        this: ObjId(2),
        method: begin,
        args: vec![desc],
        result: Value::ObjRef(ObjId(3)),
    });
    b.call(
        Section::Frame,
        3,
        "setViewport",
        vec![n(10.0), n(20.0), n(100.0), n(50.0), n(0.0), n(1.0)],
    );
    b.call(Section::Frame, 3, "draw", vec![n(3.0)]);
    let mut e = CaptureEdits::default();
    let t = template("GPURenderPassEncoder", "setViewport");
    let args = default_args(&b.cap, &mut e, &t, ObjId(3), 2);
    assert_eq!(nums(&args), vec![10.0, 20.0, 100.0, 50.0, 0.0, 1.0]);
}
