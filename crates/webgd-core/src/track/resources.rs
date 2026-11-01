//! Resource browser rows.
use crate::capture::{Capture, ObjId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    PreFrame,
    Frame(usize),
}

#[derive(Clone, PartialEq, Debug)]
pub struct ResourceRow {
    pub id: ObjId,
    pub class: String,
    pub label: String,
    pub summary: String,
    pub origin: Origin,
}

pub const RESOURCE_CLASSES: &[&str] = &[
    "WebGLBuffer", "WebGLTexture", "WebGLRenderbuffer", "WebGLFramebuffer", "WebGLShader",
    "WebGLProgram", "WebGLSampler", "WebGLVertexArrayObject", "WebGLQuery",
    "GPUBuffer", "GPUTexture", "GPUTextureView", "GPUSampler", "GPUShaderModule", "GPUBindGroup",
    "GPUBindGroupLayout", "GPUPipelineLayout", "GPURenderPipeline", "GPUComputePipeline",
    "GPUQuerySet", "GPURenderBundle",
];

pub fn list(cap: &Capture) -> Vec<ResourceRow> {
    let _ = cap;
    todo!("core agent")
}
