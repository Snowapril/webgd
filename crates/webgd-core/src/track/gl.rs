//! WebGL2 texture dimension tracker.
use crate::capture::{Capture, ObjId};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct TexDims {
    /// TEXTURE_2D / TEXTURE_CUBE_MAP / TEXTURE_3D / TEXTURE_2D_ARRAY
    pub target: u32,
    pub internalformat: u32,
    pub format: Option<u32>,
    pub ty: Option<u32>,
    pub immutable: bool,
    /// (level, face 0..6; 0 for non-cube) → (width, height, depth)
    pub levels: BTreeMap<(u32, u32), (u32, u32, u32)>,
}

/// Dims of GL texture `id` as of `frame[index]` (inclusive). Starts from the snapshot
/// `webgd:glTexture` record and the setup log, then applies frame texImage*/texStorage* calls.
pub fn texture_dims(cap: &Capture, index: usize, id: ObjId) -> Option<TexDims> {
    let _ = (cap, index, id);
    todo!("core agent")
}
