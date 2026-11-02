use webgd_analysis::{AnalysisError, Stage, glsl_stats, occupancy, wgsl_stats};

// ---- WGSL ----

#[test]
fn wgsl_sin_alu() {
    let src = "@fragment fn fs(@location(0) x: f32) -> @location(0) vec4f { return vec4f(sin(x), 0, 0, 1); }";
    let s = wgsl_stats(src, None, Stage::Fragment).unwrap();
    assert_eq!(s.alu, 4.0);
    assert_eq!(s.branches, 0.0);
    assert_eq!(s.tex_samples, 0.0);
    assert_eq!(s.workgroup_size, None);
}

#[test]
fn wgsl_loop() {
    let src = "@fragment fn fs(@location(0) x: f32) -> @location(0) vec4f {
        var a = x;
        loop { a = a + 1.0; continuing { break if a > x; } }
        return vec4f(a);
    }";
    let s = wgsl_stats(src, None, Stage::Fragment).unwrap();
    assert!(s.alu >= 8.0, "{s:?}");
    assert!(s.branches >= 8.0, "{s:?}");
}

#[test]
fn wgsl_for_loop() {
    let src = "@fragment fn fs(@location(0) x: f32) -> @location(0) vec4f {
        var a = x;
        for (var i = 0; i < 4; i++) { a += 1.0; }
        return vec4f(a);
    }";
    let s = wgsl_stats(src, None, Stage::Fragment).unwrap();
    assert!(s.alu >= 8.0, "{s:?}");
    assert!(s.branches >= 8.0, "{s:?}");
}

#[test]
fn wgsl_if() {
    let src = "@fragment fn fs(@location(0) x: f32) -> @location(0) vec4f {
        var a = x;
        if x > 0.5 { a = 2.0; }
        return vec4f(a);
    }";
    let s = wgsl_stats(src, None, Stage::Fragment).unwrap();
    assert_eq!(s.branches, 1.0);
}

#[test]
fn wgsl_texture_sample() {
    let src = "@group(0) @binding(0) var t: texture_2d<f32>;
        @group(0) @binding(1) var s: sampler;
        @fragment fn fs(@location(0) uv: vec2f) -> @location(0) vec4f { return textureSample(t, s, uv); }";
    let s = wgsl_stats(src, None, Stage::Fragment).unwrap();
    assert_eq!(s.tex_samples, 1.0);
}

#[test]
fn wgsl_compute_workgroup() {
    let src = "var<workgroup> a: array<f32, 4096>;
        @compute @workgroup_size(256) fn cs(@builtin(local_invocation_index) i: u32) { a[i] = 1.0; }";
    let s = wgsl_stats(src, None, Stage::Compute).unwrap();
    assert_eq!(s.workgroup_bytes, 16384);
    assert_eq!(s.workgroup_size, Some([256, 1, 1]));
    assert_eq!(occupancy(&s), 1.0);
}

#[test]
fn wgsl_matrix_and_calls() {
    let src = "struct U { m: mat4x4f }
        @group(0) @binding(0) var<uniform> u: U;
        fn f(v: vec4f) -> vec4f { return u.m * v; }
        @vertex fn vs(@location(0) p: vec4f) -> @builtin(position) vec4f { return f(p) + f(p); }";
    let s = wgsl_stats(src, None, Stage::Vertex).unwrap();
    // Callee inlined per call (16 each) plus one vec4 add.
    assert_eq!(s.alu, 36.0);
}

#[test]
fn wgsl_entry_selection() {
    let src = "@fragment fn a() -> @location(0) vec4f { return vec4f(1); }
        @fragment fn b() -> @location(0) vec4f { return vec4f(sin(1.5)); }";
    assert_eq!(wgsl_stats(src, None, Stage::Fragment), Err(AnalysisError::AmbiguousEntry));
    assert_eq!(wgsl_stats(src, None, Stage::Vertex), Err(AnalysisError::NoEntry));
    assert_eq!(wgsl_stats(src, Some("c"), Stage::Fragment), Err(AnalysisError::NoEntry));
    assert!(wgsl_stats(src, Some("b"), Stage::Fragment).is_ok());
    assert!(matches!(wgsl_stats("fn (", None, Stage::Fragment), Err(AnalysisError::Parse(_))));
}

// ---- GLSL ES 3.00 ----

const FS_HEAD: &str = "#version 300 es\nprecision mediump float;\nin float x;\nin vec2 uv;\nout vec4 o;\n";

fn fs(body: &str) -> String {
    format!("{FS_HEAD}{body}")
}

#[test]
fn glsl_sin_alu() {
    let s = glsl_stats(&fs("void main() { o = vec4(sin(x), 0.0, 0.0, 1.0); }"), Stage::Fragment).unwrap();
    assert_eq!(s.alu, 4.0);
    assert_eq!(s.branches, 0.0);
    assert_eq!(s.tex_samples, 0.0);
}

#[test]
fn glsl_loop() {
    let src = fs("void main() { float a = x; for (int i = 0; i < 4; i++) { a += 1.0; } o = vec4(a); }");
    let s = glsl_stats(&src, Stage::Fragment).unwrap();
    assert!(s.alu >= 8.0, "{s:?}");
    assert!(s.branches >= 8.0, "{s:?}");
}

#[test]
fn glsl_if() {
    let src = fs("void main() { float a = x; if (x > 0.5) { a = 2.0; } o = vec4(a); }");
    let s = glsl_stats(&src, Stage::Fragment).unwrap();
    assert_eq!(s.branches, 1.0);
}

#[test]
fn glsl_texture_sample() {
    let src = fs("uniform sampler2D t;\nvoid main() { o = texture(t, uv); }");
    let s = glsl_stats(&src, Stage::Fragment).unwrap();
    assert_eq!(s.tex_samples, 1.0);
}

#[test]
fn glsl_compute_workgroup() {
    let src = "#version 310 es\nlayout(local_size_x = 256) in;\nshared float a[4096];\nvoid main() { a[gl_LocalInvocationIndex] = 1.0; }";
    let s = glsl_stats(src, Stage::Compute).unwrap();
    assert_eq!(s.workgroup_bytes, 16384);
    assert_eq!(s.workgroup_size, Some([256, 1, 1]));
    assert_eq!(occupancy(&s), 1.0);
}

#[test]
fn glsl_matrix_ubo_and_calls() {
    let src = "#version 300 es
        layout(std140) uniform Globals { mat4 mvp; vec4 tint; };
        in vec3 pos;
        vec4 f(vec4 v) { return mvp * v; }
        void main() { gl_Position = f(vec4(pos, 1.0)) + f(vec4(pos, 1.0)) * tint; }";
    let s = glsl_stats(src, Stage::Vertex).unwrap();
    // Two inlined mat*vec (16 each), one vec4 mul, one vec4 add.
    assert_eq!(s.alu, 40.0);
    // pos (3 components) -> ceil(3/4) + 2.
    assert_eq!(s.est_regs_vec4, 3);
}

#[test]
fn glsl_parse_error() {
    assert!(matches!(glsl_stats("void main( {", Stage::Fragment), Err(AnalysisError::Parse(_))));
    assert_eq!(glsl_stats("void f() {}", Stage::Fragment), Err(AnalysisError::NoEntry));
}

#[test]
fn occupancy_register_bound() {
    let s = webgd_analysis::StageStats { est_regs_vec4: 16, ..Default::default() };
    // floor(512 / 64) = 8 waves of 16.
    assert_eq!(occupancy(&s), 0.5);
}
