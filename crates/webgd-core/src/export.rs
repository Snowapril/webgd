//! Plain-text (Markdown) exports of a capture and of a GPU trace, for readers that cannot run the
//! browser UI (scripts, LLM agents). Output is deterministic for a given input.
use crate::capture::{Call, Capture, ObjId, Section, Value};
use crate::display::{call_label, obj_label, value_label};
use crate::trace_model::{QueueTrack, Span, TraceResult};
use crate::track::commands::{CmdKind, CmdNode, build_tree};
use crate::track::gpu::{self, PassState};
use crate::track::resources::{self, Origin, ResourceRow};
use crate::track::revisions::{self, Access};
use std::collections::HashMap;
use std::fmt::Write as _;

/// What to include in a capture export.
#[derive(Clone, Debug)]
pub struct CaptureExportOptions {
    /// Include the frame command list (`cap.frame`). Default true.
    pub commands: bool,
    /// Include the pre-frame setup calls (`cap.setup`). Default false (can be large).
    pub setup: bool,
    /// Include the resource table and per-resource revision histories. Default true.
    pub resources: bool,
    /// Include per-draw/dispatch static state (WebGPU pass state, GL bindings known statically). Default true.
    pub draw_state: bool,
    /// Truncate each list to this many entries (`None` = everything); truncation is stated in the output.
    pub max_rows: Option<usize>,
    /// Replay errors to report, as (frame index or usize::MAX, message). Default empty.
    pub replay_errors: Vec<(usize, String)>,
}

impl Default for CaptureExportOptions {
    fn default() -> Self {
        CaptureExportOptions {
            commands: true,
            setup: false,
            resources: true,
            draw_state: true,
            max_rows: None,
            replay_errors: Vec::new(),
        }
    }
}

/// `writeln!` into a `String` (cannot fail).
macro_rules! w {
    ($out:expr) => {{
        $out.push('\n');
    }};
    ($out:expr, $($t:tt)*) => {{
        let _ = writeln!($out, $($t)*);
    }};
}

/// Markdown description of a capture: summary, command tree, resources, revisions, draw state.
pub fn capture_markdown(cap: &Capture, opts: &CaptureExportOptions) -> String {
    let mut out = String::new();
    let tree = build_tree(cap);
    let rows = resources::list(cap);
    let labels: HashMap<ObjId, String> = rows
        .iter()
        .filter(|r| !r.label.is_empty())
        .map(|r| (r.id, r.label.clone()))
        .collect();

    summary(&mut out, cap, &tree, rows.len());
    if !opts.replay_errors.is_empty() {
        replay_errors(&mut out, &opts.replay_errors, opts.max_rows);
    }
    if opts.commands {
        frame_commands(&mut out, cap, &tree, opts.max_rows);
    }
    if opts.draw_state {
        draw_state(&mut out, cap, &tree, &labels, opts.max_rows);
    }
    if opts.resources {
        resource_tables(&mut out, cap, &rows, opts.max_rows);
    }
    if opts.setup {
        setup_list(&mut out, cap, opts.max_rows);
    }
    out
}

