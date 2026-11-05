//! User edits to a decoded capture: modify the arguments of recorded frame commands and insert new
//! commands (typically dynamic state such as viewport, scissor, depth/stencil/blend enables).
//!
//! Edits are non-destructive: they are kept next to the original capture and applied to produce an
//! edited `Capture` that every tracker and the replay engine consume unchanged. Each frame command
//! of the edited capture carries an [`EditOrigin`] so the UI can highlight what changed.
use crate::capture::{Call, Capture, ObjId, Sym, TypedKind, Value};
use crate::track::gpu::{creation_call, state_at};
use std::collections::{BTreeMap, HashMap};

/// Where a frame command of the edited capture comes from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EditOrigin {
    /// Unchanged original `frame[i]`.
    Original(usize),
    /// Original `frame[i]` with edited arguments.
    Modified(usize),
    /// A user-inserted command placed before original `frame[before]`
    /// (`before == original.frame.len()` = at the end); `slot` is its position among the
    /// commands inserted at that point.
    Inserted { before: usize, slot: usize },
}

/// Edits against one original capture. Indices always refer to the ORIGINAL frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureEdits {
    /// Replacement argument lists for original frame commands (same receiver and method).
    pub modified: BTreeMap<usize, Vec<Value>>,
    /// Commands inserted before original frame index `k` (`k == frame.len()` = at the end), in order.
    pub inserted: BTreeMap<usize, Vec<Call>>,
    /// Strings created by edits; `Sym(original.strings.len() + i)` refers to `strings[i]`.
    pub strings: Vec<String>,
}

/// Result of applying edits.
#[derive(Clone, Debug)]
pub struct EditedCapture {
    pub cap: Capture,
    /// One entry per `cap.frame` command.
    pub origin: Vec<EditOrigin>,
}

impl CaptureEdits {
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.inserted.is_empty()
    }

    /// Interns `s` for use in edited values (reusing an original or previously added string).
    pub fn intern(&mut self, original: &Capture, s: &str) -> Sym {
        if let Some(sym) = original.sym(s) {
            return sym;
        }
        let i = match self.strings.iter().position(|x| x == s) {
            Some(i) => i,
            None => {
                self.strings.push(s.to_owned());
                self.strings.len() - 1
            }
        };
        Sym((original.strings.len() + i) as u32)
    }
}

/// Applies `edits` to `original`. Modified args replace `frame[i].args`; inserted commands are
/// spliced before their original index. Setup and snapshot sections are untouched.
pub fn apply(original: &Capture, edits: &CaptureEdits) -> EditedCapture {
    let mut strings = Vec::with_capacity(original.strings.len() + edits.strings.len());
    strings.extend_from_slice(&original.strings);
    strings.extend_from_slice(&edits.strings);
    let len = original.frame.len()
        + edits
            .inserted
            .range(..=original.frame.len())
            .map(|(_, calls)| calls.len())
            .sum::<usize>();
    let mut frame = Vec::with_capacity(len);
    let mut origin = Vec::with_capacity(len);
    for_each_edited(original, edits, original.frame.len(), |call, o| {
        frame.push(call);
        origin.push(o);
    });
    EditedCapture {
        cap: Capture {
            url: original.url.clone(),
            strings,
            objects: original.objects.clone(),
            blobs: original.blobs.clone(),
            setup: original.setup.clone(),
            snapshot: original.snapshot.clone(),
            frame,
        },
        origin,
    }
}

/// Emits, in edited order, every edited frame command placed before original `frame[end]`
/// (including the commands inserted at `end`; `end >= frame.len()` = the whole edited frame).
fn for_each_edited(
    original: &Capture,
    edits: &CaptureEdits,
    end: usize,
    mut emit: impl FnMut(Call, EditOrigin),
) {
    fn inserted_at(edits: &CaptureEdits, k: usize, emit: &mut impl FnMut(Call, EditOrigin)) {
        for (slot, call) in edits.inserted.get(&k).into_iter().flatten().enumerate() {
            emit(call.clone(), EditOrigin::Inserted { before: k, slot });
        }
    }
    let end = end.min(original.frame.len());
    for (i, call) in original.frame[..end].iter().enumerate() {
        inserted_at(edits, i, &mut emit);
        match edits.modified.get(&i) {
            Some(args) if *args != call.args => emit(
                Call {
                    this: call.this,
                    method: call.method,
                    args: args.clone(),
                    result: call.result.clone(),
                },
                EditOrigin::Modified(i),
            ),
            _ => emit(call.clone(), EditOrigin::Original(i)),
        }
    }
    inserted_at(edits, end, &mut emit);
}

/// Maps an edited frame index back to (origin) and an original index to its edited frame index.
pub fn edited_index_of(origin: &[EditOrigin], original_index: usize) -> Option<usize> {
    origin.iter().position(
        |o| matches!(o, EditOrigin::Original(i) | EditOrigin::Modified(i) if *i == original_index),
    )
}

/// Editable parameter of a known command.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// Floating-point number (with an optional inclusive range for sliders).
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// Integer.
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Bool,
    /// One of named enum values (GL enums / WebGPU numeric enums).
    Enum(&'static [(&'static str, u32)]),
    /// One of named string values (WebGPU string enums, e.g. index format).
    StrEnum(&'static [&'static str]),
    /// GL bitfield made of named bits.
    Bits(&'static [(&'static str, u32)]),
    /// Reference to an existing object of one of these classes.
    Object(&'static [&'static str]),
    /// RGBA color passed as ONE argument (a WebGPU `GPUColor`: Dict `{r, g, b, a}` or an Array
    /// of 4 Numbers). Read it with [`color_components`], build it with [`color_value`].
    Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub kind: ParamKind,
}

/// A command the user can insert or whose arguments get a typed editor.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandTemplate {
    /// Menu label, e.g. "viewport", "enable(DEPTH_TEST)", "setStencilReference".
    pub label: &'static str,
    /// Grouping for menus: "Viewport & scissor", "Depth", "Stencil", "Blend", "Rasterizer", "Color", "Pipeline".
    pub group: &'static str,
    /// Receiver classes this applies to, e.g. ["WebGL2RenderingContext"], ["GPURenderPassEncoder"].
    pub receivers: &'static [&'static str],
    pub method: &'static str,
    /// Specs of the LEADING arguments; extra trailing arguments of a recorded call (e.g.
    /// `setBindGroup` dynamic offsets) are not described and must be kept as they are.
    pub params: Vec<ParamSpec>,
}

