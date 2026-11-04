use webgd_core::Capture;
use webgd_core::export::{CaptureExportOptions, capture_markdown, trace_markdown};
use webgd_core::trace_model::{CounterSample, CounterTrack, QueueTrack, Span, TraceResult};
use webgd_core::wire::decode;

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/sample.wire");

#[test]
fn capture_export_fixture() {
    let cap = decode(FIXTURE).unwrap();
    let opts = CaptureExportOptions {
        setup: true,
        ..CaptureExportOptions::default()
    };
    let md = capture_markdown(&cap, &opts);
    for line in [
        "# webgd capture",
        "- url: http://fixture/",
        "- APIs: WebGL2",
        "- calls: setup 1, snapshot 0, frame 2",
        "- draws: 0, dispatches: 0",
        "## Frame commands",
        "| # | depth | kind | command |",
        "| 0 | 0 | Call | WebGL2RenderingContext#1.bufferData(34962, Float32Array[3], 35044) |",
        "| 1 | 0 | Group: pass | WebGL2RenderingContext#1.webgd:pushMarker(\"pass\", {…}) |",
        "## Draw state",
        "_no draws or dispatches_",
        "## Resources",
        "## Pre-frame setup",
        "0. WebGL2RenderingContext#1.createBuffer()",
    ] {
        assert!(
            md.lines().any(|l| l == line),
            "missing line {line:?} in:\n{md}"
        );
    }
    assert!(!md.contains("## Replay errors"));
    assert!(!md.contains("more rows omitted"));
    assert_eq!(md, capture_markdown(&cap, &opts));
}

#[test]
fn capture_export_truncates_and_reports_errors() {
    let cap = decode(FIXTURE).unwrap();
    let opts = CaptureExportOptions {
        max_rows: Some(1),
        replay_errors: vec![
            (usize::MAX, "setup: boom".into()),
            (1, "bad | pipe\nx".into()),
        ],
        ..CaptureExportOptions::default()
    };
    let md = capture_markdown(&cap, &opts);
    assert!(md.contains(
        "| 0 | 0 | Call | WebGL2RenderingContext#1.bufferData(34962, Float32Array[3], 35044) |"
    ));
    assert!(!md.contains("webgd:pushMarker"), "{md}");
    assert!(md.contains("## Replay errors"));
    assert!(md.contains("| pre-frame | setup: boom |"));
    assert!(!md.contains("bad \\| pipe"));
    assert!(
        md.lines()
            .any(|l| l == "… 1 more rows omitted (max_rows = 1)"),
        "{md}"
    );
    let full = capture_markdown(
        &cap,
        &CaptureExportOptions {
            replay_errors: vec![(1, "bad | pipe\nx".into())],
            ..CaptureExportOptions::default()
        },
    );
    assert!(full.contains("| #1 | bad \\| pipe<br>x |"), "{full}");
}

fn span(start_ns: u64, end_ns: u64, label: &str, depth: u8, index: usize) -> Span {
    Span {
        start_ns,
        end_ns,
        label: label.into(),
        depth,
        index,
    }
}

fn sample(start_ns: u64, end_ns: u64, value: f64, total: f64, index: usize) -> CounterSample {
    CounterSample {
        start_ns,
        end_ns,
        value,
        total,
        index,
    }
}

