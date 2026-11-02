//! WebGPU inspection driven by the pure pass-state tracker.
use crate::util::{creation_call, creation_desc, get_num, value_tree};
use crate::{BindingRow, Inspection, ReplayState, StateNode, TargetView, js};
use webgd_core::display::{obj_label, value_label};
use webgd_core::track::gpu::{PassState, state_at};
use webgd_core::{Capture, ObjId, Value};

fn leaf(name: &str, value: String) -> StateNode {
    StateNode::Leaf {
        name: name.to_string(),
        value,
    }
}

/// (texture, base mip, base layer) of a texture view.
fn view_target(cap: &Capture, view: ObjId) -> Option<(ObjId, u32, u32)> {
    let tex = cap.objects.get(&view)?.parent?;
    if cap.class_of(tex) != "GPUTexture" {
        return None;
    }
    let desc = creation_desc(cap, view);
    let g = |k: &str| desc.and_then(|d| get_num(cap, d, k)).unwrap_or(0.0) as u32;
    Some((tex, g("baseMipLevel"), g("baseArrayLayer")))
}

async fn read_view(st: &ReplayState, cap: &Capture, label: String, view: ObjId) -> TargetView {
    let Some((tex, mip, layer)) = view_target(cap, view) else {
        return TargetView {
            label,
            resource: Some(view),
            image: None,
            note: Some("view texture unknown".into()),
        };
    };
    let (image, note) = match st.read_texture(cap, tex, mip, layer).await {
        Ok(i) => (Some(i), None),
        Err(crate::ReplayError::Js { message, .. }) => (None, Some(message)),
        Err(e) => (None, Some(e.to_string())),
    };
    TargetView {
        label,
        resource: Some(tex),
        image,
        note,
    }
}

fn visibility(v: u32) -> String {
    let mut s = Vec::new();
    for (bit, name) in [(1, "VERTEX"), (2, "FRAGMENT"), (4, "COMPUTE")] {
        if v & bit != 0 {
            s.push(name);
        }
    }
    s.join("|")
}

