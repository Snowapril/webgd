//! GPU trace result data (produced by `webgd-replay::run_trace`, consumed by the UI and exporters).

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraceResult {
    pub queues: Vec<QueueTrack>,
    pub counters: Vec<CounterTrack>,
    pub notes: Vec<String>,
    pub total_ns: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueTrack {
    pub name: String,
    pub passes: Vec<Span>,
    pub markers: Vec<Span>,
    pub items: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub start_ns: u64,
    pub end_ns: u64,
    pub label: String,
    pub depth: u8,
    /// Frame command index.
    pub index: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CounterTrack {
    pub name: String,
    pub unit: String,
    pub samples: Vec<CounterSample>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterSample {
    pub start_ns: u64,
    pub end_ns: u64,
    pub value: f64,
    pub total: f64,
    /// Frame command index of the item this sample was measured for (samples are one per item;
    /// several items can share identical time spans when timestamps are quantized).
    pub index: usize,
}
