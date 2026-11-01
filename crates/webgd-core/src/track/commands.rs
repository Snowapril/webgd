//! Frame command tree (groups, passes, item kinds).
use crate::capture::Capture;

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

pub fn build_tree(cap: &Capture) -> Vec<CmdNode> {
    let _ = cap;
    todo!("core agent")
}
