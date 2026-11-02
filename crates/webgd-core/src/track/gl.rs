//! WebGL2 texture dimension tracker.
use crate::capture::{Call, Capture, ObjId, Value};
use std::collections::{BTreeMap, HashMap};

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

pub const TEXTURE_2D: u32 = 0x0DE1;
pub const TEXTURE_CUBE_MAP: u32 = 0x8513;
pub const TEXTURE_CUBE_MAP_POSITIVE_X: u32 = 0x8515;
pub const TEXTURE_3D: u32 = 0x806F;
pub const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const TEXTURE0: u32 = 0x84C0;
const ACTIVE_TEXTURE: &str = "34016";
/// Pseudo texture unit used by setup-log `webgd:bindForUpload`.
const UPLOAD_UNIT: u32 = u32::MAX;

/// Upload/bind target → (bind target, cube face).
fn bind_target(target: u32) -> (u32, u32) {
    if (TEXTURE_CUBE_MAP_POSITIVE_X..TEXTURE_CUBE_MAP_POSITIVE_X + 6).contains(&target) {
        (TEXTURE_CUBE_MAP, target - TEXTURE_CUBE_MAP_POSITIVE_X)
    } else {
        (target, 0)
    }
}

#[derive(Default)]
struct Tracker {
    active: HashMap<ObjId, u32>,
    bound: HashMap<(ObjId, u32, u32), ObjId>,
    dims: Option<TexDims>,
}

impl Tracker {
    fn bound_tex(&self, ctx: ObjId, target: u32) -> Option<(ObjId, u32, u32)> {
        let (bt, face) = bind_target(target);
        let unit = self.active.get(&ctx).copied().unwrap_or(0);
        self.bound.get(&(ctx, unit, bt)).map(|&t| (t, bt, face))
    }

    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        bt: u32,
        face: u32,
        level: u32,
        ifmt: u32,
        size: (u32, u32, u32),
        fmt_ty: Option<(u32, u32)>,
        immutable: bool,
    ) {
        let d = self.dims.get_or_insert_with(|| TexDims {
            target: bt,
            internalformat: ifmt,
            format: None,
            ty: None,
            immutable: false,
            levels: BTreeMap::new(),
        });
        d.target = bt;
        d.internalformat = ifmt;
        d.format = fmt_ty.map(|f| f.0);
        d.ty = fmt_ty.map(|f| f.1);
        d.immutable |= immutable;
        d.levels.insert((level, face), size);
    }

    fn apply(&mut self, cap: &Capture, call: &Call, id: ObjId) {
        let a = &call.args;
        let n = |i: usize| a.get(i).and_then(Value::as_f64).map(|v| v as u32);
        let ctx = call.this;
        match cap.method(call) {
            "activeTexture" => {
                if let Some(e) = n(0) {
                    self.active.insert(ctx, e.wrapping_sub(TEXTURE0));
                }
            }
            "bindTexture" | "webgd:bindForUpload" => {
                let Some(t) = n(0) else { return };
                let unit = if cap.method(call) == "bindTexture" {
                    self.active.get(&ctx).copied().unwrap_or(0)
                } else {
                    self.active.insert(ctx, UPLOAD_UNIT);
                    UPLOAD_UNIT
                };
                match a.get(1).and_then(Value::as_obj) {
                    Some(tex) => self.bound.insert((ctx, unit, t), tex),
                    None => self.bound.remove(&(ctx, unit, t)),
                };
            }
            m @ ("texImage2D"
            | "texImage3D"
            | "copyTexImage2D"
            | "compressedTexImage2D"
            | "compressedTexImage3D"
            | "texStorage2D"
            | "texStorage3D") => {
                let Some(target) = n(0) else { return };
                let Some((tex, bt, face)) = self.bound_tex(ctx, target) else {
                    return;
                };
                if tex != id {
                    return;
                }
                let (Some(l), Some(ifmt)) = (n(1), n(2)) else {
                    return;
                };
                match m {
                    "texImage2D" if a.len() == 6 => {
                        // (target, level, internalformat, format, type, source)
                        let Some(Value::Image { width, height, .. }) = a.get(5) else {
                            return;
                        };
                        let ft = n(3).zip(n(4));
                        self.define(bt, face, l, ifmt, (*width, *height, 1), ft, false);
                    }
                    "texImage2D" => {
                        let (Some(w), Some(h)) = (n(3), n(4)) else {
                            return;
                        };
                        self.define(bt, face, l, ifmt, (w, h, 1), n(6).zip(n(7)), false);
                    }
                    "texImage3D" => {
                        let (Some(w), Some(h), Some(d)) = (n(3), n(4), n(5)) else {
                            return;
                        };
                        self.define(bt, face, l, ifmt, (w, h, d), n(7).zip(n(8)), false);
                    }
                    "copyTexImage2D" => {
                        let (Some(w), Some(h)) = (n(5), n(6)) else {
                            return;
                        };
                        self.define(bt, face, l, ifmt, (w, h, 1), None, false);
                    }
                    "compressedTexImage2D" => {
                        let (Some(w), Some(h)) = (n(3), n(4)) else {
                            return;
                        };
                        self.define(bt, face, l, ifmt, (w, h, 1), None, false);
                    }
                    "compressedTexImage3D" => {
                        let (Some(w), Some(h), Some(d)) = (n(3), n(4), n(5)) else {
                            return;
                        };
                        self.define(bt, face, l, ifmt, (w, h, d), None, false);
                    }
                    _ => {
                        // texStorage2D(target, levels, ifmt, w, h) / texStorage3D(..., d)
                        let (Some(w), Some(h)) = (n(3), n(4)) else {
                            return;
                        };
                        let d = if m == "texStorage3D" {
                            n(5).unwrap_or(1)
                        } else {
                            1
                        };
                        if let Some(old) = &mut self.dims {
                            old.levels.clear();
                        }
                        let faces = if bt == TEXTURE_CUBE_MAP { 6 } else { 1 };
                        let (mut w, mut h, mut d) = (w, h, d);
                        for level in 0..l.max(1) {
                            for f in 0..faces {
                                self.define(bt, f, level, ifmt, (w, h, d), None, true);
                            }
                            w = (w / 2).max(1);
                            h = (h / 2).max(1);
                            if bt == TEXTURE_3D {
                                d = (d / 2).max(1);
                            }
                        }
                    }
                }
            }
            "generateMipmap" => {
                let Some(target) = n(0) else { return };
                let Some((tex, bt, _)) = self.bound_tex(ctx, target) else {
                    return;
                };
                let Some(d) = self.dims.as_mut().filter(|_| tex == id) else {
                    return;
                };
                let faces = if bt == TEXTURE_CUBE_MAP { 6 } else { 1 };
                for f in 0..faces {
                    let Some(&(mut w, mut h, mut z)) = d.levels.get(&(0, f)) else {
                        continue;
                    };
                    let mut level = 0;
                    while w > 1 || h > 1 || (bt == TEXTURE_3D && z > 1) {
                        level += 1;
                        w = (w / 2).max(1);
                        h = (h / 2).max(1);
                        if bt == TEXTURE_3D {
                            z = (z / 2).max(1);
                        }
                        d.levels.entry((level, f)).or_insert((w, h, z));
                    }
                }
            }
            _ => {}
        }
    }

    /// Frame-start bindings from a `webgd:glState` snapshot record.
    fn load_state(&mut self, cap: &Capture, ctx: ObjId, state: &Value) {
        self.bound.retain(|k, _| k.0 != ctx);
        let active = cap
            .dict_get(state, ACTIVE_TEXTURE)
            .and_then(Value::as_f64)
            .map_or(0, |e| (e as u32).wrapping_sub(TEXTURE0));
        self.active.insert(ctx, active);
        let Some(units) = cap.dict_get(state, "units").and_then(Value::as_array) else {
            return;
        };
        for (u, unit) in units.iter().enumerate() {
            for (key, target) in [
                ("2d", TEXTURE_2D),
                ("cube", TEXTURE_CUBE_MAP),
                ("3d", TEXTURE_3D),
                ("array", TEXTURE_2D_ARRAY),
            ] {
                if let Some(tex) = cap.dict_get(unit, key).and_then(Value::as_obj) {
                    self.bound.insert((ctx, u as u32, target), tex);
                }
            }
        }
    }
}

