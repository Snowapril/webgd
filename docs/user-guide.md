# webgd user guide

webgd captures one frame of a WebGL2 or WebGPU page and lets you inspect it in a separate Chrome tab. For how it works internally see [architecture.md](architecture.md); for building from source see [development.md](development.md).

## Install

1. Build the UI once: `cargo xtask build` (see [development.md](development.md)). This fills `extension/ui/`.
2. Open `chrome://extensions`, enable **Developer mode**, click **Load unpacked** and pick the `extension/` directory.
3. Reload any page that was already open. The capture hook (`extension/hook.js`) is injected at `document_start`, so pages loaded before the extension was installed are not instrumented.

Chrome 121 or newer is required (`minimum_chrome_version` in `extension/manifest.json`).

## Open the UI

Click the webgd toolbar action ("Open webgd"). A new tab opens at `chrome-extension://<id>/ui/index.html`. The UI is a dock of tabs: **Capture**, **Commands**, **Log**, **Render Targets**, **Pipeline State**, **Bindings**, **Resources**, **Resource Viewer**, **Trace**. Tabs can be dragged and split.

Panels other than Capture show "No capture loaded. Capture a live target or open a .webgd file in the Capture tab." until a capture exists.

## Capture tab

- **Open .webgd…** opens a file picker and loads a saved capture.
- **Save .webgd** downloads the current capture as `capture-<unix-ms>.webgd`. The file is the raw wire bytes ([wire-format.md](wire-format.md)). Disabled when nothing is loaded.
- "Current capture: <url>" shows what is loaded; the status line reports e.g. `loaded … (N bytes): a setup / b snapshot / c frame calls`.
- **Targets** lists every page or iframe whose bridge is connected. **Refresh** asks every tab's bridge to reconnect. Each entry shows the title, `(iframe)` for subframes, `tab <id>`, the URL, and status counts:
  - `gl2: n` — live WebGL2 contexts
  - `gpuDevices: n` — live WebGPU devices
  - `webgl1: n (unsupported)` — shown only when the page uses WebGL1
- **Capture** is enabled when `gl2 + gpuDevices > 0`. After clicking, the entry shows "waiting for next frame…", then a progress bar `received / total bytes` while chunks arrive, then the capture loads. Failures are shown on the entry.

If no target appears: "No targets connected. Open or reload a page using WebGL2/WebGPU."

## Commands

The frame's calls as a tree.

- Rows are indented by nesting: render/compute passes, `pushDebugGroup`/`popDebugGroup`, and `window.__webgd` markers form collapsible groups (⏵/⏷).
- Draw and dispatch calls are bold. Rows whose call raised an error during the current replay are red.
- **Filter:** substring match on the call label (`Class#id.method(args)`).
- **Collapse all** / **Expand all**; the header shows the call count.
- Click a row to select it; Up/Down arrows move the selection. Selection drives every inspection panel and the Resource Viewer.

Selecting a command replays the frame from scratch up to and including that command on webgd's own WebGL2 context / GPUDevice. Requests are latest-wins, so fast scrolling only replays the final selection.

## Log

Shows the result of the last replay: `replaying to #N…`, then `replay to #N: k error(s)` and one line per error. Frame-section errors link to their command (`#i`); errors from setup or snapshot application are tagged `[setup/snapshot]`. GL errors are reported once per section. "no replay yet" until a command is selected.

## Render Targets

Thumbnails of the attachments bound at the selected command, with `W×H Format`. Click a thumbnail ("Open in Resource Viewer") to open the backing resource.

- WebGL2: each draw buffer of the current draw framebuffer, or the default framebuffer. Depth/stencil attachments are not readable in WebGL2 and show a note.
- WebGPU: color attachments of the active render pass (texture, mip and layer from the view), and the depth attachment where the format is readable.

"No render targets bound for this command." when none apply.

## Pipeline State

Collapsible tree of the state that applies to the selected command.

- WebGL2: groups such as Program, Rasterizer, Depth, Stencil, Blend, ColorMask, Viewport/Scissor, Multisample, DrawBuffers, read from the replay context; enums are shown by name.
- WebGPU: the pipeline's creation descriptor, followed by a Dynamic group (viewport, scissor, blend constant, stencil reference).