/// Components of a `GPUColor` value (Dict `{r, g, b, a}` or Array of 4 Numbers).
pub fn color_components(cap: &Capture, v: &Value) -> Option<[f64; 4]> {
    let mut out = [0.0; 4];
    match v {
        Value::Array(items) if items.len() == 4 => {
            for (o, item) in out.iter_mut().zip(items) {
                *o = item.as_f64()?;
            }
        }
        Value::Dict(_) => {
            for (o, key) in out.iter_mut().zip(["r", "g", "b", "a"]) {
                *o = cap.dict_get(v, key)?.as_f64()?;
            }
        }
        _ => return None,
    }
    Some(out)
}

/// A `GPUColorDict` value `{r, g, b, a}` with keys interned in `edits`.
pub fn color_value(original: &Capture, edits: &mut CaptureEdits, c: [f64; 4]) -> Value {
    Value::Dict(
        ["r", "g", "b", "a"]
            .into_iter()
            .zip(c)
            .map(|(key, v)| (edits.intern(original, key), Value::Number(v)))
            .collect(),
    )
}

// ---- Templates -------------------------------------------------------------------------------

const G_VIEWPORT: &str = "Viewport & scissor";
const G_DEPTH: &str = "Depth";
const G_STENCIL: &str = "Stencil";
const G_BLEND: &str = "Blend";
const G_RASTER: &str = "Rasterizer";
const G_COLOR: &str = "Color";
const G_PIPELINE: &str = "Pipeline";

const GL: &[&str] = &["WebGL2RenderingContext"];
const RENDER_PASS: &[&str] = &["GPURenderPassEncoder"];
const RENDER: &[&str] = &["GPURenderPassEncoder", "GPURenderBundleEncoder"];
const COMPUTE: &[&str] = &["GPUComputePassEncoder"];

const ANY_FLOAT: ParamKind = ParamKind::Float {
    min: None,
    max: None,
};
const UNIT_FLOAT: ParamKind = ParamKind::Float {
    min: Some(0.0),
    max: Some(1.0),
};
const SIZE_FLOAT: ParamKind = ParamKind::Float {
    min: Some(0.0),
    max: None,
};
const ANY_INT: ParamKind = ParamKind::Int {
    min: None,
    max: None,
};
const SIZE_INT: ParamKind = ParamKind::Int {
    min: Some(0),
    max: None,
};
const STENCIL_VALUE: ParamKind = ParamKind::Int {
    min: Some(0),
    max: Some(255),
};
const MASK: ParamKind = ParamKind::Int {
    min: Some(0),
    max: Some(0xFFFF_FFFF),
};

/// GL capabilities for enable/disable.
static CAPS: [(&str, u32); 10] = [
    ("BLEND", 3042),
    ("CULL_FACE", 2884),
    ("DEPTH_TEST", 2929),
    ("DITHER", 3024),
    ("POLYGON_OFFSET_FILL", 32823),
    ("RASTERIZER_DISCARD", 35977),
    ("SAMPLE_ALPHA_TO_COVERAGE", 32926),
    ("SAMPLE_COVERAGE", 32928),
    ("SCISSOR_TEST", 3089),
    ("STENCIL_TEST", 2960),
];
/// Per `CAPS` entry: (group, enable label, disable label).
static CAP_TEMPLATES: [(&str, &str, &str); 10] = [
    (G_BLEND, "enable(BLEND)", "disable(BLEND)"),
    (G_RASTER, "enable(CULL_FACE)", "disable(CULL_FACE)"),
    (G_DEPTH, "enable(DEPTH_TEST)", "disable(DEPTH_TEST)"),
    (G_COLOR, "enable(DITHER)", "disable(DITHER)"),
    (
        G_RASTER,
        "enable(POLYGON_OFFSET_FILL)",
        "disable(POLYGON_OFFSET_FILL)",
    ),
    (
        G_RASTER,
        "enable(RASTERIZER_DISCARD)",
        "disable(RASTERIZER_DISCARD)",
    ),
    (
        G_RASTER,
        "enable(SAMPLE_ALPHA_TO_COVERAGE)",
        "disable(SAMPLE_ALPHA_TO_COVERAGE)",
    ),
    (
        G_RASTER,
        "enable(SAMPLE_COVERAGE)",
        "disable(SAMPLE_COVERAGE)",
    ),
    (G_VIEWPORT, "enable(SCISSOR_TEST)", "disable(SCISSOR_TEST)"),
    (G_STENCIL, "enable(STENCIL_TEST)", "disable(STENCIL_TEST)"),
];
const COMPARE_FUNCS: &[(&str, u32)] = &[
    ("NEVER", 512),
    ("LESS", 513),
    ("EQUAL", 514),
    ("LEQUAL", 515),
    ("GREATER", 516),
    ("NOTEQUAL", 517),
    ("GEQUAL", 518),
    ("ALWAYS", 519),
];
const FACES: &[(&str, u32)] = &[("FRONT", 1028), ("BACK", 1029), ("FRONT_AND_BACK", 1032)];
const FRONT_FACES: &[(&str, u32)] = &[("CW", 2304), ("CCW", 2305)];
const BLEND_FACTORS: &[(&str, u32)] = &[
    ("ZERO", 0),
    ("ONE", 1),
    ("SRC_COLOR", 768),
    ("ONE_MINUS_SRC_COLOR", 769),
    ("SRC_ALPHA", 770),
    ("ONE_MINUS_SRC_ALPHA", 771),
    ("DST_ALPHA", 772),
    ("ONE_MINUS_DST_ALPHA", 773),
    ("DST_COLOR", 774),
    ("ONE_MINUS_DST_COLOR", 775),
    ("SRC_ALPHA_SATURATE", 776),
    ("CONSTANT_COLOR", 32769),
    ("ONE_MINUS_CONSTANT_COLOR", 32770),
    ("CONSTANT_ALPHA", 32771),
    ("ONE_MINUS_CONSTANT_ALPHA", 32772),
];
const BLEND_EQUATIONS: &[(&str, u32)] = &[
    ("FUNC_ADD", 32774),
    ("FUNC_SUBTRACT", 32778),
    ("FUNC_REVERSE_SUBTRACT", 32779),
    ("MIN", 32775),
    ("MAX", 32776),
];
const STENCIL_OPS: &[(&str, u32)] = &[
    ("KEEP", 7680),
    ("ZERO", 0),
    ("REPLACE", 7681),
    ("INCR", 7682),
    ("DECR", 7683),
    ("INVERT", 5386),
    ("INCR_WRAP", 34055),
    ("DECR_WRAP", 34056),
];