/// Dims from a snapshot `webgd:glTexture` Dict.
pub fn snapshot_dims(cap: &Capture, desc: &Value) -> Option<TexDims> {
    let num = |k: &str| {
        cap.dict_get(desc, k)
            .and_then(Value::as_f64)
            .map(|v| v as u32)
    };
    let mut levels = BTreeMap::new();
    for lv in cap
        .dict_get(desc, "levels")
        .and_then(Value::as_array)
        .unwrap_or(&[])
    {
        let ln = |k: &str| {
            cap.dict_get(lv, k)
                .and_then(Value::as_f64)
                .map(|v| v as u32)
        };
        let (Some(level), Some(w), Some(h)) = (ln("level"), ln("width"), ln("height")) else {
            continue;
        };
        levels.insert(
            (level, ln("face").unwrap_or(0)),
            (w, h, ln("depth").unwrap_or(1)),
        );
    }
    Some(TexDims {
        target: num("target")?,
        internalformat: num("internalformat").unwrap_or(0),
        format: num("format"),
        ty: num("type"),
        immutable: cap
            .dict_get(desc, "immutable")
            .and_then(Value::as_f64)
            .is_some_and(|v| v != 0.0),
        levels,
    })
}

/// Dims of GL texture `id` as of `frame[index]` (inclusive). Starts from the snapshot
/// `webgd:glTexture` record and the setup log, then applies frame texImage*/texStorage* calls.
pub fn texture_dims(cap: &Capture, index: usize, id: ObjId) -> Option<TexDims> {
    let mut t = Tracker::default();
    for call in &cap.setup {
        t.apply(cap, call, id);
    }
    for call in &cap.snapshot {
        match cap.method(call) {
            "webgd:glTexture" if call.args.first().and_then(Value::as_obj) == Some(id) => {
                if let Some(d) = call.args.get(1).and_then(|v| snapshot_dims(cap, v)) {
                    t.dims = Some(d);
                }
            }
            "webgd:glState" => {
                if let Some(s) = call.args.first() {
                    t.load_state(cap, call.this, s);
                }
            }
            _ => {}
        }
    }
    let end = index.saturating_add(1).min(cap.frame.len());
    for call in &cap.frame[..end] {
        t.apply(cap, call, id);
    }
    t.dims
}
