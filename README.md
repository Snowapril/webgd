# webgd

webgd is a frame debugger for web pages that render with WebGL2 or WebGPU. It runs inside the same Chrome instance as the target page. A Chrome MV3 extension records every graphics API call made during one `requestAnimationFrame` frame, along with the GPU resource contents at the start of that frame. A Rust/WASM UI built on egui then replays the frame on its own device, so you can inspect the state at any command, browse resources and look at a GPU timeline. Apart from three small extension scripts, all of it is written in Rust.

## Features

- **Frame capture.** Records every WebGL2/WebGPU call in one rAF frame. Pre-frame state comes from a snapshot taken at the start of that frame.
- **Command tree.** Calls are grouped by render/compute passes, debug groups and page markers. Draws and dispatches are bold, rows that hit a replay error are red, and a text filter narrows the list.
- **Inspection.** Selecting a command replays the frame up to that command on webgd's own device, then shows the bound render targets, pipeline state and bindings.
- **Resource browser.** Lists every buffer, texture, shader, program, pipeline, sampler, bind group and so on, and opens each one in a viewer:
  - textures: mip/layer selection, channel toggles, a value range with `Auto`, zoom/pan and a texel readout
  - buffers: hex, u8, u16, u32, i32, f32 or f16
  - shaders: source plus estimated per-stage statistics
  - programs and descriptor trees
  - a yellow banner on resources whose contents could not be captured
- **GPU trace.** A timeline with rows for queues, passes, markers and items, plus counter rows: Occupancy, ALU, Branches and Texture samples (all marked "(est.)"), and Fragments. Ctrl+wheel zooms, drag pans and `F` fits the view. Clicking a bar selects its command.
- **Save/Open `.webgd`.** A `.webgd` file holds the raw capture bytes. Saved captures open in standalone mode without the extension.
- **Page-side markers.** Pages can call `window.__webgd.pushMarker(name)`, `popMarker()` and `insertMarker(name)` to annotate WebGL2 frames.

## How it works

```mermaid
flowchart LR
  P[page JS] --> H[hook.js<br/>MAIN world, wraps WebGL2/WebGPU]
  H -- window.postMessage --> B[bridge.js<br/>ISOLATED world]
  B -- chrome.runtime port<br/>base64 chunks --> U[webgd-ui<br/>extension tab]
  U --> C[webgd-core<br/>wire decode, model, trackers]
  U --> R[webgd-replay<br/>replay, inspection, trace]
  U --> A[webgd-analysis<br/>shader stats, cost model]
```

`hook.js` patches the WebGL2/WebGPU prototypes at `document_start` and encodes the captured frame in the wire format. `bridge.js` forwards those bytes to the UI tab over a `chrome.runtime` port. The UI decodes them with `webgd-core`. It then replays the calls through `webgd-replay` on fresh WebGL2 contexts and GPU devices, and computes estimated counters with `webgd-analysis`. For details, see [docs/architecture.md](docs/architecture.md) and [docs/wire-format.md](docs/wire-format.md).

## Quick start

### Prerequisites

- Rust 1.99+ (edition 2024)
- `rustup target add wasm32-unknown-unknown`
- `cargo install wasm-bindgen-cli --version 0.2.130 --locked`. This must match the pinned `wasm-bindgen` crate; `cargo xtask build` checks it.
- Node.js, needed only to regenerate the wire fixture
- Chrome 121 or newer

### Build

```sh
cargo xtask build            # or: cargo xtask build --release
```

This writes `index.html`, `main.js`, `webgd_ui.js` and `webgd_ui_bg.wasm` into `extension/ui/`.

### Load the extension

1. Open `chrome://extensions` and enable Developer mode.
2. Click **Load unpacked** and select the `extension/` directory.
3. Reload any page that was already open before the extension was installed.

Branded Chrome ignores `--load-extension`. For automated setups, load the extension through CDP `Extensions.loadUnpacked` and start Chrome with `--enable-unsafe-extension-debugging`.

### Capture a fixture page

```sh
python3 -m http.server 8000 --directory tests/pages
```