fn p(name: &'static str, kind: ParamKind) -> ParamSpec {
    ParamSpec { name, kind }
}

/// Template whose label is its method name.
fn t(
    method: &'static str,
    group: &'static str,
    receivers: &'static [&'static str],
    params: Vec<ParamSpec>,
) -> CommandTemplate {
    CommandTemplate {
        label: method,
        group,
        receivers,
        method,
        params,
    }
}

fn rgba(kind: ParamKind) -> Vec<ParamSpec> {
    ["red", "green", "blue", "alpha"]
        .into_iter()
        .map(|name| p(name, kind.clone()))
        .collect()
}

fn gl_templates() -> Vec<CommandTemplate> {
    use ParamKind::{Bool, Enum, Object};
    let rect = || {
        vec![
            p("x", ANY_INT),
            p("y", ANY_INT),
            p("width", SIZE_INT),
            p("height", SIZE_INT),
        ]
    };
    let mut v = vec![
        t("viewport", G_VIEWPORT, GL, rect()),
        t("scissor", G_VIEWPORT, GL, rect()),
    ];
    for (i, &(group, enable, disable)) in CAP_TEMPLATES.iter().enumerate() {
        for (label, method) in [(enable, "enable"), (disable, "disable")] {
            v.push(CommandTemplate {
                label,
                group,
                receivers: GL,
                method,
                params: vec![p("cap", Enum(&CAPS[i..=i]))],
            });
        }
    }
    let face = || p("face", Enum(FACES));
    let stencil_op = || {
        vec![
            p("fail", Enum(STENCIL_OPS)),
            p("zfail", Enum(STENCIL_OPS)),
            p("zpass", Enum(STENCIL_OPS)),
        ]
    };
    let stencil_func = || {
        vec![
            p("func", Enum(COMPARE_FUNCS)),
            p("ref", STENCIL_VALUE),
            p("mask", MASK),
        ]
    };
    v.extend([
        t("enable", G_RASTER, GL, vec![p("cap", Enum(&CAPS))]),
        t("disable", G_RASTER, GL, vec![p("cap", Enum(&CAPS))]),
        t(
            "depthFunc",
            G_DEPTH,
            GL,
            vec![p("func", Enum(COMPARE_FUNCS))],
        ),
        t("depthMask", G_DEPTH, GL, vec![p("flag", Bool)]),
        t(
            "depthRange",
            G_DEPTH,
            GL,
            vec![p("zNear", UNIT_FLOAT), p("zFar", UNIT_FLOAT)],
        ),
        t("clearDepth", G_DEPTH, GL, vec![p("depth", UNIT_FLOAT)]),
        t("stencilFunc", G_STENCIL, GL, stencil_func()),
        t(
            "stencilFuncSeparate",
            G_STENCIL,
            GL,
            std::iter::once(face()).chain(stencil_func()).collect(),
        ),
        t("stencilOp", G_STENCIL, GL, stencil_op()),
        t(
            "stencilOpSeparate",
            G_STENCIL,
            GL,
            std::iter::once(face()).chain(stencil_op()).collect(),
        ),
        t("stencilMask", G_STENCIL, GL, vec![p("mask", MASK)]),
        t(
            "stencilMaskSeparate",
            G_STENCIL,
            GL,
            vec![face(), p("mask", MASK)],
        ),
        t("clearStencil", G_STENCIL, GL, vec![p("s", STENCIL_VALUE)]),
        t(
            "blendFunc",
            G_BLEND,
            GL,
            vec![
                p("sfactor", Enum(BLEND_FACTORS)),
                p("dfactor", Enum(BLEND_FACTORS)),
            ],
        ),
        t(
            "blendFuncSeparate",
            G_BLEND,
            GL,
            vec![
                p("srcRGB", Enum(BLEND_FACTORS)),
                p("dstRGB", Enum(BLEND_FACTORS)),
                p("srcAlpha", Enum(BLEND_FACTORS)),
                p("dstAlpha", Enum(BLEND_FACTORS)),
            ],
        ),
        t(
            "blendEquation",
            G_BLEND,
            GL,
            vec![p("mode", Enum(BLEND_EQUATIONS))],
        ),
        t(
            "blendEquationSeparate",
            G_BLEND,
            GL,
            vec![
                p("modeRGB", Enum(BLEND_EQUATIONS)),
                p("modeAlpha", Enum(BLEND_EQUATIONS)),
            ],
        ),
        t("blendColor", G_BLEND, GL, rgba(UNIT_FLOAT)),
        t("cullFace", G_RASTER, GL, vec![p("mode", Enum(FACES))]),
        t(
            "frontFace",
            G_RASTER,
            GL,
            vec![p("mode", Enum(FRONT_FACES))],
        ),
        t(
            "polygonOffset",
            G_RASTER,
            GL,
            vec![p("factor", ANY_FLOAT), p("units", ANY_FLOAT)],
        ),
        t("lineWidth", G_RASTER, GL, vec![p("width", SIZE_FLOAT)]),
        t("colorMask", G_COLOR, GL, rgba(Bool)),
        t("clearColor", G_COLOR, GL, rgba(ANY_FLOAT)),
        t(
            "useProgram",
            G_PIPELINE,
            GL,
            vec![p("program", Object(&["WebGLProgram"]))],
        ),
        t(
            "bindVertexArray",
            G_PIPELINE,
            GL,
            vec![p("vertexArray", Object(&["WebGLVertexArrayObject"]))],
        ),
    ]);
    v
}

