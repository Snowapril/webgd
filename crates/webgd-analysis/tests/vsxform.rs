use webgd_analysis::AnalysisError;
use webgd_analysis::vsxform::{
    ScalarKind, VsCompute, VsInput, VsOutput, glsl_vertex_outputs, wgsl_vertex_to_compute,
};

const WEBGPU_PAGE: &str = include_str!("../../../tests/pages/webgpu.html");
const WEBGL2_PAGE: &str = include_str!("../../../tests/pages/webgl2.html");

/// Text between the first `start` and the following `end`.
fn between<'a>(hay: &'a str, start: &str, end: &str) -> &'a str {
    let from = hay.find(start).expect("start marker") + start.len();
    let len = hay[from..].find(end).expect("end marker");
    &hay[from..from + len]
}

/// The generated WGSL must parse and validate on its own and expose the IO contract.
fn check_wgsl(c: &VsCompute) {
    let module = naga::front::wgsl::parse_str(&c.wgsl)
        .unwrap_or_else(|e| panic!("{}\n{}", e.emit_to_string(&c.wgsl), c.wgsl));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{}\n{}", e.emit_to_string(&c.wgsl), c.wgsl));
    assert_eq!(module.entry_points.len(), 1);
    let ep = &module.entry_points[0];
    assert_eq!(ep.stage, naga::ShaderStage::Compute);
    assert_eq!(ep.name, c.entry);
    assert_eq!(ep.workgroup_size, [64, 1, 1]);
    let mut io: Vec<(u32, &str)> = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| {
            let b = g.binding.as_ref()?;
            (b.group == c.io_group).then(|| (b.binding, g.name.as_deref().unwrap_or("")))
        })
        .collect();
    io.sort();
    assert_eq!(
        io,
        vec![
            (0, "webgd_in"),
            (1, "webgd_ids"),
            (2, "webgd_params"),
            (3, "webgd_out")
        ]
    );
}

fn input(location: u32, components: u32, kind: ScalarKind) -> VsInput {
    VsInput {
        location,
        components,
        kind,
    }
}

fn output(name: &str, location: Option<u32>, components: u32, kind: ScalarKind) -> VsOutput {
    VsOutput {
        name: name.to_string(),
        builtin_position: name == "position" || name == "gl_Position",
        location,
        components,
        kind,
    }
}

#[test]
fn fixture_cube_vs() {
    let src = between(WEBGPU_PAGE, "label: 'cube', code: /* wgsl */ `", "` });");
    let c = wgsl_vertex_to_compute(src, None).unwrap();
    assert_eq!(c.io_group, 1);
    assert_eq!(
        c.inputs,
        vec![
            input(0, 3, ScalarKind::Float),
            input(1, 2, ScalarKind::Float)
        ]
    );
    assert_eq!(
        c.outputs,
        vec![
            output("position", None, 4, ScalarKind::Float),
            output("uv", Some(0), 2, ScalarKind::Float),
        ]
    );
    assert_eq!(c.used_groups, vec![(0, vec![0])]);
    check_wgsl(&c);
    // Named entry selection gives the same result.
    assert_eq!(wgsl_vertex_to_compute(src, Some("vs")).unwrap(), c);
}

#[test]
fn plain_args_vertex_index_and_struct_output() {
    let src = "
        struct Out {
            @location(1) color: vec4f,
            @builtin(position) pos: vec4f,
            @location(0) @interpolate(flat) id: u32,
        };
        @vertex fn main(@location(2) w: i32, @builtin(vertex_index) vi: u32, @location(0) p: vec2f) -> Out {
            var o: Out;
            o.pos = vec4f(p, f32(w), 1.0);
            o.color = vec4f(f32(vi));
            o.id = vi;
            return o;
        }";
    let c = wgsl_vertex_to_compute(src, None).unwrap();
    assert_eq!(c.io_group, 0);
    assert_eq!(
        c.inputs,
        vec![
            input(0, 2, ScalarKind::Float),
            input(2, 1, ScalarKind::Sint)
        ]
    );
    assert_eq!(
        c.outputs,
        vec![
            output("position", None, 4, ScalarKind::Float),
            output("id", Some(0), 1, ScalarKind::Uint),
            output("color", Some(1), 4, ScalarKind::Float),
        ]
    );
    assert!(c.used_groups.is_empty());
    check_wgsl(&c);
}

#[test]
fn fixture_composite_vs_without_location_inputs() {
    let src = between(
        WEBGPU_PAGE,
        "label: 'composite', code: /* wgsl */ `",
        "` });",
    );
    let c = wgsl_vertex_to_compute(src, None).unwrap();
    assert_eq!(c.io_group, 1);
    assert!(c.inputs.is_empty());
    assert_eq!(
        c.outputs,
        vec![
            output("position", None, 4, ScalarKind::Float),
            output("uv", Some(0), 2, ScalarKind::Float),
        ]
    );
    // The fragment-only texture/sampler are not used by the transformed code.
    assert!(c.used_groups.is_empty());
    check_wgsl(&c);
    // `webgd_in` stays statically referenced (declaration + use) for auto layouts.
    assert!(c.wgsl.matches("webgd_in").count() >= 2, "{}", c.wgsl);
}

#[test]
fn entry_selection_errors() {
    let src = "
        @vertex fn a(@location(0) p: vec4f) -> @builtin(position) vec4f { return p; }
        @vertex fn b(@location(0) p: vec4f) -> @builtin(position) vec4f { return p * 2.0; }
        @fragment fn f() -> @location(0) vec4f { return vec4f(1.0); }";
    assert_eq!(
        wgsl_vertex_to_compute(src, None),
        Err(AnalysisError::AmbiguousEntry)
    );
    assert_eq!(
        wgsl_vertex_to_compute(src, Some("f")),
        Err(AnalysisError::NoEntry)
    );
    let c = wgsl_vertex_to_compute(src, Some("b")).unwrap();
    assert_eq!(c.inputs, vec![input(0, 4, ScalarKind::Float)]);
    assert_eq!(
        c.outputs,
        vec![output("position", None, 4, ScalarKind::Float)]
    );
    check_wgsl(&c);
}

#[test]
fn parse_error() {
    assert!(matches!(
        wgsl_vertex_to_compute("fn (", None),
        Err(AnalysisError::Parse(_))
    ));
}

#[test]
fn glsl_fixture_outputs() {
    let cube_vs = between(WEBGL2_PAGE, "const cubeProg = program(`", "`, `");
    assert_eq!(
        glsl_vertex_outputs(cube_vs).unwrap(),
        vec![
            output("gl_Position", None, 4, ScalarKind::Float),
            output("vUv", None, 2, ScalarKind::Float),
        ]
    );
    let comp_vs = between(WEBGL2_PAGE, "const compProg = program(`", "`, `");
    assert_eq!(
        glsl_vertex_outputs(comp_vs).unwrap(),
        vec![
            output("gl_Position", None, 4, ScalarKind::Float),
            output("vUv", None, 2, ScalarKind::Float),
        ]
    );
}

#[test]
fn glsl_output_kinds_and_skips() {
    let src = "#version 300 es
        in vec4 p;
        flat out int a;
        flat out uvec3 b, c[2];
        out mat2 m;
        out float f[3];
        smooth out vec4 v;
        void main() { gl_Position = p; }";
    assert_eq!(
        glsl_vertex_outputs(src).unwrap(),
        vec![
            output("gl_Position", None, 4, ScalarKind::Float),
            output("a", None, 1, ScalarKind::Sint),
            output("b", None, 3, ScalarKind::Uint),
            output("v", None, 4, ScalarKind::Float),
        ]
    );
}