1. Open `http://localhost:8000/webgpu.html` or `http://localhost:8000/webgl2.html` in a visible tab.
2. Click the webgd toolbar action. The webgd UI opens in a new tab.
3. In the **Capture** tab, click **Capture** next to the target.
4. Select a command in **Commands** to see it in **Render Targets**, **Pipeline State** and **Bindings**. Browse resources in **Resources**, or click **Run trace** in **Trace**.

[docs/user-guide.md](docs/user-guide.md) covers each panel.

## Standalone mode

The built UI can run without the extension to view saved captures:

```sh
python3 -m http.server 8080 --directory extension/ui
```

Open `http://localhost:8080` and use **Open .webgd…**. Live capture is disabled, and the Capture tab shows `standalone mode: live capture unavailable`.

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/webgd-core` | Capture model, wire decoder (`wire.rs`), call labels (`display.rs`), pixel decoding (`pixels.rs`), state trackers (`track/`). Native-testable. |
| `crates/webgd-analysis` | WGSL (naga) and GLSL (glsl-lang) static analysis; cost model in `model.rs`. Native-testable. |
| `crates/webgd-replay` | WASM only. Replay engine (`engine.rs`), JS helpers, snapshot application, GL/GPU inspection (`gl/`, `gpu/`), trace runner (`trace.rs`). |
| `crates/webgd-ui` | WASM cdylib. eframe 0.36 + egui_dock 0.21 app on the wgpu backend: extension port client (`port.rs`), replay scheduling (`replay_queue.rs`), panels (`panels/`). |
| `xtask` | `cargo xtask build [--release]`: builds the UI to WASM, runs wasm-bindgen, copies the host page. |
| `extension/` | `manifest.json`, `hook.js`, `bridge.js`, `background.js`. `extension/ui/` is build output and is gitignored. |
| `web/` | UI host page templates (`index.html`, `main.js`), copied into `extension/ui/` at build time. |
| `tests/pages/` | Fixture target pages: `webgl2.html`, `webgpu.html`, `common.js`. |
| `tests/fixtures/sample.wire` | Wire-format fixture produced by `tools/gen-wire-fixture.cjs`. |
| `tools/gen-wire-fixture.cjs` | Generates the fixture with the same `WireWriter` that `hook.js` uses. |

## Testing

```sh
cargo test                                                          # native crates (webgd-core, webgd-analysis, xtask)
cargo clippy -p webgd-replay -p webgd-ui --target wasm32-unknown-unknown
node tools/gen-wire-fixture.cjs && git diff --exit-code tests/fixtures/sample.wire
cargo xtask build
```

The fixture check confirms that the JS encoder in `hook.js` and the Rust decoder still agree byte for byte. [docs/development.md](docs/development.md) describes the full workflow.

## Known limitations

- Counters are estimates from a static cost model. Timings are measured on webgd's replay device, not on the page's own submission.
- Chrome quantizes WebGPU timestamps (about 65 µs) unless `chrome://flags/#enable-webgpu-developer-features` is enabled.
- WebGPU occlusion results in Chrome are 0/1 visibility flags, so `Fragments (post-depth)` is 0 or 1.
- `depth24plus` textures cannot be viewed. Pre-frame depth/stencil and MSAA contents are not captured; such resources show a banner.
- WebGL1 is detected but not supported. Workers and OffscreenCanvas in workers are not supported.
- Capture needs a visible tab driven by `requestAnimationFrame`. Otherwise it fails with `target does not render with requestAnimationFrame`.

[docs/user-guide.md](docs/user-guide.md) has the full list.

## Documentation

- [docs/architecture.md](docs/architecture.md): components, data flow, replay design
- [docs/wire-format.md](docs/wire-format.md): capture byte format (`.webgd` files)
- [docs/user-guide.md](docs/user-guide.md): installing, capturing, panels, limitations
- [docs/development.md](docs/development.md): toolchain, building, testing, fixtures
- [docs/index.html](docs/index.html): project report

## License

MIT. See [LICENSE](LICENSE).
