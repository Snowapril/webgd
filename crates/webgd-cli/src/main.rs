//! `webgd` command-line tool: exports `.webgd` captures as Markdown.

use std::process::ExitCode;

use webgd_core::export::{CaptureExportOptions, capture_markdown};

const USAGE: &str = "\
webgd - export webgd frame captures as Markdown

USAGE:
    webgd capture <file.webgd> [OPTIONS]
    webgd help

CAPTURE OPTIONS:
    --setup           include pre-frame setup calls
    --no-commands     omit the frame command list
    --no-resources    omit the resource table and revision histories
    --no-draw-state   omit per-draw/dispatch static state
    --max-rows N      truncate each list to N entries
    -o <out.md>       write to a file instead of stdout

The CLI only reads .webgd captures. GPU traces (timeline, counters, notes)
require a GPU replay in the browser UI; export them as Markdown from the UI's
GPU Trace panel.
";

enum Failure {
    Usage(String),
    Io(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(msg)) => {
            eprintln!("webgd: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Io(msg)) => {
            eprintln!("webgd: {msg}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<(), Failure> {
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("capture") => capture(&args[1..]),
        Some(other) => Err(Failure::Usage(format!("unknown command `{other}`"))),
    }
}

fn capture(args: &[String]) -> Result<(), Failure> {
    let mut opts = CaptureExportOptions::default();
    let mut input: Option<&str> = None;
    let mut output: Option<&str> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--setup" => opts.setup = true,
            "--no-commands" => opts.commands = false,
            "--no-resources" => opts.resources = false,
            "--no-draw-state" => opts.draw_state = false,
            "--max-rows" => {
                let v = it
                    .next()
                    .ok_or_else(|| Failure::Usage("--max-rows needs a value".into()))?;
                let n = v
                    .parse::<usize>()
                    .map_err(|_| Failure::Usage(format!("invalid --max-rows value `{v}`")))?;
                opts.max_rows = Some(n);
            }
            "-o" => {
                let v = it
                    .next()
                    .ok_or_else(|| Failure::Usage("-o needs a path".into()))?;
                output = Some(v);
            }
            s if s.starts_with('-') => {
                return Err(Failure::Usage(format!("unknown option `{s}`")));
            }
            s => {
                if input.replace(s).is_some() {
                    return Err(Failure::Usage("more than one input file given".into()));
                }
            }
        }
    }
    let input = input.ok_or_else(|| Failure::Usage("missing <file.webgd>".into()))?;
    let bytes =
        std::fs::read(input).map_err(|e| Failure::Io(format!("cannot read {input}: {e}")))?;
    let cap = webgd_core::wire::decode(&bytes)
        .map_err(|e| Failure::Io(format!("cannot decode {input}: {e}")))?;
    let md = capture_markdown(&cap, &opts);
    match output {
        Some(path) => {
            std::fs::write(path, md).map_err(|e| Failure::Io(format!("cannot write {path}: {e}")))
        }
        None => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(md.as_bytes())
                .and_then(|()| out.flush())
                .map_err(|e| Failure::Io(format!("cannot write stdout: {e}")))
        }
    }
}