/// Markdown description of a GPU trace: notes, per-queue passes/markers/items timeline, counter
/// tracks, and a per-item table joining durations with every counter's value/total.
pub fn trace_markdown(cap: &Capture, trace: &TraceResult) -> String {
    let mut out = String::new();
    w!(out, "# webgd GPU trace");
    w!(out);
    w!(out, "- capture: {}", esc(&cap.url));
    w!(out, "- total: {}", dur_cell(trace.total_ns));
    w!(
        out,
        "- queues: {}, counter tracks: {}",
        trace.queues.len(),
        trace.counters.len()
    );
    w!(out);

    w!(out, "## Notes");
    w!(out);
    if trace.notes.is_empty() {
        w!(out, "_none_");
    }
    for n in &trace.notes {
        w!(out, "- {}", esc(n));
    }
    w!(out);

    for q in &trace.queues {
        w!(out, "## Queue: {}", esc(&q.name));
        w!(out);
        w!(out, "### Passes");
        w!(out);
        span_table(&mut out, &q.passes, "pass");
        w!(out, "### Markers");
        w!(out);
        if q.markers.is_empty() {
            w!(out, "_none_");
        }
        for m in sorted_markers(&q.markers) {
            w!(
                out,
                "{}- {} — {}..{} ns ({} ns, {}), #{}",
                "  ".repeat(m.depth as usize),
                esc(&m.label),
                m.start_ns,
                m.end_ns,
                span_ns(m),
                human(span_ns(m)),
                m.index
            );
        }
        w!(out);
        w!(out, "### Items");
        w!(out);
        span_table(&mut out, &q.items, "command");
    }

    let items: Vec<(&QueueTrack, &Span)> = trace
        .queues
        .iter()
        .flat_map(|q| q.items.iter().map(move |s| (q, s)))
        .collect();

    w!(out, "## Counters");
    w!(out);
    if trace.counters.is_empty() {
        w!(out, "_none_");
        w!(out);
    }
    for c in &trace.counters {
        w!(out, "### {}", esc(&c.name));
        w!(out);
        w!(out, "- unit: {}", esc(&c.unit));
        w!(out);
        if c.samples.is_empty() {
            w!(out, "_no samples_");
            w!(out);
            continue;
        }
        w!(out, "| # | start | end | value | total |");
        w!(out, "|---|---|---|---|---|");
        for s in &c.samples {
            let idx = s.index.to_string();
            row(
                &mut out,
                &[
                    idx,
                    s.start_ns.to_string(),
                    s.end_ns.to_string(),
                    num(s.value),
                    num(s.total),
                ],
            );
        }
        w!(out);
    }

    w!(out, "## Per-item summary");
    w!(out);
    if items.is_empty() {
        w!(out, "_no items_");
        return out;
    }
    let mut header = vec![
        "#".to_owned(),
        "queue".to_owned(),
        "command".to_owned(),
        "duration".to_owned(),
    ];
    for c in &trace.counters {
        header.push(esc(&c.name));
        header.push(format!("{} total", esc(&c.name)));
    }
    w!(out, "| {} |", header.join(" | "));
    w!(out, "|{}", "---|".repeat(header.len()));
    for (q, it) in &items {
        let mut cells = vec![
            it.index.to_string(),
            esc(&q.name),
            esc(&it.label),
            dur_cell(span_ns(it)),
        ];
        for c in &trace.counters {
            match c.samples.iter().find(|s| s.index == it.index) {
                Some(s) => {
                    cells.push(num(s.value));
                    cells.push(num(s.total));
                }
                None => {
                    cells.push(String::new());
                    cells.push(String::new());
                }
            }
        }
        row(&mut out, &cells);
    }
    w!(out);
    out
}

// ---------------------------------------------------------------------------------------------
// Capture sections

fn summary(out: &mut String, cap: &Capture, tree: &[CmdNode], resource_count: usize) {
    let has = |class: &str| cap.objects.values().any(|o| cap.str(o.class) == class);
    let mut apis = Vec::new();
    if has("WebGL2RenderingContext") {
        apis.push("WebGL2");
    }
    if has("GPUDevice") {
        apis.push("WebGPU");
    }
    if has("WebGLRenderingContext") {
        apis.push("WebGL1 (unsupported)");
    }
    let count = |k: CmdKind| tree.iter().filter(|n| n.kind == k).count();
    w!(out, "# webgd capture");
    w!(out);
    w!(out, "- url: {}", esc(&cap.url));
    w!(
        out,
        "- APIs: {}",
        if apis.is_empty() {
            "none detected".to_owned()
        } else {
            apis.join(", ")
        }
    );
    w!(
        out,
        "- calls: setup {}, snapshot {}, frame {}",
        cap.setup.len(),
        cap.snapshot.len(),
        cap.frame.len()
    );
    w!(out, "- objects: {}", cap.objects.len());
    w!(
        out,
        "- draws: {}, dispatches: {}",
        count(CmdKind::Draw),
        count(CmdKind::Dispatch)
    );
    w!(out, "- resources: {resource_count}");
    w!(out);
}

fn replay_errors(out: &mut String, errors: &[(usize, String)], max: Option<usize>) {
    w!(out, "## Replay errors");
    w!(out);
    w!(out, "| # | message |");
    w!(out, "|---|---|");
    let (shown, rest) = truncate(errors, max);
    for (i, msg) in shown {
        let at = if *i == usize::MAX {
            "pre-frame".to_owned()
        } else {
            format!("#{i}")
        };
        row(out, &[at, esc(msg)]);
    }
    omitted(out, rest, max);
}