/// Render pass templates; `bundle` keeps only the commands valid on render bundle encoders.
fn gpu_render_templates(bundle: bool) -> Vec<CommandTemplate> {
    let mut v = Vec::new();
    if !bundle {
        v.extend([
            t(
                "setViewport",
                G_VIEWPORT,
                RENDER_PASS,
                vec![
                    p("x", ANY_FLOAT),
                    p("y", ANY_FLOAT),
                    p("width", SIZE_FLOAT),
                    p("height", SIZE_FLOAT),
                    p("minDepth", UNIT_FLOAT),
                    p("maxDepth", UNIT_FLOAT),
                ],
            ),
            t(
                "setScissorRect",
                G_VIEWPORT,
                RENDER_PASS,
                vec![
                    p("x", SIZE_INT),
                    p("y", SIZE_INT),
                    p("width", SIZE_INT),
                    p("height", SIZE_INT),
                ],
            ),
            t(
                "setBlendConstant",
                G_BLEND,
                RENDER_PASS,
                vec![p("color", ParamKind::Color)],
            ),
            t(
                "setStencilReference",
                G_STENCIL,
                RENDER_PASS,
                vec![p("reference", STENCIL_VALUE)],
            ),
        ]);
    }
    v.extend([
        t(
            "setPipeline",
            G_PIPELINE,
            RENDER,
            vec![p("pipeline", ParamKind::Object(&["GPURenderPipeline"]))],
        ),
        t(
            "setBindGroup",
            G_PIPELINE,
            RENDER,
            vec![
                p("index", SIZE_INT),
                p("bindGroup", ParamKind::Object(&["GPUBindGroup"])),
            ],
        ),
    ]);
    v
}

fn gpu_compute_templates() -> Vec<CommandTemplate> {
    vec![
        t(
            "setPipeline",
            G_PIPELINE,
            COMPUTE,
            vec![p("pipeline", ParamKind::Object(&["GPUComputePipeline"]))],
        ),
        t(
            "setBindGroup",
            G_PIPELINE,
            COMPUTE,
            vec![
                p("index", SIZE_INT),
                p("bindGroup", ParamKind::Object(&["GPUBindGroup"])),
            ],
        ),
    ]
}

/// Every insertable template for a receiver class (empty for classes without dynamic state).
pub fn templates_for(receiver_class: &str) -> Vec<CommandTemplate> {
    match receiver_class {
        "WebGL2RenderingContext" => gl_templates(),
        "GPURenderPassEncoder" => gpu_render_templates(false),
        "GPURenderBundleEncoder" => gpu_render_templates(true),
        "GPUComputePassEncoder" => gpu_compute_templates(),
        _ => Vec::new(),
    }
}

/// Typed parameter specs for an existing call (`None` → the UI falls back to a generic value editor).
pub fn params_for_call(cap: &Capture, call: &Call) -> Option<Vec<ParamSpec>> {
    let method = cap.method(call);
    // `label == method` picks the generic enable/disable over the fixed-cap ones.
    templates_for(cap.class_of(call.this))
        .into_iter()
        .find(|t| t.method == method && t.label == method)
        .map(|t| t.params)
}

// ---- Defaults --------------------------------------------------------------------------------

// `webgd:glState` pnames.
const VIEWPORT: u32 = 0x0BA2;
const SCISSOR_BOX: u32 = 0x0C10;
const DEPTH_FUNC: u32 = 0x0B74;
const DEPTH_WRITEMASK: u32 = 0x0B72;
const DEPTH_RANGE: u32 = 0x0B70;
const DEPTH_CLEAR_VALUE: u32 = 0x0B73;
const COLOR_WRITEMASK: u32 = 0x0C23;
const COLOR_CLEAR_VALUE: u32 = 0x0C22;
const CULL_FACE_MODE: u32 = 0x0B45;
const FRONT_FACE: u32 = 0x0B46;
const LINE_WIDTH: u32 = 0x0B21;
const POLYGON_OFFSET_FACTOR: u32 = 0x8038;
const POLYGON_OFFSET_UNITS: u32 = 0x2A00;
const BLEND_COLOR: u32 = 0x8005;
const BLEND_EQUATION_RGB: u32 = 0x8009;
const BLEND_EQUATION_ALPHA: u32 = 0x883D;
const BLEND_SRC_RGB: u32 = 0x80C9;
const BLEND_DST_RGB: u32 = 0x80C8;
const BLEND_SRC_ALPHA: u32 = 0x80CB;
const BLEND_DST_ALPHA: u32 = 0x80CA;
const STENCIL_FUNC: u32 = 0x0B92;
const STENCIL_REF: u32 = 0x0B97;
const STENCIL_VALUE_MASK: u32 = 0x0B93;
const STENCIL_FAIL: u32 = 0x0B94;
const STENCIL_PASS_DEPTH_FAIL: u32 = 0x0B95;
const STENCIL_PASS_DEPTH_PASS: u32 = 0x0B96;
const STENCIL_WRITEMASK: u32 = 0x0B98;
const STENCIL_BACK_FUNC: u32 = 0x8800;
const STENCIL_BACK_FAIL: u32 = 0x8801;
const STENCIL_BACK_PASS_DEPTH_FAIL: u32 = 0x8802;
const STENCIL_BACK_PASS_DEPTH_PASS: u32 = 0x8803;
const STENCIL_BACK_REF: u32 = 0x8CA3;
const STENCIL_BACK_VALUE_MASK: u32 = 0x8CA4;
const STENCIL_BACK_WRITEMASK: u32 = 0x8CA5;
const STENCIL_CLEAR_VALUE: u32 = 0x0B91;
const CURRENT_PROGRAM: u32 = 0x8B8D;
const VERTEX_ARRAY_BINDING: u32 = 0x85B5;

const FRONT: u32 = 1028;
const BACK: u32 = 1029;
const FRONT_AND_BACK: u32 = 1032;
const LESS: f64 = 513.0;
const ALWAYS: f64 = 519.0;
const CCW: f64 = 2305.0;
const ONE: f64 = 1.0;
const ZERO: f64 = 0.0;
const FUNC_ADD: f64 = 32774.0;
const KEEP: f64 = 7680.0;
const MASK_ALL: f64 = 4_294_967_295.0;

