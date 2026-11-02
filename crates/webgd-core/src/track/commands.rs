//! Frame command tree (groups, passes, item kinds).
use crate::capture::{Call, Capture, ObjId, Value};

#[derive(Clone, PartialEq, Debug)]
pub enum CmdKind {
    Call,
    Draw,
    Dispatch,
    Clear,
    Copy,
    Submit,
    GroupBegin(String),
    GroupEnd,
    PassBegin,
    PassEnd,
}

/// One node per `cap.frame` call, in order (`nodes[i].index == i`).
/// `span_end` is the index of the matching close for GroupBegin/PassBegin, else `index`.
#[derive(Clone, PartialEq, Debug)]
pub struct CmdNode {
    pub index: usize,
    pub depth: u16,
    pub kind: CmdKind,
    pub span_end: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// Pass encoder object; closed by `end` on it.
    Pass(ObjId),
    /// `pushDebugGroup` / `webgd:pushMarker` on `this`; closed by the matching pop on the same object.
    Group(ObjId),
}

/// Leaf kind of a non-scope call.
pub fn classify(cap: &Capture, call: &Call) -> CmdKind {
    let m = cap.method(call);
    let class = cap.class_of(call.this);
    if !class.starts_with("GPU") {
        if m.starts_with("drawArrays")
            || m.starts_with("drawElements")
            || m.starts_with("drawRangeElements")
            || m.starts_with("multiDraw")
        {
            return CmdKind::Draw;
        }
        if m == "clear" || m.starts_with("clearBuffer") || m == "blitFramebuffer" {
            return CmdKind::Clear;
        }
        return CmdKind::Call;
    }
    match m {
        "draw" | "drawIndexed" | "drawIndirect" | "drawIndexedIndirect" | "executeBundles"
            if class != "GPURenderBundleEncoder" =>
        {
            CmdKind::Draw
        }
        "submit" if class == "GPUQueue" => CmdKind::Submit,
        "writeBuffer"
        | "writeTexture"
        | "copyExternalImageToTexture"
        | "resolveQuerySet"
        | "clearBuffer" => CmdKind::Copy,
        _ if m.starts_with("dispatchWorkgroups") => CmdKind::Dispatch,
        _ if m.starts_with("copy") => CmdKind::Copy,
        _ => CmdKind::Call,
    }
}

pub fn build_tree(cap: &Capture) -> Vec<CmdNode> {
    let mut nodes: Vec<CmdNode> = Vec::with_capacity(cap.frame.len());
    // (scope, node index of its opener)
    let mut stack: Vec<(Scope, usize)> = Vec::new();
    for (i, call) in cap.frame.iter().enumerate() {
        let m = cap.method(call);
        let depth = stack.len() as u16;
        let open = match m {
            "beginRenderPass" | "beginComputePass" => call.result.as_obj().map(Scope::Pass),
            "pushDebugGroup" | "webgd:pushMarker" => Some(Scope::Group(call.this)),
            _ => None,
        };
        if let Some(scope) = open {
            let kind = match scope {
                Scope::Pass(_) => CmdKind::PassBegin,
                Scope::Group(_) => CmdKind::GroupBegin(match call.args.first() {
                    Some(Value::String(s)) => cap.str(*s).to_owned(),
                    _ => String::new(),
                }),
            };
            nodes.push(CmdNode {
                index: i,
                depth,
                kind,
                span_end: i,
            });
            stack.push((scope, i));
            continue;
        }
        let close = match m {
            "end" => Some((Scope::Pass(call.this), CmdKind::PassEnd)),
            "popDebugGroup" | "webgd:popMarker" => {
                Some((Scope::Group(call.this), CmdKind::GroupEnd))
            }
            _ => None,
        };
        if let Some((scope, kind)) = close
            && let Some(pos) = stack.iter().rposition(|(s, _)| *s == scope)
        {
            // Close the matched scope and anything still open inside it.
            for (_, opener) in stack.drain(pos..) {
                nodes[opener].span_end = i;
            }
            nodes.push(CmdNode {
                index: i,
                depth: pos as u16,
                kind,
                span_end: i,
            });
            continue;
        }
        // Unbalanced closes fall through as plain calls.
        nodes.push(CmdNode {
            index: i,
            depth,
            kind: classify(cap, call),
            span_end: i,
        });
    }
    if let Some(last) = cap.frame.len().checked_sub(1) {
        for (_, opener) in stack {
            nodes[opener].span_end = last;
        }
    }
    nodes
}