## Bindings

A table grouped by binding group. Click a resource to open it in the Resource Viewer.

- WebGL2: attributes of the bound VAO, index buffer, uniforms, uniform blocks, and textures/samplers per sampler uniform.
- WebGPU: each bind group's entries (buffer + offset/size, texture view, sampler, external texture) with dynamic offsets and layout info (`auto layout` for `getBindGroupLayout` layouts), then vertex buffer slots and the index buffer.

## Resources

Every GPU object in the capture: columns `id`, `class`, `summary` (size/usage, format and dimensions, shader stage), `label` (descriptor label or latest `label` assignment), `origin` (`pre-frame` or a `frame #i` link that selects the creating command).

Use the kind combo (**All kinds** or a class) and the **filter** box (matches id, class, label, summary). Clicking a row opens it in the Resource Viewer.

## Resource Viewer

Shows the selected resource as of the replay point: the selected command, or the last command of the frame when nothing is selected. The header reads `contents at command #N`. Object links (e.g. `parent:`) open other resources. Resources whose pre-frame contents could not be captured show a yellow note banner.

**Textures** (`GPUTexture`, `GPUTextureView` → its texture, `WebGLTexture`, `WebGLRenderbuffer`):
- `mip` and `layer` (or `face` for cube maps) selectors.
- Channel toggles **R G B A**. A single enabled channel is shown as grayscale; disabled alpha displays as opaque.
- `range` min/max drag values; **Auto** sets them to the min/max of the enabled channels (NaN/Inf ignored). 8-bit normalized formats display unchanged by default; other formats map the range linearly to 0–255.
- **Fit** fits the image to the panel; ctrl+wheel zooms (0.125–32×, current zoom shown as `N×`); drag pans.
- Hovering shows `x, y: r g b a` with raw texel values.

**Buffers** (`GPUBuffer`, `WebGLBuffer`): element type combo `hex | u8 | u16 | u32 | i32 | f32 | f16`, `columns` (1–16), `byte offset`. Each row starts with its byte offset.