fn kind_name(k: &CmdKind) -> String {
    match k {
        CmdKind::Call => "Call".into(),
        CmdKind::Draw => "Draw".into(),
        CmdKind::Dispatch => "Dispatch".into(),
        CmdKind::Clear => "Clear".into(),
        CmdKind::Copy => "Copy".into(),
        CmdKind::Submit => "Submit".into(),
        CmdKind::GroupBegin(name) => format!("Group: {}", esc(name)),
        CmdKind::GroupEnd => "Group end".into(),
        CmdKind::PassBegin => "Pass".into(),
        CmdKind::PassEnd => "Pass end".into(),
    }
}

fn frame_commands(out: &mut String, cap: &Capture, tree: &[CmdNode], max: Option<usize>) {
    w!(out, "## Frame commands");
    w!(out);
    if tree.is_empty() {
        w!(out, "_empty frame_");
        w!(out);
        return;
    }
    w!(out, "| # | depth | kind | command |");
    w!(out, "|---|---|---|---|");
    let (shown, rest) = truncate(tree, max);
    for n in shown {
        row(
            out,
            &[
                n.index.to_string(),
                n.depth.to_string(),
                kind_name(&n.kind),
                format!(
                    "{}{}",
                    "· ".repeat(n.depth as usize),
                    esc(&call_label(cap, &cap.frame[n.index]))
                ),
            ],
        );
    }
    omitted(out, rest, max);
}

/// Statically known GL bindings of one context. `None` = unknown, `Some(ObjId(0))` = none/default.
#[derive(Clone, Copy, Default)]
struct GlStatic {
    program: Option<ObjId>,
    vao: Option<ObjId>,
    draw_fb: Option<ObjId>,
    read_fb: Option<ObjId>,
}

const GL_READ_FRAMEBUFFER: u32 = 36008;
const GL_DRAW_FRAMEBUFFER: u32 = 36009;
const GL_FRAMEBUFFER: u32 = 36160;

