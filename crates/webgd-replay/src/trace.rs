//! GPU trace runner: timestamp/timer-query instrumented replays + estimated counters.
use crate::{ReplayError, Replayer};
use std::rc::Rc;
use webgd_core::Capture;

pub enum TraceInstr {
    /// Timed run (WebGPU timestamp + occlusion queries, GL timer queries).
    Timed,
    /// Untimed GL run counting rasterized fragments per draw.
    GlFragmentCount,
}

pub struct TraceResult {
    pub queues: Vec<QueueTrack>,
    pub counters: Vec<CounterTrack>,
    pub notes: Vec<String>,
    pub total_ns: u64,
}

pub struct QueueTrack {
    pub name: String,
    pub passes: Vec<Span>,
    pub markers: Vec<Span>,
    pub items: Vec<Span>,
}

#[derive(Clone)]
pub struct Span {
    pub start_ns: u64,
    pub end_ns: u64,
    pub label: String,
    pub depth: u8,
    /// Frame command index.
    pub index: usize,
}

pub struct CounterTrack {
    pub name: String,
    pub unit: String,
    pub samples: Vec<CounterSample>,
}

pub struct CounterSample {
    pub start_ns: u64,
    pub end_ns: u64,
    pub value: f64,
    pub total: f64,
}

pub async fn run_trace(rep: &Replayer, cap: &Rc<Capture>) -> Result<TraceResult, ReplayError> {
    let _ = (rep, cap);
    todo!("replay agent")
}