/// GL setters whose arguments together form one multi-value state: (method, pname, fallback).
const GL_VECTOR_STATE: &[(&str, u32, &[f64])] = &[
    ("depthRange", DEPTH_RANGE, &[0.0, 1.0]),
    ("colorMask", COLOR_WRITEMASK, &[1.0; 4]),
    ("clearColor", COLOR_CLEAR_VALUE, &[0.0; 4]),
    ("blendColor", BLEND_COLOR, &[0.0; 4]),
];

/// GL setters whose i-th argument is the i-th state: (method, [(pname, fallback)]).
/// `blendFunc` / `blendEquation` set both RGB and alpha state when tracked.
const GL_SCALAR_STATE: &[(&str, &[(u32, f64)])] = &[
    ("depthFunc", &[(DEPTH_FUNC, LESS)]),
    ("depthMask", &[(DEPTH_WRITEMASK, 1.0)]),
    ("clearDepth", &[(DEPTH_CLEAR_VALUE, 1.0)]),
    ("cullFace", &[(CULL_FACE_MODE, BACK as f64)]),
    ("frontFace", &[(FRONT_FACE, CCW)]),
    ("lineWidth", &[(LINE_WIDTH, 1.0)]),
    (
        "polygonOffset",
        &[(POLYGON_OFFSET_FACTOR, 0.0), (POLYGON_OFFSET_UNITS, 0.0)],
    ),
    ("blendEquation", &[(BLEND_EQUATION_RGB, FUNC_ADD)]),
    (
        "blendEquationSeparate",
        &[
            (BLEND_EQUATION_RGB, FUNC_ADD),
            (BLEND_EQUATION_ALPHA, FUNC_ADD),
        ],
    ),
    ("blendFunc", &[(BLEND_SRC_RGB, ONE), (BLEND_DST_RGB, ZERO)]),
    (
        "blendFuncSeparate",
        &[
            (BLEND_SRC_RGB, ONE),
            (BLEND_DST_RGB, ZERO),
            (BLEND_SRC_ALPHA, ONE),
            (BLEND_DST_ALPHA, ZERO),
        ],
    ),
    ("clearStencil", &[(STENCIL_CLEAR_VALUE, 0.0)]),
];

/// Front / back pnames and fallbacks of stencilFunc, stencilOp and stencilMask.
const STENCIL_FUNC_KEYS: [&[u32]; 2] = [
    &[STENCIL_FUNC, STENCIL_REF, STENCIL_VALUE_MASK],
    &[STENCIL_BACK_FUNC, STENCIL_BACK_REF, STENCIL_BACK_VALUE_MASK],
];
const STENCIL_FUNC_DEFAULTS: &[f64] = &[ALWAYS, 0.0, MASK_ALL];
const STENCIL_OP_KEYS: [&[u32]; 2] = [
    &[
        STENCIL_FAIL,
        STENCIL_PASS_DEPTH_FAIL,
        STENCIL_PASS_DEPTH_PASS,
    ],
    &[
        STENCIL_BACK_FAIL,
        STENCIL_BACK_PASS_DEPTH_FAIL,
        STENCIL_BACK_PASS_DEPTH_PASS,
    ],
];
const STENCIL_OP_DEFAULTS: &[f64] = &[KEEP; 3];
const STENCIL_MASK_KEYS: [&[u32]; 2] = [&[STENCIL_WRITEMASK], &[STENCIL_BACK_WRITEMASK]];
const STENCIL_MASK_DEFAULTS: &[f64] = &[MASK_ALL];

/// (front/back pnames, fallbacks) of a stencil setter, `*Separate` or not.
fn stencil_state(method: &str) -> Option<([&'static [u32]; 2], &'static [f64])> {
    match method.strip_suffix("Separate").unwrap_or(method) {
        "stencilFunc" => Some((STENCIL_FUNC_KEYS, STENCIL_FUNC_DEFAULTS)),
        "stencilOp" => Some((STENCIL_OP_KEYS, STENCIL_OP_DEFAULTS)),
        "stencilMask" => Some((STENCIL_MASK_KEYS, STENCIL_MASK_DEFAULTS)),
        _ => None,
    }
}

/// Indices into front/back key pairs affected by a GL face enum.
fn sides(face: f64) -> &'static [usize] {
    match face as u32 {
        FRONT => &[0],
        BACK => &[1],
        FRONT_AND_BACK => &[0, 1],
        _ => &[],
    }
}

