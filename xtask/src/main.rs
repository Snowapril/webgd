use std::path::{Path, PathBuf};
use std::process::{Command, exit};

const WASM_BINDGEN_VERSION: &str = "0.2.130";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("failed to run {cmd:?}: {e}");
        exit(1)
    });
    if !status.success() {
        eprintln!("command failed: {cmd:?}");
        exit(status.code().unwrap_or(1));
    }
}

fn build(release: bool) {
    let root = root();
    let ok = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(WASM_BINDGEN_VERSION))
        .unwrap_or(false);
    if !ok {
        eprintln!("wasm-bindgen-cli {WASM_BINDGEN_VERSION} required");
        exit(1);
    }
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(&root).args(["build", "-p", "webgd-ui", "--target", "wasm32-unknown-unknown"]);
    if release {
        cargo.arg("--release");
    }
    run(&mut cargo);
    let profile = if release { "release" } else { "debug" };
    let out = root.join("extension/ui");
    std::fs::create_dir_all(&out).unwrap();
    run(Command::new("wasm-bindgen")
        .current_dir(&root)
        .arg(format!("target/wasm32-unknown-unknown/{profile}/webgd_ui.wasm"))
        .args(["--target", "web", "--no-typescript", "--out-dir", "extension/ui", "--out-name", "webgd_ui"]));
    for f in ["index.html", "main.js"] {
        std::fs::copy(root.join("web").join(f), out.join(f)).unwrap();
    }
    println!("built {}", out.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build(args.iter().any(|a| a == "--release")),
        _ => {
            eprintln!("usage: cargo xtask build [--release]");
            exit(2);
        }
    }
}