#[test]
fn trace_export() {
    let trace = TraceResult {
        queues: vec![QueueTrack {
            name: "Queue (GPUDevice#3)".into(),
            passes: vec![span(0, 3000, "pass", 0, 2)],
            markers: vec![span(0, 3000, "Offscreen", 0, 1)],
            items: vec![
                span(0, 1500, "GPURenderPassEncoder#5.draw(3)", 0, 4),
                span(1500, 3000, "GPURenderPassEncoder#5.draw(6)", 0, 5),
            ],
        }],
        counters: vec![
            CounterTrack {
                name: "ALU ops/ns (est.)".into(),
                unit: "ops/ns".into(),
                samples: vec![
                    sample(0, 1500, 2.0, 3000.0, 4),
                    sample(1500, 3000, 0.5, 750.0, 5),
                ],
            },
            CounterTrack {
                name: "Fragments (post-depth)".into(),
                unit: "fragments".into(),
                samples: vec![sample(0, 1500, 100.0, 100.0, 4)],
            },
        ],
        notes: vec!["timestamp-query unsupported".into()],
        total_ns: 3000,
    };
    let cap = Capture {
        url: "http://fixture/".into(),
        ..Capture::default()
    };
    let md = trace_markdown(&cap, &trace);
    for line in [
        "# webgd GPU trace",
        "- capture: http://fixture/",
        "- total: 3000 ns (3.000 µs)",
        "## Notes",
        "- timestamp-query unsupported",
        "## Queue: Queue (GPUDevice#3)",
        "### Passes",
        "| # | start | end | duration | pass |",
        "| 2 | 0 | 3000 | 3000 ns (3.000 µs) | pass |",
        "### Markers",
        "- Offscreen — 0..3000 ns (3000 ns, 3.000 µs), #1",
        "### Items",
        "| # | start | end | duration | command |",
        "| 4 | 0 | 1500 | 1500 ns (1.500 µs) | GPURenderPassEncoder#5.draw(3) |",
        "| 5 | 1500 | 3000 | 1500 ns (1.500 µs) | GPURenderPassEncoder#5.draw(6) |",
        "## Counters",
        "### ALU ops/ns (est.)",
        "- unit: ops/ns",
        "| # | start | end | value | total |",
        "| 4 | 0 | 1500 | 2 | 3000 |",
        "| 5 | 1500 | 3000 | 0.5 | 750 |",
        "### Fragments (post-depth)",
        "| 4 | 0 | 1500 | 100 | 100 |",
        "## Per-item summary",
        "| # | queue | command | duration | ALU ops/ns (est.) | ALU ops/ns (est.) total | Fragments (post-depth) | Fragments (post-depth) total |",
        "| 4 | Queue (GPUDevice#3) | GPURenderPassEncoder#5.draw(3) | 1500 ns (1.500 µs) | 2 | 3000 | 100 | 100 |",
        "| 5 | Queue (GPUDevice#3) | GPURenderPassEncoder#5.draw(6) | 1500 ns (1.500 µs) | 0.5 | 750 |  |  |",
    ] {
        assert!(
            md.lines().any(|l| l == line),
            "missing line {line:?} in:\n{md}"
        );
    }
    assert_eq!(md, trace_markdown(&cap, &trace));
}

#[test]
fn trace_export_identical_spans_and_marker_order() {
    let trace = TraceResult {
        queues: vec![QueueTrack {
            name: "Q".into(),
            passes: vec![],
            // Child listed before its parent.
            markers: vec![span(0, 0, "cube", 1, 12), span(0, 0, "Offscreen", 0, 6)],
            items: vec![
                span(0, 0, "drawIndexed", 0, 13),
                span(0, 0, "dispatchWorkgroups", 0, 20),
            ],
        }],
        counters: vec![CounterTrack {
            name: "ALU".into(),
            unit: "ops/ns".into(),
            samples: vec![sample(0, 0, 1.0, 10.0, 13), sample(0, 0, 2.0, 20.0, 20)],
        }],
        notes: vec![],
        total_ns: 0,
    };
    let md = trace_markdown(&Capture::default(), &trace);
    let lines: Vec<&str> = md.lines().collect();
    for line in [
        "| 13 | 0 | 0 | 1 | 10 |",
        "| 20 | 0 | 0 | 2 | 20 |",
        "| 13 | Q | drawIndexed | 0 ns (0.000 µs) | 1 | 10 |",
        "| 20 | Q | dispatchWorkgroups | 0 ns (0.000 µs) | 2 | 20 |",
    ] {
        assert!(lines.contains(&line), "missing line {line:?} in:\n{md}");
    }
    let parent = lines
        .iter()
        .position(|l| l.starts_with("- Offscreen —"))
        .expect(&md);
    let child = lines
        .iter()
        .position(|l| l.starts_with("  - cube —"))
        .expect(&md);
    assert!(parent < child, "{md}");
}