/// Layout entry summary for `binding` of a bind group layout.
fn layout_detail(cap: &Capture, layout: Option<ObjId>, binding: u32) -> String {
    let Some(layout) = layout else {
        return String::new();
    };
    let Some(call) = creation_call(cap, layout) else {
        return String::new();
    };
    if cap.method(call) == "getBindGroupLayout" {
        return "auto layout".into();
    }
    let entries = call
        .args
        .first()
        .and_then(|d| cap.dict_get(d, "entries"))
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for e in entries {
        if get_num(cap, e, "binding").map(|b| b as u32) != Some(binding) {
            continue;
        }
        let vis = visibility(get_num(cap, e, "visibility").unwrap_or(0.0) as u32);
        for kind in [
            "buffer",
            "sampler",
            "texture",
            "storageTexture",
            "externalTexture",
        ] {
            if let Some(d) = cap.dict_get(e, kind) {
                let ty = ["type", "sampleType", "access"]
                    .iter()
                    .find_map(|k| match cap.dict_get(d, k) {
                        Some(Value::String(s)) => Some(cap.str(*s).to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                return format!("{vis} {kind} {ty}").trim().to_string();
            }
        }
        return vis;
    }
    String::new()
}

pub async fn inspect(st: &ReplayState, cap: &Capture, index: usize) -> Inspection {
    let mut ins = Inspection {
        index,
        targets: Vec::new(),
        pipeline: Vec::new(),
        bindings: Vec::new(),
        notes: Vec::new(),
    };
    if let Some(c) = cap.frame.get(index)
        && cap.class_of(c.this) == "GPURenderBundleEncoder"
    {
        ins.notes
            .push("bundle recording — select executeBundles".into());
        return ins;
    }
    let Some(ps) = state_at(cap, index).pass else {
        ins.notes.push("not inside a render or compute pass".into());
        return ins;
    };

    // Targets.
    if !ps.compute {
        let colors = cap
            .dict_get(&ps.desc, "colorAttachments")
            .and_then(Value::as_array)
            .unwrap_or(&[]);
        for (i, a) in colors.iter().enumerate() {
            let Some(view) = cap.dict_get(a, "view").and_then(Value::as_obj) else {
                continue;
            };
            let resolve = cap.dict_get(a, "resolveTarget").and_then(Value::as_obj);
            let ms = view_target(cap, view)
                .and_then(|(t, _, _)| st.objs.fwd.get(&t))
                .is_some_and(|t| js::f(&js::get(t, "sampleCount")) > 1.0);
            let t = match (ms, resolve) {
                (true, Some(r)) => {
                    let mut t = read_view(st, cap, format!("Color {i} (resolve target)"), r).await;
                    t.note.get_or_insert_with(|| {
                        "multisampled; showing resolve target (written at pass end)".into()
                    });
                    t
                }
                _ => read_view(st, cap, format!("Color {i}"), view).await,
            };
            ins.targets.push(t);
        }
        if let Some(ds) = cap.dict_get(&ps.desc, "depthStencilAttachment")
            && let Some(view) = cap.dict_get(ds, "view").and_then(Value::as_obj)
        {
            ins.targets
                .push(read_view(st, cap, "Depth/Stencil".into(), view).await);
        }
    }

    if ps.pipeline.is_none()
        && cap
            .frame
            .get(index)
            .is_some_and(|c| cap.method(c) == "executeBundles")
    {
        for b in cap.frame[index]
            .args
            .first()
            .and_then(Value::as_array)
            .unwrap_or(&[])
        {
            let Some(bundle) = b.as_obj() else { continue };
            if let Some(bs) = bundle_state(cap, bundle) {
                ins.notes
                    .push(format!("state recorded in {}", obj_label(cap, bundle)));
                describe(cap, &bs, &mut ins, false);
            }
        }
        return ins;
    }
    describe(cap, &ps, &mut ins, !ps.compute);
    ins
}

/// Pass-like state of a render bundle, from its encoder's recorded calls.
fn bundle_state(cap: &Capture, bundle: ObjId) -> Option<PassState> {
    let enc = creation_call(cap, bundle)?.this;
    let mut ps = PassState {
        encoder: enc,
        pass: enc,
        compute: false,
        desc: Value::Undefined,
        pipeline: None,
        bind_groups: Default::default(),
        vertex_buffers: Default::default(),
        index_buffer: None,
        viewport: None,
        scissor: None,
        blend_constant: None,
        stencil_ref: None,
    };
    for c in cap
        .setup
        .iter()
        .chain(cap.frame.iter())
        .filter(|c| c.this == enc)
    {
        let num = |i: usize| c.args.get(i).and_then(Value::as_f64);
        match cap.method(c) {
            "setPipeline" => ps.pipeline = c.args.first().and_then(Value::as_obj),
            "setBindGroup" => {
                if let (Some(i), Some(bg)) = (num(0), c.args.get(1).and_then(Value::as_obj)) {
                    let offs = c
                        .args
                        .get(2)
                        .map(|v| {
                            crate::util::num_list(cap, v)
                                .into_iter()
                                .map(|x| x as u32)
                                .collect()
                        })
                        .unwrap_or_default();
                    ps.bind_groups.insert(i as u32, (bg, offs));
                }
            }
            "setVertexBuffer" => {
                if let (Some(i), Some(b)) = (num(0), c.args.get(1).and_then(Value::as_obj)) {
                    ps.vertex_buffers
                        .insert(i as u32, (b, num(2).unwrap_or(0.0), num(3)));
                }
            }
            "setIndexBuffer" => {
                if let (Some(b), Some(Value::String(f))) =
                    (c.args.first().and_then(Value::as_obj), c.args.get(1))
                {
                    ps.index_buffer =
                        Some((b, cap.str(*f).to_string(), num(2).unwrap_or(0.0), num(3)));
                }
            }
            "finish" => break,
            _ => {}
        }
    }
    Some(ps)
}

/// Pipeline tree, dynamic state and bindings of a pass (or bundle) state.
fn describe(cap: &Capture, ps: &PassState, ins: &mut Inspection, dynamic_state: bool) {
    // Pipeline.
    match ps.pipeline {
        Some(p) => match creation_desc(cap, p) {
            Some(desc) => ins.pipeline.push(value_tree(cap, &obj_label(cap, p), desc)),
            None => ins.pipeline.push(leaf("pipeline", obj_label(cap, p))),
        },
        None => ins.notes.push("no pipeline set".into()),
    }
    if dynamic_state {
        let mut dynamic = Vec::new();
        let fmt_list = |v: &[f64]| {
            v.iter()
                .map(|x| format!("{x}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        dynamic.push(leaf(
            "viewport",
            ps.viewport
                .map(|v| fmt_list(&v))
                .unwrap_or_else(|| "full target".into()),
        ));
        dynamic.push(leaf(
            "scissor",
            ps.scissor
                .map(|v| fmt_list(&v))
                .unwrap_or_else(|| "full target".into()),
        ));
        dynamic.push(match &ps.blend_constant {
            Some(v @ (Value::Dict(_) | Value::Array(_))) => value_tree(cap, "blendConstant", v),
            Some(v) => leaf("blendConstant", value_label(cap, v)),
            None => leaf("blendConstant", "[0, 0, 0, 0]".into()),
        });
        dynamic.push(leaf(
            "stencilReference",
            format!("{}", ps.stencil_ref.unwrap_or(0.0)),
        ));
        ins.pipeline.push(StateNode::Group {
            name: "Dynamic".into(),
            children: dynamic,
        });
    }

    // Bindings.
    for (idx, (bg, offsets)) in &ps.bind_groups {
        let group = format!("Bind group {idx} ({})", obj_label(cap, *bg));
        let Some(desc) = creation_desc(cap, *bg) else {
            ins.bindings.push(BindingRow {
                group,
                slot: "?".into(),
                resource: Some(*bg),
                detail: "descriptor unknown".into(),
            });
            continue;
        };
        let layout = cap.dict_get(desc, "layout").and_then(Value::as_obj);
        let mut dyn_iter = offsets.iter();
        for e in cap
            .dict_get(desc, "entries")
            .and_then(Value::as_array)
            .unwrap_or(&[])
        {
            let binding = get_num(cap, e, "binding").unwrap_or(0.0) as u32;
            let res = cap.dict_get(e, "resource").unwrap_or(&Value::Undefined);
            let ld = layout_detail(cap, layout, binding);
            let (resource, mut detail) = match res {
                Value::ObjRef(id) => match cap.class_of(*id) {
                    "GPUTextureView" => (
                        view_target(cap, *id).map(|t| t.0).or(Some(*id)),
                        format!("view {}", obj_label(cap, *id)),
                    ),
                    _ => (Some(*id), obj_label(cap, *id)),
                },
                Value::Dict(_) => {
                    let buf = cap.dict_get(res, "buffer").and_then(Value::as_obj);
                    let off = get_num(cap, res, "offset").unwrap_or(0.0);
                    let size = get_num(cap, res, "size")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "rest".into());
                    let mut d = format!(
                        "{} offset {off} size {size}",
                        buf.map(|b| obj_label(cap, b)).unwrap_or_default()
                    );
                    let dynamic = layout.and_then(|l| creation_call(cap, l)).is_some_and(|c| {
                        c.args
                            .first()
                            .and_then(|d| cap.dict_get(d, "entries"))
                            .and_then(Value::as_array)
                            .is_some_and(|es| {
                                es.iter().any(|le| {
                                    get_num(cap, le, "binding") == Some(binding as f64)
                                        && cap
                                            .dict_get(le, "buffer")
                                            .and_then(|b| cap.dict_get(b, "hasDynamicOffset"))
                                            == Some(&Value::Bool(true))
                                })
                            })
                    });
                    if dynamic && let Some(o) = dyn_iter.next() {
                        d.push_str(&format!(" + dynamic offset {o}"));
                    }
                    (buf, d)
                }
                v => (None, value_label(cap, v)),
            };
            if !ld.is_empty() {
                detail = format!("{detail} [{ld}]");
            }
            ins.bindings.push(BindingRow {
                group: group.clone(),
                slot: format!("@binding({binding})"),
                resource,
                detail,
            });
        }
        if !offsets.is_empty() {
            ins.bindings.push(BindingRow {
                group: group.clone(),
                slot: "dynamic offsets".into(),
                resource: None,
                detail: format!("{offsets:?}"),
            });
        }
    }
    for (slot, (buf, off, size)) in &ps.vertex_buffers {
        ins.bindings.push(BindingRow {
            group: "Vertex buffers".into(),
            slot: format!("slot {slot}"),
            resource: Some(*buf),
            detail: format!(
                "{} offset {off} size {}",
                obj_label(cap, *buf),
                size.map(|s| s.to_string()).unwrap_or_else(|| "rest".into())
            ),
        });
    }
    if let Some((buf, format, off, size)) = &ps.index_buffer {
        ins.bindings.push(BindingRow {
            group: "Index buffer".into(),
            slot: format.clone(),
            resource: Some(*buf),
            detail: format!(
                "{} offset {off} size {}",
                obj_label(cap, *buf),
                size.map(|s| s.to_string()).unwrap_or_else(|| "rest".into())
            ),
        });
    }
}