**Shaders** (`WebGLShader`, `GPUShaderModule`): the source (GL: latest `shaderSource` before the replay point; WebGPU: the module's `code`), the GL info log, and a **Shader stats (est.)** table per entry point: `entry`, `stage`, `ALU (est.)`, `branches`, `tex samples`, `regs (vec4)`, `workgroup`, `wg bytes`, `occupancy (est.)`. A shader that cannot be parsed shows `n/a` (hover for the error).

**Programs** (`WebGLProgram`): attached shaders (links), `Link status`, info log, **Active attributes** and **Active uniforms**.

Every resource also has a **Descriptor** section: the creating call ("created by …") and its arguments as a tree, plus the pre-frame snapshot record for GL objects. Object references are links.

## Trace

**Run trace** replays the whole frame 5 times with instrumentation ("tracing (5 instrumented replays)…") and keeps the run with the median total GPU time. Notes from the run are listed above the timeline.

Rows, left to right on one shared time axis:
- Per queue (`Queue (GPUDevice#id)` or `GL context #id`): a pass row, marker rows `markers [depth]`, and an `Items` row with one bar per draw, dispatch or clear.
- Counter rows, drawn as step graphs normalized to the track maximum (printed in the label as `max …`).

Controls: ctrl+wheel zooms around the cursor, drag pans, **F** fits, clicking a bar selects that command in Commands. Hovering shows the label, command index, duration, start time and every counter value (with totals) at that time.

### Counters

All shader counters are estimates from webgd's static cost model (`crates/webgd-analysis/src/model.rs`), not hardware counters. Per item, `vinv` is vertex invocations (GL: `count × instanceCount`; WebGPU: `vertexCount`/`indexCount × instanceCount`, indirect args read back from the GPU), `frags` is the item's fragment count, and `threads` is `workgroups × workgroup size`. Duration is the item's measured span (minimum 1 ns).

| Row | Unit | Value |
|---|---|---|
| `Occupancy (est.)` | % | Estimated occupancy of the fragment shader (vertex if none) for draws, compute shader for dispatches: `min(16, floor(512 / max(16, regs_vec4·4)), lds_waves) / 16`, with `lds_waves` limited by workgroup memory for compute |
| `ALU ops/ns (est.)` | ops/ns | `(vs.alu·vinv + fs.alu·frags) / duration`, or `cs.alu·threads / duration` |
| `Branches/ns (est.)` | branches/ns | same formula with branch counts |
| `Texture samples/ns (est.)` | samples/ns | same formula with texture sample/load counts |
| `Fragments (post-depth)` | fragments | WebGPU: occlusion query result per draw divided by sample count |
| `Fragments (rasterized, pre-depth)` | fragments | WebGL2: fragments counted by an untimed extra replay that redraws each item with additive blending into an R16F target, depth/stencil test disabled |

Cost model: arithmetic and comparisons cost one op per result component; transcendental builtins 4 per component; loops assume 8 iterations; each `if`/`switch`/loop exit counts as a branch; the more expensive branch arm is charged.

Timing comes from WebGPU `timestamp-query` (each draw/dispatch is split into its own pass segment) or WebGL `EXT_disjoint_timer_query_webgl2`. WebGL items are packed sequentially from 0 ("WebGL: sequential layout, no GPU timestamps").

## Marking up your page

The hook exposes `window.__webgd` in the page:

```js
window.__webgd?.pushMarker('Offscreen');   // optional 2nd arg: the WebGL2 context
// ... draws ...
window.__webgd?.popMarker();
window.__webgd?.insertMarker('cube');
```

Markers apply to the given context, or to the last WebGL2 context that received a call. They appear as groups in Commands and as marker rows in Trace. WebGPU pages can use the native `pushDebugGroup` / `popDebugGroup` / `insertDebugMarker`, which webgd treats the same way. Use optional chaining so the page still runs without the extension.

## Standalone mode

The UI also runs without the extension, e.g. `python3 -m http.server 8080 --directory extension/ui` then `http://localhost:8080`. The Capture tab shows `standalone mode: live capture unavailable`; **Open .webgd…** and every inspection panel still work.

## Troubleshooting

- **No targets / page not listed.** Reload the page after installing or reloading the extension. Use **Refresh** in the Capture tab.
- **`target does not render with requestAnimationFrame`.** The page did not call a `requestAnimationFrame` callback within 2 s of arming. Capture needs the page to render via rAF, and the tab must be visible: Chrome throttles rAF in background tabs. Put the target and the webgd tab in separate windows.
- **`snapshot readback timed out`.** WebGPU pre-frame readbacks did not finish within 10 s.
- **Trace timings in ~65 µs steps.** Chrome quantizes WebGPU timestamps. Enable `chrome://flags/#enable-webgpu-developer-features` for full resolution.
- **`timestamp-query unsupported` / `EXT_disjoint_timer_query_webgl2 unavailable`.** The replay device lacks the timing feature; items get a nominal layout.
- **Red rows / Log errors.** Some call failed on the replay device; the message is the JS exception text. Inspection state after the error may be incomplete.

## Limitations

- Counters are estimates from a static cost model, labelled `(est.)`. Timings are measured on webgd's replay of the frame, on the same GPU and browser but not the page's own submission.
- Chrome quantizes WebGPU timestamps (~65 µs) unless `chrome://flags/#enable-webgpu-developer-features` is enabled.
- WebGPU occlusion results in Chrome are 0/1 visibility flags, so `Fragments (post-depth)` is 0 or 1 (a note says so).
- Passes using render bundles or occlusion queries are timed as a whole; their items share the pass span ("unsplit pass").
- Independent WebGPU passes may show overlapping timestamps because the GPU runs them concurrently.
- `depth24plus` textures are not viewable.
- Pre-frame depth/stencil and MSAA contents are not captured; replay starts them zeroed and the resource shows a banner.
- GL errors are reported once per section.
- WebGL1 contexts are detected and reported but not supported.
- Workers and OffscreenCanvas in workers are not supported.
- Capture needs a visible tab driven by `requestAnimationFrame`.
- Each replayed `requestAdapter` gets a fresh adapter on webgd's side.
