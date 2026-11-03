//! GLSL front ends run a real preprocessor: only the active `#if`/`#ifdef` branches are parsed and
//! macros (object- and function-like) are expanded, as in three.js-generated shaders.
use webgd_analysis::vsxform::{ScalarKind, VsOutput, glsl_vertex_outputs};
use webgd_analysis::{AnalysisError, Stage, glsl_stats};

/// three.js-style fragment shader. The inactive branches declare conflicting variables and contain
/// token sequences that are not valid GLSL, so parsing fails unless they are dropped.
const THREE_FS: &str = "#version 300 es
#extension GL_OES_standard_derivatives : enable
#define USE_CLEARCOAT_NORMALMAP
#define NUM_LIGHTS 2
#define saturate(a) clamp(a, 0.0, 1.0)
#ifndef GL_ES
#error GL_ES must be predefined
#endif
#if __VERSION__ != 300
#error __VERSION__ must follow #version
#endif
precision highp float;
uniform sampler2D map;
uniform sampler2D clearcoatNormalMap;
in vec2 vUv;
#ifdef USE_CLEARCOAT_NORMALMAP
in vec2 vClearcoatNormalMapUv;
#else
in vec3 vClearcoatNormalMapUv;
#endif
out vec4 fragColor;
void main() {
    vec4 c = texture(map, vUv);
#ifdef USE_CLEARCOAT_NORMALMAP
    vec3 n = texture(clearcoatNormalMap, vClearcoatNormalMapUv).xyz;
#else
    vec3 n = vClearcoatNormalMapUv texture(map, vUv).xyz;
#endif
#if defined(NUM_LIGHTS) && NUM_LIGHTS > 1
    c.rgb += saturate(n);
#else
    c.rgb = texture(map, vUv).rgb texture(map, vUv).rgb;
#endif
#if NUM_LIGHTS > 5
    c += texture(map, vUv);
#endif
    fragColor = c;
}
";

/// `THREE_FS` preprocessed by hand.
const THREE_FS_EXPANDED: &str = "#version 300 es
precision highp float;
uniform sampler2D map;
uniform sampler2D clearcoatNormalMap;
in vec2 vUv;
in vec2 vClearcoatNormalMapUv;
out vec4 fragColor;
void main() {
    vec4 c = texture(map, vUv);
    vec3 n = texture(clearcoatNormalMap, vClearcoatNormalMapUv).xyz;
    c.rgb += clamp(n, 0.0, 1.0);
    fragColor = c;
}
";

#[test]
fn fragment_uses_active_branches_and_expands_macros() {
    let s = glsl_stats(THREE_FS, Stage::Fragment).unwrap();
    assert_eq!(s.tex_samples, 2.0, "{s:?}");
    assert_eq!(s, glsl_stats(THREE_FS_EXPANDED, Stage::Fragment).unwrap());
}

/// three.js-style vertex shader: GLSL 1 keyword macros, an extension unknown to the preprocessor
/// (a warning, not an error), a pragma and `out` variables declared only in some branches.
const THREE_VS: &str = "#version 300 es
#extension GL_ANGLE_multi_draw : require
#define USE_UV
#define USE_BATCHING_COLOR 0
#define attribute in
#define varying out
#define MAT_SCALE(m, s) ((m) * (s))
attribute vec3 position;
attribute vec2 uv;
uniform mat4 mvp;
#ifdef USE_UV
varying vec2 vUv;
#endif
#ifdef USE_CLEARCOAT_NORMALMAP
varying vec2 vClearcoatNormalMapUv;
#else
flat out int vNoClearcoat;
#endif
#if defined(USE_BATCHING_COLOR) && USE_BATCHING_COLOR > 0
varying vec3 vBatchColor;
#endif
#pragma optimize(on)
void main() {
#ifdef USE_UV
    vUv = uv;
#endif
#ifndef USE_CLEARCOAT_NORMALMAP
    vNoClearcoat = 1;
#endif
    gl_Position = MAT_SCALE(mvp, 1.0) * vec4(position, 1.0);
}
";

fn output(name: &str, components: u32, kind: ScalarKind) -> VsOutput {
    VsOutput {
        name: name.to_string(),
        builtin_position: name == "gl_Position",
        location: None,
        components,
        kind,
    }
}

#[test]
fn vertex_outputs_from_active_branches() {
    assert_eq!(
        glsl_vertex_outputs(THREE_VS).unwrap(),
        vec![
            output("gl_Position", 4, ScalarKind::Float),
            output("vUv", 2, ScalarKind::Float),
            output("vNoClearcoat", 1, ScalarKind::Sint),
        ]
    );
    assert!(glsl_stats(THREE_VS, Stage::Vertex).is_ok());
}

#[test]
fn parse_error_lines_follow_the_original_source() {
    let src = "#version 300 es\n#define FOO 1\n#ifdef FOO\nvoid main( {\n#endif\n";
    let Err(AnalysisError::Parse(msg)) = glsl_stats(src, Stage::Fragment) else {
        panic!("expected a parse error");
    };
    assert!(msg.starts_with("4:"), "{msg}");
}

#[test]
fn unexpected_end_of_input_is_a_parse_error() {
    let src = "#version 300 es\nprecision mediump float;\nvoid main() {\n";
    assert!(matches!(
        glsl_stats(src, Stage::Fragment),
        Err(AnalysisError::Parse(_))
    ));
    assert!(matches!(
        glsl_vertex_outputs(src),
        Err(AnalysisError::Parse(_))
    ));
}

#[test]
fn error_directive_in_active_branch_fails() {
    let src = "#version 300 es\n#ifdef GL_ES\n#error unsupported\n#endif\nvoid main() {}\n";
    assert!(matches!(
        glsl_stats(src, Stage::Fragment),
        Err(AnalysisError::Parse(_))
    ));
}
