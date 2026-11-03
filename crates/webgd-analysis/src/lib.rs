//! Shader static analysis and the estimated-counter model.
mod glsl;
pub mod model;
pub mod vsxform;
mod wgsl;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageStats {
    pub alu: f64,
    pub branches: f64,
    pub tex_samples: f64,
    pub est_regs_vec4: u32,
    pub workgroup_size: Option<[u32; 3]>,
    pub workgroup_bytes: u32,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum AnalysisError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("validation error: {0}")]
    Validate(String),
    #[error("no entry point for stage")]
    NoEntry,
    #[error("ambiguous entry point")]
    AmbiguousEntry,
}

/// Static stats for a WGSL entry point. `entry = None` picks the single entry
/// point of `stage` ([`AnalysisError::AmbiguousEntry`] if there are several).
pub fn wgsl_stats(
    src: &str,
    entry: Option<&str>,
    stage: Stage,
) -> Result<StageStats, AnalysisError> {
    wgsl::stats(src, entry, stage)
}

/// Static stats for a GLSL (ES 3.00) shader; the entry point is `main`.
pub fn glsl_stats(src: &str, stage: Stage) -> Result<StageStats, AnalysisError> {
    glsl::stats(src, stage)
}

pub use model::occupancy;