fn draw_state(
    out: &mut String,
    cap: &Capture,
    tree: &[CmdNode],
    labels: &HashMap<ObjId, String>,
    max: Option<usize>,
) {
    w!(out, "## Draw state");
    w!(out);
    let draws: Vec<usize> = tree
        .iter()
        .filter(|n| matches!(n.kind, CmdKind::Draw | CmdKind::Dispatch))
        .map(|n| n.index)
        .collect();
    if draws.is_empty() {
        w!(out, "_no draws or dispatches_");
        w!(out);
        return;
    }
    let (shown, rest) = truncate(&draws, max);

    // Frame-start GL bindings from the snapshot.
    let mut gl: HashMap<ObjId, GlStatic> = HashMap::new();
    for c in cap
        .snapshot
        .iter()
        .filter(|c| cap.method(c) == "webgd:glState")
    {
        let Some(s) = c.args.first() else { continue };
        let obj = |k: &str| cap.dict_get(s, k).map(|v| v.as_obj().unwrap_or(ObjId(0)));
        gl.insert(
            c.this,
            GlStatic {
                program: obj("35725"),
                vao: obj("34229"),
                draw_fb: obj("36006"),
                read_fb: obj("36010"),
            },
        );
    }

    let mut next = shown.iter().copied().peekable();
    let last = shown.last().copied().unwrap_or(0);
    for (i, call) in cap.frame.iter().enumerate().take(last + 1) {
        let is_gpu = cap.class_of(call.this).starts_with("GPU");
        if !is_gpu {
            let obj = |k: usize| {
                call.args
                    .get(k)
                    .map(|v| v.as_obj().unwrap_or(ObjId(0)))
                    .unwrap_or(ObjId(0))
            };
            let st = gl.entry(call.this);
            match cap.method(call) {
                "useProgram" => st.or_default().program = Some(obj(0)),
                "bindVertexArray" => st.or_default().vao = Some(obj(0)),
                "bindFramebuffer" => {
                    let fb = Some(obj(1));
                    let st = st.or_default();
                    match call.args.first().and_then(Value::as_f64).map(|t| t as u32) {
                        Some(GL_DRAW_FRAMEBUFFER) => st.draw_fb = fb,
                        Some(GL_READ_FRAMEBUFFER) => st.read_fb = fb,
                        Some(GL_FRAMEBUFFER) => {
                            st.draw_fb = fb;
                            st.read_fb = fb;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if next.peek() != Some(&i) {
            continue;
        }
        next.next();
        w!(out, "### #{i} {}", esc(&call_label(cap, call)));
        w!(out);
        if is_gpu {
            match gpu::state_at(cap, i).pass {
                Some(p) => gpu_pass_state(out, cap, labels, &p),
                None => w!(out, "- no open pass at this command"),
            }
        } else {
            let st = gl.get(&call.this).copied().unwrap_or_default();
            let gl_obj = |v: Option<ObjId>, zero: &str| match v {
                None => "unknown".to_owned(),
                Some(ObjId(0)) => zero.to_owned(),
                Some(id) => named(cap, labels, id),
            };
            w!(out, "- context: {} (static)", obj_label(cap, call.this));
            w!(out, "- program: {}", gl_obj(st.program, "none"));
            w!(out, "- vertex array: {}", gl_obj(st.vao, "default"));
            w!(out, "- draw framebuffer: {}", gl_obj(st.draw_fb, "default"));
            w!(out, "- read framebuffer: {}", gl_obj(st.read_fb, "default"));
        }
        w!(out);
    }
    omitted(out, rest, max);
}

fn gpu_pass_state(out: &mut String, cap: &Capture, labels: &HashMap<ObjId, String>, p: &PassState) {
    w!(
        out,
        "- pass: {} ({}) on {}",
        named(cap, labels, p.pass),
        if p.compute { "compute" } else { "render" },
        named(cap, labels, p.encoder)
    );
    match p.pipeline {
        Some(id) => {
            w!(out, "- pipeline: {}", named(cap, labels, id));
            pipeline_summary(out, cap, labels, id);
        }
        None => w!(out, "- pipeline: none"),
    }
    if p.bind_groups.is_empty() {
        w!(out, "- bind groups: none");
    } else {
        w!(out, "- bind groups:");
        for (i, (bg, offs)) in &p.bind_groups {
            let offs: Vec<String> = offs.iter().map(u32::to_string).collect();
            w!(
                out,
                "  - {i}: {} [{}]",
                named(cap, labels, *bg),
                offs.join(", ")
            );
        }
    }
    if !p.compute {
        if p.vertex_buffers.is_empty() {
            w!(out, "- vertex buffers: none");
        } else {
            w!(out, "- vertex buffers:");
            for (slot, (buf, off, size)) in &p.vertex_buffers {
                w!(
                    out,
                    "  - {slot}: {} @{} size {}",
                    named(cap, labels, *buf),
                    num(*off),
                    size.map_or("rest".to_owned(), num)
                );
            }
        }
        match &p.index_buffer {
            Some((buf, fmt, off, size)) => w!(
                out,
                "- index buffer: {} {} @{} size {}",
                named(cap, labels, *buf),
                esc(fmt),
                num(*off),
                size.map_or("rest".to_owned(), num)
            ),
            None => w!(out, "- index buffer: none"),
        }
        match p.viewport {
            Some([x, y, wd, h, mn, mx]) => w!(
                out,
                "- viewport: x {} y {} w {} h {} depth {}..{}",
                num(x),
                num(y),
                num(wd),
                num(h),
                num(mn),
                num(mx)
            ),
            None => w!(out, "- viewport: default (full attachment)"),
        }
        match p.scissor {
            Some([x, y, wd, h]) => w!(
                out,
                "- scissor: x {} y {} w {} h {}",
                num(x),
                num(y),
                num(wd),
                num(h)
            ),
            None => w!(out, "- scissor: default (full attachment)"),
        }
        if let Some(bc) = &p.blend_constant {
            w!(out, "- blend constant: {}", esc(&value_text(cap, bc)));
        }
        if let Some(r) = p.stencil_ref {
            w!(out, "- stencil reference: {}", num(r));
        }
    }
}

/// The creating call of `id`: a direct `ObjRef` result, or a `*Async` call whose pending token
/// was resolved to `id` by `webgd:resolved`.
fn creating_call(cap: &Capture, id: ObjId) -> Option<&Call> {
    if let Some((s, i)) = gpu::creation_call(cap, id) {
        return Some(&cap.section(s)[i]);
    }
    let target = Value::ObjRef(id);
    for s in [Section::Setup, Section::Frame] {
        let calls = cap.section(s);
        let token = calls
            .iter()
            .find(|c| cap.method(c) == "webgd:resolved" && c.args.get(1) == Some(&target))
            .and_then(|c| c.args.first())
            .and_then(Value::as_f64);
        if let Some(t) = token {
            let pending = Value::Pending(t as u32);
            for s2 in [Section::Setup, Section::Frame] {
                if let Some(c) = cap.section(s2).iter().find(|c| c.result == pending) {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn pipeline_summary(out: &mut String, cap: &Capture, labels: &HashMap<ObjId, String>, id: ObjId) {
    let Some(desc) = creating_call(cap, id).and_then(|c| c.args.first()) else {
        w!(out, "  - descriptor: not found");
        return;
    };
    let get = |v: &Value, k: &str| cap.dict_get(v, k).cloned();
    let text = |v: Option<Value>| match v {
        Some(Value::ObjRef(o)) => named(cap, labels, o),
        Some(v) if !v.is_nullish() => esc(&value_text(cap, &v)),
        _ => String::new(),
    };
    if let Some(layout) = get(desc, "layout") {
        w!(out, "  - layout: {}", text(Some(layout)));
    }
    let stage = |key: &str| {
        get(desc, key).map(|st| {
            let entry = text(get(&st, "entryPoint"));
            format!(
                "{} entry {}",
                text(get(&st, "module")),
                if entry.is_empty() {
                    "(default)".to_owned()
                } else {
                    entry
                }
            )
        })
    };
    if let Some(cs) = stage("compute") {
        w!(out, "  - compute: {cs}");
        return;
    }
    let topology = get(desc, "primitive")
        .and_then(|p| get(&p, "topology"))
        .map(|t| text(Some(t)))
        .unwrap_or_else(|| "triangle-list".to_owned());
    w!(out, "  - topology: {topology}");
    if let Some(vs) = stage("vertex") {
        w!(out, "  - vertex: {vs}");
    }
    if let Some(fs) = stage("fragment") {
        let formats: Vec<String> = get(desc, "fragment")
            .and_then(|f| get(&f, "targets"))
            .and_then(|t| t.as_array().map(<[Value]>::to_vec))
            .unwrap_or_default()
            .iter()
            .map(|t| {
                let f = text(get(t, "format"));
                if f.is_empty() { "none".to_owned() } else { f }
            })
            .collect();
        w!(out, "  - fragment: {fs}, targets: {}", formats.join(", "));
    }
    if let Some(ds) = get(desc, "depthStencil") {
        w!(out, "  - depthStencil: {}", text(get(&ds, "format")));
    }
}

fn resource_tables(out: &mut String, cap: &Capture, rows: &[ResourceRow], max: Option<usize>) {
    let revs = revisions::all(cap);
    let writes = |id: ObjId| {
        revs.get(&id).map_or(0, |h| {
            h.iter().filter(|r| r.access == Access::Write).count()
        })
    };
    w!(out, "## Resources");
    w!(out);
    if rows.is_empty() {
        w!(out, "_no resources_");
        w!(out);
    } else {
        w!(out, "| id | class | label | summary | origin | writes |");
        w!(out, "|---|---|---|---|---|---|");
        let (shown, rest) = truncate(rows, max);
        for r in shown {
            row(
                out,
                &[
                    r.id.0.to_string(),
                    esc(&r.class),
                    esc(&r.label),
                    esc(&r.summary),
                    match r.origin {
                        Origin::PreFrame => "pre-frame".to_owned(),
                        Origin::Frame(i) => format!("frame #{i}"),
                    },
                    writes(r.id).to_string(),
                ],
            );
        }
        omitted(out, rest, max);
    }

    w!(out, "### Revisions");
    w!(out);
    let written: Vec<&ResourceRow> = rows.iter().filter(|r| writes(r.id) > 0).collect();
    if written.is_empty() {
        w!(out, "_no resource is written_");
        w!(out);
        return;
    }
    let (shown, rest) = truncate(&written, max);
    for r in shown {
        if r.label.is_empty() {
            w!(out, "#### {}", obj_label(cap, r.id));
        } else {
            w!(out, "#### {} \"{}\"", obj_label(cap, r.id), esc(&r.label));
        }
        w!(out);
        let history = revs.get(&r.id).map_or(&[][..], Vec::as_slice);
        let (hs, hrest) = truncate(history, max);
        for rev in hs {
            let at = match rev.section {
                Section::Frame => format!("#{}", rev.index),
                _ => "pre-frame".to_owned(),
            };
            w!(
                out,
                "- r{} {} {:?} {}",
                rev.revision,
                at,
                rev.access,
                esc(&rev.detail)
            );
        }
        w!(out);
        omitted(out, hrest, max);
    }
    omitted(out, rest, max);
}

fn setup_list(out: &mut String, cap: &Capture, max: Option<usize>) {
    w!(out, "## Pre-frame setup");
    w!(out);
    if cap.setup.is_empty() {
        w!(out, "_none_");
        w!(out);
        return;
    }
    let (shown, rest) = truncate(&cap.setup, max);
    for (i, c) in shown.iter().enumerate() {
        w!(out, "{i}. {}", esc(&call_label(cap, c)));
    }
    w!(out);
    omitted(out, rest, max);
}

// ---------------------------------------------------------------------------------------------
// Helpers

/// `Class#id "label"` (label only when known).
fn named(cap: &Capture, labels: &HashMap<ObjId, String>, id: ObjId) -> String {
    match labels.get(&id) {
        Some(l) => format!("{} \"{}\"", obj_label(cap, id), esc(l)),
        None => obj_label(cap, id),
    }
}

/// Compact full rendering of a value (arrays and dicts expanded).
fn value_text(cap: &Capture, v: &Value) -> String {
    match v {
        Value::String(s) => cap.str(*s).to_owned(),
        Value::Number(n) => num(*n),
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|x| value_text(cap, x))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Dict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, x)| format!("{}: {}", cap.str(*k), value_text(cap, x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => value_label(cap, other),
    }
}

/// Escapes `|` and newlines for table cells and single-line list items.
fn esc(s: &str) -> String {
    s.replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Integers without `.0`; other values with 3 decimals (6 below 1), trailing zeros trimmed.
fn num(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = if v.abs() >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.6}")
    };
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// `1.500 µs` below 1 ms, else `1.500 ms`.
fn human(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.3} ms", ns as f64 / 1e6)
    } else {
        format!("{:.3} µs", ns as f64 / 1e3)
    }
}

fn dur_cell(ns: u64) -> String {
    format!("{ns} ns ({})", human(ns))
}

fn span_ns(s: &Span) -> u64 {
    s.end_ns.saturating_sub(s.start_ns)
}

fn row(out: &mut String, cells: &[String]) {
    w!(out, "| {} |", cells.join(" | "));
}

fn span_table(out: &mut String, spans: &[Span], what: &str) {
    if spans.is_empty() {
        w!(out, "_none_");
        w!(out);
        return;
    }
    w!(out, "| # | start | end | duration | {what} |");
    w!(out, "|---|---|---|---|---|");
    for s in spans {
        row(
            out,
            &[
                s.index.to_string(),
                s.start_ns.to_string(),
                s.end_ns.to_string(),
                dur_cell(span_ns(s)),
                esc(&s.label),
            ],
        );
    }
    w!(out);
}

/// Markers in chronological order; at equal start the earlier command (the parent) first.
fn sorted_markers(markers: &[Span]) -> Vec<&Span> {
    let mut v: Vec<&Span> = markers.iter().collect();
    v.sort_by_key(|m| (m.start_ns, m.index));
    v
}

fn truncate<T>(v: &[T], max: Option<usize>) -> (&[T], usize) {
    match max {
        Some(m) if v.len() > m => (&v[..m], v.len() - m),
        _ => (v, 0),
    }
}

/// Closes a (possibly truncated) list; the caller has already emitted the list.
fn omitted(out: &mut String, rest: usize, max: Option<usize>) {
    if rest > 0 {
        w!(out);
        w!(
            out,
            "… {rest} more rows omitted (max_rows = {})",
            max.unwrap_or(0)
        );
    }
    w!(out);
}
