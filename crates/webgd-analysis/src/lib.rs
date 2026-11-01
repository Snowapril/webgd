//! Shader static analysis and the estimated-counter model.
pub mod model;

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

pub fn wgsl_stats(src: &str, entry: Option<&str>, stage: Stage) -> Result<StageStats, AnalysisError> {
    let _ = (src, entry, stage);
    todo!("analysis agent")
}

pub fn glsl_stats(src: &str, stage: Stage) -> Result<StageStats, AnalysisError> {
    let _ = (src, stage);
    todo!("analysis agent")
}

pub use model::occupancy;