/// Numbers of a Number / Bool / Array / typed array value.
fn num_list(cap: &Capture, v: &Value) -> Vec<f64> {
    match v {
        Value::Number(_) | Value::Bool(_) => v.as_f64().into_iter().collect(),
        Value::Array(items) => items.iter().filter_map(Value::as_f64).collect(),
        Value::Typed { kind, blob } => cap
            .blob(*blob)
            .chunks_exact(kind.element_size())
            .filter_map(|c| {
                Some(match kind {
                    TypedKind::Int8 => c[0] as i8 as f64,
                    TypedKind::Uint8 | TypedKind::Uint8Clamped => c[0] as f64,
                    TypedKind::Int16 => i16::from_le_bytes(c.try_into().ok()?) as f64,
                    TypedKind::Uint16 => u16::from_le_bytes(c.try_into().ok()?) as f64,
                    TypedKind::Int32 => i32::from_le_bytes(c.try_into().ok()?) as f64,
                    TypedKind::Uint32 => u32::from_le_bytes(c.try_into().ok()?) as f64,
                    TypedKind::Float32 => f32::from_le_bytes(c.try_into().ok()?) as f64,
                    TypedKind::Float64 => f64::from_le_bytes(c.try_into().ok()?),
                    _ => return None,
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Dynamic GL state of one context, keyed by `getParameter` pname.
#[derive(Default)]
struct GlDynamicState {
    nums: HashMap<u32, Vec<f64>>,
    /// Object bindings; `ObjId(0)` = null.
    objs: HashMap<u32, ObjId>,
}

impl GlDynamicState {
    /// State of `ctx` after the snapshot `webgd:glState` and every `prefix.frame` call on it.
    fn at(original: &Capture, prefix: &Capture, ctx: ObjId) -> Self {
        let mut st = Self::default();
        let state = original
            .snapshot
            .iter()
            .find(|c| c.this == ctx && original.method(c) == "webgd:glState")
            .and_then(|c| c.args.first());
        if let Some(Value::Dict(entries)) = state {
            for (k, v) in entries {
                let Ok(key) = original.str(*k).parse::<u32>() else {
                    continue;
                };
                match v {
                    Value::ObjRef(id) => {
                        st.objs.insert(key, *id);
                    }
                    Value::Null => {
                        st.objs.insert(key, ObjId(0));
                    }
                    _ => {
                        let l = num_list(original, v);
                        if !l.is_empty() {
                            st.nums.insert(key, l);
                        }
                    }
                }
            }
        }
        for call in prefix.frame.iter().filter(|c| c.this == ctx) {
            st.track(prefix, call);
        }
        st
    }

    fn set(&mut self, key: u32, v: f64) {
        self.nums.insert(key, vec![v]);
    }

    fn track(&mut self, cap: &Capture, call: &Call) {
        let a = &call.args;
        let n = |i: usize| a.get(i).and_then(Value::as_f64);
        let all = |r: std::ops::Range<usize>| -> Option<Vec<f64>> {
            a.get(r)?.iter().map(Value::as_f64).collect()
        };
        let m = cap.method(call);
        match m {
            "viewport" | "scissor" => {
                if let Some(v) = all(0..4) {
                    self.nums.insert(
                        if m == "viewport" {
                            VIEWPORT
                        } else {
                            SCISSOR_BOX
                        },
                        v,
                    );
                }
            }
            "enable" | "disable" => {
                if let Some(c) = n(0) {
                    self.set(c as u32, if m == "enable" { 1.0 } else { 0.0 });
                }
            }
            "blendFunc" => {
                if let (Some(s), Some(d)) = (n(0), n(1)) {
                    self.set(BLEND_SRC_RGB, s);
                    self.set(BLEND_SRC_ALPHA, s);
                    self.set(BLEND_DST_RGB, d);
                    self.set(BLEND_DST_ALPHA, d);
                }
            }
            "blendEquation" => {
                if let Some(e) = n(0) {
                    self.set(BLEND_EQUATION_RGB, e);
                    self.set(BLEND_EQUATION_ALPHA, e);
                }
            }
            "useProgram" | "bindVertexArray" => {
                let key = if m == "useProgram" {
                    CURRENT_PROGRAM
                } else {
                    VERTEX_ARRAY_BINDING
                };
                let id = a.first().and_then(Value::as_obj).unwrap_or_default();
                self.objs.insert(key, id);
            }
            _ => {
                if let Some(&(_, key, fallback)) = GL_VECTOR_STATE.iter().find(|e| e.0 == m) {
                    if let Some(v) = all(0..fallback.len()) {
                        self.nums.insert(key, v);
                    }
                } else if let Some(&(_, keys)) = GL_SCALAR_STATE.iter().find(|e| e.0 == m) {
                    for (i, &(key, _)) in keys.iter().enumerate() {
                        if let Some(v) = n(i) {
                            self.set(key, v);
                        }
                    }
                } else if let Some((keys, _)) = stencil_state(m) {
                    let separate = m.ends_with("Separate");
                    let start = usize::from(separate);
                    let face = if separate {
                        n(0)
                    } else {
                        Some(f64::from(FRONT_AND_BACK))
                    };
                    let (Some(face), Some(vals)) = (face, all(start..start + keys[0].len())) else {
                        return;
                    };
                    for &side in sides(face) {
                        for (&key, &v) in keys[side].iter().zip(&vals) {
                            self.set(key, v);
                        }
                    }
                }
            }
        }
    }

    /// Multi-value state `key` if it has exactly `len` values.
    fn vector(&self, key: u32, len: usize) -> Option<Vec<f64>> {
        self.nums.get(&key).filter(|v| v.len() == len).cloned()
    }

    /// First value of each state key, else its fallback.
    fn scalars(&self, keys: impl IntoIterator<Item = (u32, f64)>) -> Vec<f64> {
        keys.into_iter()
            .map(|(key, fallback)| {
                self.nums
                    .get(&key)
                    .and_then(|v| v.first().copied())
                    .unwrap_or(fallback)
            })
            .collect()
    }
}

/// `width`/`height` Numbers of a Dict.
fn dict_size(cap: &Capture, d: &Value) -> Option<(f64, f64)> {
    Some((
        cap.dict_get(d, "width")?.as_f64()?,
        cap.dict_get(d, "height")?.as_f64()?,
    ))
}

/// The canvas a context (GL context or `GPUCanvasContext`) was obtained from via `getContext`.
fn canvas_of(original: &Capture, ctx: ObjId) -> Option<ObjId> {
    let (section, i) = creation_call(original, ctx)?;
    Some(original.section(section)[i].this).filter(|id| id.0 != 0)
}

/// Canvas size at the insertion point: setup/snapshot sizing, then frame resizes (default 300×150).
fn canvas_size(original: &Capture, prefix: &Capture, canvas: Option<ObjId>) -> (f64, f64) {
    fn resize(cap: &Capture, c: &Call, size: &mut (f64, f64)) {
        let n = |i: usize| c.args.get(i).and_then(Value::as_f64);
        match cap.method(c) {
            "set:width" => {
                if let Some(w) = n(0) {
                    size.0 = w;
                }
            }
            "set:height" => {
                if let Some(h) = n(0) {
                    size.1 = h;
                }
            }
            "webgd:canvasSize" => {
                if let (Some(w), Some(h)) = (n(0), n(1)) {
                    *size = (w, h);
                }
            }
            _ => {}
        }
    }
    let mut size = (300.0, 150.0);
    let Some(canvas) = canvas else { return size };
    for c in original.setup.iter().chain(&original.snapshot) {
        if c.this == canvas {
            resize(original, c, &mut size);
        }
    }
    for c in prefix.frame.iter().filter(|c| c.this == canvas) {
        resize(prefix, c, &mut size);
    }
    size
}

/// GL drawing buffer size: snapshot `webgd:glDefaultFramebuffer`, else the canvas size.
fn gl_drawing_buffer_size(original: &Capture, prefix: &Capture, ctx: ObjId) -> (f64, f64) {
    original
        .snapshot
        .iter()
        .find(|c| c.this == ctx && original.method(c) == "webgd:glDefaultFramebuffer")
        .and_then(|c| dict_size(original, c.args.first()?))
        .unwrap_or_else(|| canvas_size(original, prefix, canvas_of(original, ctx)))
}

/// Mip-0 width/height of a `GPUTexture`.
fn gpu_texture_size(original: &Capture, prefix: &Capture, tex: ObjId) -> Option<(f64, f64)> {
    if let Some((section, i)) = creation_call(original, tex) {
        let c = &original.section(section)[i];
        match original.method(c) {
            "createTexture" => {
                if let Some(size) = c.args.first().and_then(|d| original.dict_get(d, "size")) {
                    let dim = |i: usize, key: &str| match size {
                        Value::Array(a) => a.get(i).and_then(Value::as_f64),
                        _ => original.dict_get(size, key).and_then(Value::as_f64),
                    };
                    if let Some(w) = dim(0, "width") {
                        return Some((w, dim(1, "height").unwrap_or(1.0)));
                    }
                }
            }
            "getCurrentTexture" => {
                return Some(canvas_size(original, prefix, canvas_of(original, c.this)));
            }
            _ => {}
        }
    }
    // Pre-frame contents record: mip 0 subresource.
    original
        .snapshot
        .iter()
        .filter(|c| c.this == tex && original.method(c) == "webgd:gpuTexture")
        .filter_map(|c| c.args.first()?.as_array())
        .flatten()
        .find(|sub| original.dict_get(sub, "mip").and_then(Value::as_f64) == Some(0.0))
        .and_then(|sub| dict_size(original, sub))
}

/// Size of the mip a render attachment `view` (GPUTextureView, or GPUTexture) refers to.
fn gpu_view_size(original: &Capture, prefix: &Capture, view: ObjId) -> Option<(f64, f64)> {
    let (tex, mip) = if original.class_of(view) == "GPUTexture" {
        (view, 0.0)
    } else {
        let creation = creation_call(original, view).map(|(s, i)| &original.section(s)[i]);
        let tex = original
            .objects
            .get(&view)
            .and_then(|o| o.parent)
            .or(creation.map(|c| c.this))?;
        let mip = creation
            .and_then(|c| c.args.first())
            .and_then(|d| original.dict_get(d, "baseMipLevel"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        (tex, mip)
    };
    let (w, h) = gpu_texture_size(original, prefix, tex)?;
    let scale = 2f64.powf(mip);
    Some(((w / scale).floor().max(1.0), (h / scale).floor().max(1.0)))
}

/// Render target size from a `beginRenderPass` descriptor (first color attachment, else the
/// depth/stencil attachment); 1×1 when unknown.
fn render_pass_size(original: &Capture, prefix: &Capture, desc: &Value) -> (f64, f64) {
    let color = original
        .dict_get(desc, "colorAttachments")
        .and_then(Value::as_array)
        .and_then(|atts| {
            atts.iter()
                .find_map(|att| original.dict_get(att, "view")?.as_obj())
        });
    let depth = || {
        let att = original.dict_get(desc, "depthStencilAttachment")?;
        original.dict_get(att, "view")?.as_obj()
    };
    color
        .or_else(depth)
        .and_then(|view| gpu_view_size(original, prefix, view))
        .unwrap_or((1.0, 1.0))
}

fn first_object(original: &Capture, classes: &[&str]) -> Option<ObjId> {
    original
        .objects
        .values()
        .find(|o| classes.contains(&original.str(o.class)))
        .map(|o| o.id)
}

/// `current` (if bound) else the first object of `classes`, else Null.
fn object_or_first(original: &Capture, current: Option<ObjId>, classes: &[&str]) -> Value {
    current
        .filter(|id| id.0 != 0)
        .or_else(|| first_object(original, classes))
        .map_or(Value::Null, Value::ObjRef)
}

fn object_classes(template: &CommandTemplate, i: usize) -> &'static [&'static str] {
    match template.params.get(i).map(|p| &p.kind) {
        Some(ParamKind::Object(classes)) => classes,
        _ => &[],
    }
}

fn number_values(params: &[ParamSpec], nums: &[f64]) -> Vec<Value> {
    params
        .iter()
        .zip(nums)
        .map(|(p, &v)| match p.kind {
            ParamKind::Bool => Value::Bool(v != 0.0),
            _ => Value::Number(v),
        })
        .collect()
}

fn gl_defaults(
    original: &Capture,
    prefix: &Capture,
    ctx: ObjId,
    template: &CommandTemplate,
) -> Option<Vec<Value>> {
    let st = GlDynamicState::at(original, prefix, ctx);
    let m = template.method;
    let nums: Vec<f64> = if matches!(m, "viewport" | "scissor") {
        let key = if m == "viewport" {
            VIEWPORT
        } else {
            SCISSOR_BOX
        };
        st.vector(key, 4).unwrap_or_else(|| {
            let (w, h) = gl_drawing_buffer_size(original, prefix, ctx);
            vec![0.0, 0.0, w, h]
        })
    } else if matches!(m, "enable" | "disable") {
        // Fixed-cap templates carry a single-entry Enum.
        let ParamKind::Enum(caps) = &template.params.first()?.kind else {
            return None;
        };
        vec![f64::from(caps.first()?.1)]
    } else if matches!(m, "useProgram" | "bindVertexArray") {
        let key = if m == "useProgram" {
            CURRENT_PROGRAM
        } else {
            VERTEX_ARRAY_BINDING
        };
        let current = st.objs.get(&key).copied();
        return Some(vec![object_or_first(
            original,
            current,
            object_classes(template, 0),
        )]);
    } else if let Some(&(_, key, fallback)) = GL_VECTOR_STATE.iter().find(|e| e.0 == m) {
        st.vector(key, fallback.len())
            .unwrap_or_else(|| fallback.to_vec())
    } else if let Some(&(_, keys)) = GL_SCALAR_STATE.iter().find(|e| e.0 == m) {
        st.scalars(keys.iter().copied())
    } else if let Some((keys, fallback)) = stencil_state(m) {
        let front = st.scalars(keys[0].iter().copied().zip(fallback.iter().copied()));
        if m.ends_with("Separate") {
            std::iter::once(f64::from(FRONT_AND_BACK))
                .chain(front)
                .collect()
        } else {
            front
        }
    } else {
        return None;
    };
    Some(number_values(&template.params, &nums))
}

fn gpu_defaults(
    original: &Capture,
    edits: &mut CaptureEdits,
    prefix: &mut Capture,
    receiver: ObjId,
    template: &CommandTemplate,
) -> Option<Vec<Value>> {
    let pass = if matches!(
        original.class_of(receiver),
        "GPURenderPassEncoder" | "GPUComputePassEncoder"
    ) {
        // A no-op probe call (unknown method) on the receiver makes `state_at` track that pass.
        prefix.frame.push(Call {
            this: receiver,
            method: Sym(u32::MAX),
            args: Vec::new(),
            result: Value::Undefined,
        });
        let st = state_at(prefix, prefix.frame.len() - 1).pass;
        prefix.frame.pop();
        st.filter(|st| st.pass == receiver)
    } else {
        None
    };
    // Render bundle encoders have no pass state: the last matching call on the receiver.
    let last_call = |method: &str, pick: &dyn Fn(&[Value]) -> Option<ObjId>| {
        prefix
            .frame
            .iter()
            .rev()
            .filter(|c| c.this == receiver && prefix.method(c) == method)
            .find_map(|c| pick(&c.args))
    };
    let size = || {
        let desc = pass.as_ref().map_or(&Value::Undefined, |st| &st.desc);
        render_pass_size(original, prefix, desc)
    };
    let numbers = |v: Vec<f64>| v.into_iter().map(Value::Number).collect::<Vec<_>>();
    Some(match template.method {
        "setViewport" => numbers(
            pass.as_ref()
                .and_then(|st| st.viewport)
                .map(Vec::from)
                .unwrap_or_else(|| {
                    let (w, h) = size();
                    vec![0.0, 0.0, w, h, 0.0, 1.0]
                }),
        ),
        "setScissorRect" => numbers(
            pass.as_ref()
                .and_then(|st| st.scissor)
                .map(Vec::from)
                .unwrap_or_else(|| {
                    let (w, h) = size();
                    vec![0.0, 0.0, w, h]
                }),
        ),
        "setStencilReference" => numbers(vec![
            pass.as_ref().and_then(|st| st.stencil_ref).unwrap_or(0.0),
        ]),
        "setBlendConstant" => vec![
            pass.as_ref()
                .and_then(|st| st.blend_constant.clone())
                .unwrap_or_else(|| color_value(original, edits, [0.0; 4])),
        ],
        "setPipeline" => {
            let current = match &pass {
                Some(st) => st.pipeline,
                None => last_call("setPipeline", &|a| a.first()?.as_obj()),
            };
            vec![object_or_first(
                original,
                current,
                object_classes(template, 0),
            )]
        }
        "setBindGroup" => {
            let current = match &pass {
                Some(st) => st.bind_groups.get(&0).map(|b| b.0),
                None => last_call("setBindGroup", &|a| {
                    (a.first()?.as_f64()? == 0.0)
                        .then(|| a.get(1)?.as_obj())
                        .flatten()
                }),
            };
            vec![
                Value::Number(0.0),
                object_or_first(original, current, object_classes(template, 1)),
            ]
        }
        _ => return None,
    })
}

/// Generic default of a parameter kind (zero clamped into range, first enum entry, first object).
fn kind_default(original: &Capture, edits: &mut CaptureEdits, kind: &ParamKind) -> Value {
    match kind {
        ParamKind::Float { min, max } => Value::Number(
            0f64.max(min.unwrap_or(f64::MIN))
                .min(max.unwrap_or(f64::MAX)),
        ),
        ParamKind::Int { min, max } => Value::Number(
            0i64.max(min.unwrap_or(i64::MIN))
                .min(max.unwrap_or(i64::MAX)) as f64,
        ),
        ParamKind::Bool => Value::Bool(false),
        ParamKind::Enum(entries) => Value::Number(entries.first().map_or(0.0, |e| f64::from(e.1))),
        ParamKind::StrEnum(names) => names.first().map_or(Value::Undefined, |s| {
            Value::String(edits.intern(original, s))
        }),
        ParamKind::Bits(_) => Value::Number(0.0),
        ParamKind::Object(classes) => object_or_first(original, None, classes),
        ParamKind::Color => color_value(original, edits, [0.0; 4]),
    }
}

/// Default argument values for a template, taken from the GL state / pass state in effect at
/// `before_original_index` when known (e.g. current viewport), else sensible defaults.
///
/// The state includes earlier edits: modified commands and commands already inserted at or
/// before `before_original_index` (a new command is appended after those).
pub fn default_args(
    original: &Capture,
    edits: &mut CaptureEdits,
    template: &CommandTemplate,
    receiver: crate::ObjId,
    before_original_index: usize,
) -> Vec<Value> {
    let mut prefix = Capture {
        strings: original
            .strings
            .iter()
            .chain(&edits.strings)
            .cloned()
            .collect(),
        objects: original.objects.clone(),
        ..Capture::default()
    };
    for_each_edited(original, edits, before_original_index, |call, _| {
        prefix.frame.push(call)
    });
    let known = match original.class_of(receiver) {
        "WebGL2RenderingContext" => gl_defaults(original, &prefix, receiver, template),
        "GPURenderPassEncoder" | "GPUComputePassEncoder" | "GPURenderBundleEncoder" => {
            gpu_defaults(original, edits, &mut prefix, receiver, template)
        }
        _ => None,
    };
    match known {
        Some(args) if args.len() == template.params.len() => args,
        _ => template
            .params
            .iter()
            .map(|p| kind_default(original, edits, &p.kind))
            .collect(),
    }
}
