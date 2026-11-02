# webgd user guide

webgd captures one frame of a WebGL2 or WebGPU page and lets you inspect it in a separate Chrome tab. For how it works internally see [architecture.md](architecture.md); for building from source see [development.md](development.md).

## Install

1. Build the UI once: `cargo xtask build` (see [development.md](development.md)). This fills `extension/ui/`.
2. Open `chrome://extensions`, enable **Developer mode**, click **Load unpacked** and pick the `extension/` directory.
3. Reload any page that was already open. The capture hook (`extension/hook.js`) is injected at `document_start`, so pages loaded before the extension was installed are not instrumented.

Chrome 121 or newer is required (`minimum_chrome_version` in `extension/manifest.json`).

## Open the UI

Click the webgd toolbar action ("Open webgd"). A new tab opens at `chrome-extension://<id>/ui/index.html`.

The window has a menu bar at the top, a dock of panels in the middle and a status bar at the bottom. Panels can be dragged, split and re-tabbed. Until a capture is loaded, panels show "No capture loaded. Use File > Open .webgd… or capture a live target from the Capture menu."

### Menu bar

- **File**
  - **Open .webgd…** (Ctrl+O, Cmd+O on macOS) opens a file picker and loads a saved capture.
  - **Save .webgd** downloads the current capture as `capture-<unix-ms>.webgd`. The file is the raw wire bytes ([wire-format.md](wire-format.md)). Disabled when nothing is loaded.
- **Capture**
  - **Refresh targets** asks every tab's bridge to reconnect.
  - **Show Capture panel** focuses the Capture panel.
  - One **Capture** entry per live target: the page title (or URL) and `gl2 n · gpu m`. Clicking captures the next frame of that target and focuses the Capture panel. "No targets connected" when none are listed. In standalone mode the menu only shows `standalone mode: live capture unavailable`.
- **Window** lists every panel (Events, Scrubber, Render Targets, API Inspector, Resources, Resource Viewer, GPU Trace, Log, Capture). Picking one focuses it, switching workspace if the panel lives only in the other one.
- **Frame Debugger** / **GPU Trace** switch the workspace. Each workspace keeps its own dock layout:
  - Frame Debugger: Events and Capture on the left, Render Targets and Resource Viewer in the center, API Inspector and Resources on the right, Scrubber and Log along the bottom.
  - GPU Trace: the GPU Trace timeline in the center, Events and Log below it, API Inspector and Resource Viewer on the right.
- The right side shows a capture summary: API (`WebGL2`, `WebGPU` or `WebGL2 + WebGPU`), `N events`, `N draws/dispatches`, `N resources`; `no capture` otherwise.

### Status bar

From left to right: the capture source (hover for the full URL), the current position, the replay status, the error count and the latest status message.

- Position: `Event i / N` (1-based; hover shows the call), `Pre-frame setup call #k` when a setup call is selected, `Frame end` when nothing is selected, `Frame start` for an empty frame.
- Replay status: a spinner with `replaying to …`, then `replayed to …` or `replay to … failed` (hover for the error).
- `N replay error(s)` in red; click it to open the Log.

### Keyboard shortcuts

| Key | Effect |
|---|---|
| Ctrl/Cmd+O | Open .webgd… |
| Home / End | Select the first / last event (ignored while typing in a text field) |
| Up / Down | Move the selection in Events |
| Left / Right | Step to the previous / next action in the Scrubber |
| ctrl+wheel, drag, F | Zoom, pan, fit in GPU Trace (ctrl+wheel and drag also zoom/pan the Resource Viewer texture) |
| shift+drag, Esc | Select / clear a time range in GPU Trace |

## Capture panel

- **Open .webgd…** and **Save .webgd**, as in the File menu.
- **Current capture** shows what is loaded (`source: …`) and the status line, e.g. `loaded … (N bytes): a setup / b snapshot / c frame calls`.
- **Targets** lists every page or iframe whose bridge is connected. **Refresh** asks every tab's bridge to reconnect. Each entry shows the title, `(iframe)` for subframes, `tab <id>`, the URL, and status counts:
  - `gl2: n`: live WebGL2 contexts
  - `gpuDevices: n`: live WebGPU devices
  - `webgl1: n (unsupported)`: shown only when the page uses WebGL1
- **Capture frame** is enabled when `gl2 + gpuDevices > 0`. After clicking, the entry shows "waiting for next frame…", then a progress bar `received / total bytes` while chunks arrive, then the capture loads. Failures are shown on the entry.

If no target appears: "No targets connected. Open or reload a page using WebGL2/WebGPU."

## Events

The frame's calls as a table with columns `#` (frame call index), a color strip for the command kind, **Event** (`Class#id.method(args)`), and **GPU Duration** once a trace has run.

- Rows are indented by nesting: render/compute passes, `pushDebugGroup`/`popDebugGroup` and `window.__webgd` markers form collapsible groups. Click the arrow (or double-click the row) to collapse a group.
- Draws and dispatches are bold. Rows whose call raised an error in the current replay are red; hover shows `replay error: …`.
- **Filter:** substring match on the event label.
- **Actions only** shows only draws, dispatches, clears, copies and submits, plus the groups that contain them.
- **Collapse all** / **Expand all**. The header shows `N events · M actions` and, if any, `K with errors`.
- Click a row to select it; Up/Down move the selection. The selection drives every other panel.

**Pre-frame setup.** The first row, `Pre-frame setup (N calls)`, is a collapsible group holding the setup section: the calls that created and initialised resources before the captured frame. Its rows are selectable. Selecting one replays to **frame start** (setup and snapshot applied, no frame calls), so panels show the state every setup call has produced; the status bar reads `Pre-frame setup call #k`.

Selecting a frame command replays the frame from scratch up to and including that command on webgd's own WebGL2 context / GPUDevice. Requests are latest-wins, so fast scrolling only replays the final selection.

## Scrubber

A horizontal strip with one cell per action (draw, dispatch, clear, copy, submit). Every cell has the same width until a trace has run; after that, widths follow each action's GPU duration. Marker and debug-group bands are drawn above the strip (up to 6 levels).

- Click or drag to select an action; Left/Right step between actions.
- Hover shows `#i  label` with the kind and GPU duration, or for a band its range `#a–#b · N actions` and GPU time.

## Render Targets

The attachments bound at the selected event.

- A thumbnail strip with `W×H format` per attachment. Click a thumbnail to show it in the large preview.
- The preview has **Fit** / **1:1**, **Open in Resource Viewer**, and a texel readout under the cursor ("Hover the preview to read texel values.").
- WebGL2: each draw buffer of the current draw framebuffer, or the default framebuffer. Depth/stencil attachments are not readable in WebGL2 and show a note.
- WebGPU: color attachments of the active render pass (texture, mip and layer from the view), and the depth attachment where the format is readable.

"No render targets are bound at this event." when none apply.

## API Inspector

The state that applies to the selected event, grouped by pipeline stage. The header shows the API (`WebGL2` / `WebGPU`) and the event index. Pick a stage on the left:

- Draws: **Input Assembly**, **Vertex Shader**, **Rasterizer**, **Fragment Shader**, **Depth/Stencil**, **Color Output**.
- Dispatches: **Compute Shader**, **Dispatch**.
- **All state**: the full tree (pipeline state, bindings, attachments).

Each stage shows the relevant sections, for example:

- WebGL2: Draw call, Vertex attributes, Index buffer, Program state, Uniforms & uniform blocks, Rasterizer, Viewport & scissor, Multisample, Textures & samplers, Depth, Stencil, Blend, Color mask, Draw buffers, Attachments. Enums are shown by name.
- WebGPU: Draw call, Primitive assembly, Vertex buffer layouts, Vertex & index buffers, Vertex shader, Fragment shader, Bound resources (filtered by the binding's shader visibility), Primitive state, Depth bias, Depth/stencil state, Color targets, Viewport & scissor, Stencil reference, Blend constant, Attachments. Compute stages add Compute shader, Workgroup (est.) and the Dispatch call with Totals.

Every object reference is a link that opens the resource in the Resource Viewer; attachments link to Render Targets ("Show in Render Targets"). "Nothing recorded for this stage at the selected event." when a stage is empty.

## Resources

Every GPU object in the capture, with columns:

| Column | Content |
|---|---|
| ID | `#id` |
| Type | class, e.g. `GPUTexture`, `WebGLBuffer` |
| Name/label | descriptor label or latest `label` assignment |
| Summary | size/usage, format and dimensions, shader stage |
| Origin | `pre-frame`, or a `#i` link that selects the creating command |
| Revisions | number of recorded events (see [Revision history](#revision-history)) |
| Last write | the latest write: a `#i` link for frame commands, `pre-frame #k` for setup calls |

Use the kind combo (**All kinds** or a class), **Written in frame** (only resources modified by a frame command) and the **filter** box (matches id, class, label, summary). The footer shows `N of M resources`. Click a row to open it in the Resource Viewer; double-click also selects its last write.

## Resource Viewer

Shows the selected resource as of the replay point: the selected event, frame start when a pre-frame setup call is selected, or the last command of the frame when nothing is selected. The heading shows the resource, its label and summary; `parent:` links open the parent object. Resources whose pre-frame contents could not be captured show a yellow note banner.

The header line reads `Revision r · contents at command #idx`, or `… contents at frame start` when the resource has not been touched by a frame command up to that point. A texture view says `Revision r of parent texture · …`, a resource created later in the frame says `not created yet at …`. With a pre-frame call selected the viewer adds "pre-frame call selected: replay shows the state at frame start".

**Textures** (`GPUTexture`, `GPUTextureView` → its texture, `WebGLTexture`, `WebGLRenderbuffer`):
- `mip` and `layer` (or `face` for cube maps) selectors.
- Channel toggles **R G B A**. A single enabled channel is shown as grayscale; disabled alpha displays as opaque.
- `range` min/max drag values; **Auto** sets them to the min/max of the enabled channels (NaN/Inf ignored). 8-bit normalized formats display unchanged by default; other formats map the range linearly to 0–255.
- **Fit** fits the image to the panel; ctrl+wheel zooms (0.125–32×, current zoom shown as `N×`); drag pans.
- Hovering shows `x, y: r g b a` with raw texel values.

**Buffers** (`GPUBuffer`, `WebGLBuffer`): element type combo `hex | u8 | u16 | u32 | i32 | f32 | f16`, `columns` (1–16), `byte offset`. Each row starts with its byte offset.

**Shaders** (`WebGLShader`, `GPUShaderModule`): the source (GL: latest `shaderSource` before the replay point; WebGPU: the module's `code`), the GL `Info log:`, and a **Shader stats (est.)** table per entry point: `entry`, `stage`, `ALU (est.)`, `branches`, `tex samples`, `regs (vec4)`, `workgroup`, `wg bytes`, `occupancy (est.)`. A shader that cannot be parsed shows `n/a` (hover for the error).

**Programs** (`WebGLProgram`): `Attached shaders:` (links), `Link status: ok|FAILED`, info log, **Active attributes (N)** and **Active uniforms (N)**.

Every resource also has a **Descriptor** section: the creating call and its arguments as a tree, plus the pre-frame snapshot record for GL objects. Object references are links.

### Revision history

The **Revisions** side panel (toggle it with the **Revisions** button at the right of the header) lists every recorded event that touched the resource, in capture order: all pre-frame setup events, then frame events. The title shows `N events · M writes`.

| Column | Content |
|---|---|
| Rev | content revision after this event; the revision current at the selection is marked `• r` |
| Section | `pre-frame` (setup section) or `frame` |
| Event | call index within its section, `#i` |
| Access | `create`, `write`, `read` or `destroy` |
| Call | the call label |
| Detail | what happened, e.g. `bufferSubData 64 B @ 16`, `color attachment 0 (clear)`, `vertex buffer slot 0 (draw)` |

**Numbering.** Each resource has its own counter. `create` sets it to 0, every `write` adds 1, `read` and `destroy` keep the current value. A resource whose history has no `create` (it existed before the setup log) starts at revision 0. One call produces at most one event per resource and access kind. The revision at the selected event is that of the last pre-frame event or the last frame event at or before it; a resource whose history begins with a frame `create` after the selection has no revision yet. The snapshot section never produces events; it only restores frame-start bindings for the walk.

**What counts as what.**

- **create**: any `create*` call, `getCurrentTexture`, `getBindGroupLayout`, `finish`, and resolved `create*Async` results. A repeated `getCurrentTexture` that returns a known texture counts as a read.
- **destroy**: WebGL2 `delete*`; WebGPU `destroy`.
- WebGL2 (resolved through the tracked texture units, buffer bindings, VAOs and framebuffers):
  - write: `bufferData`, `bufferSubData`, `copyBufferSubData` (destination), `texImage*`, `texSubImage*`, `texStorage*`, `copyTex*Image*`, `compressedTex*`, `generateMipmap`, `renderbufferStorage*`, `framebufferTexture2D` / `framebufferTextureLayer` / `framebufferRenderbuffer` (the framebuffer), `vertexAttribPointer` / `vertexAttribIPointer` / `enable`/`disableVertexAttribArray` (the VAO), `shaderSource`, `linkProgram`, `samplerParameter*`, `beginQuery`, and `readPixels` into a bound pixel pack buffer. Draws write every attachment of the draw framebuffer; `clear`, `clearBuffer*` and `blitFramebuffer` write the attachments selected by their mask.
  - read: `getBufferSubData`, copy sources (`copyBufferSubData`, `copyTex*Image*` read framebuffer, `blitFramebuffer` source), pixel unpack buffers, `readPixels` (read framebuffer), `useProgram`, `compileShader`, `attachShader`/`detachShader`. Draws read the bound textures and samplers on every unit, enabled vertex attribute buffers, the index buffer (`drawElements*`), uniform block buffers and the program.
- WebGPU (resolved through tracked pass and bundle encoder state and bind groups; views also add the event to their parent texture):
  - write: `beginRenderPass` color attachments (`(clear)` or `(load)`), resolve targets, and depth/stencil attachments that are not read-only; every draw and `executeBundles` again writes the pass attachments. `copyBufferToBuffer`, `copyBufferToTexture`, `copyTextureToBuffer`, `copyTextureToTexture` (destination), `clearBuffer`, `resolveQuerySet` (destination buffer), `writeBuffer`, `writeTexture`, `copyExternalImageToTexture`, mapped writes (`mapped write …`, or `unmap (mapped write)` when no bytes were recorded). Bind group entries are written when their explicit layout entry is a `storage` buffer or a non-read-only storage texture; with an auto layout, dispatches write buffers with STORAGE usage and views of textures with STORAGE_BINDING usage.
  - read: copy sources, `resolveQuerySet` query set, `mapAsync` for reading, `setPipeline`, `setBindGroup`, read-only depth/stencil attachments, vertex buffers, the index buffer (`drawIndexed*`), indirect argument buffers, other bind group entries, and bundles in `executeBundles`.

**Navigation.**

- Click any row to select that API command. Frame rows select the frame command and replay to it. Pre-frame rows select the setup call (shown in the Events `Pre-frame setup` group) and replay to frame start, so the viewer shows the frame-start contents.
- **←** (Previous write) / **→** (Next write) jump to the previous or next `write` before or after the current position, across both sections.
- For a texture view the panel shows the **Parent texture** history (which drives the header and ←/→) above **This view (…)**.

## GPU Trace

**Run trace** replays the whole frame 5 times with instrumentation and keeps the run with the median total GPU time. Notes from the run are listed above the timeline. Afterwards the Events **GPU Duration** column and the Scrubber widths use the result.

Rows, on one shared time axis with an adaptive ruler:
- One collapsible group per queue (`Queue (GPUDevice#id)` or `GL context #id`, with `(N items)`): a pass row, marker rows (`Markers`, `Markers · level d`) and an `Items` row with one bar per draw, dispatch or clear. Click a group header to collapse it.
- A collapsible `Counters (est.)` group: step graphs normalized to the track maximum (printed in the label as `max …`).

Controls (also listed above the timeline):
- ctrl+wheel zooms around the cursor, drag pans, **F** fits.
- A crosshair follows the cursor with a precise time readout on the ruler.
- Clicking a bar selects that command. Hovering shows the label, `command #i`, duration, start time and every counter value (with totals) at that time.
- **Range selection**: drag on the ruler, or shift+drag on the rows. **Zoom to range** zooms to it; **Clear range** or Esc clears it.

The **Range summary** shows the bounds, `duration`, `N items`, `item time`, a Counter / Average / Total table per counter, and the top 10 items by duration (Command / Duration / Start); clicking an item selects it.

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

## Log

Errors and notes of the latest replay: replay errors (`error`), resource notes (`info `) and warnings. Severity chips **Errors**, **Warnings**, **Notes** with counts toggle each kind; the **filter** box matches message text. Each row shows its location: `#i` for a frame command (click to select it), `pre-frame` for setup/snapshot errors, or a resource link that opens the Resource Viewer. While a replay runs the header shows `replaying to …`. GL errors are reported once per section.

## Marking up your page

The hook exposes `window.__webgd` in the page:

```js
window.__webgd?.pushMarker('Offscreen');   // optional 2nd arg: the WebGL2 context
// ... draws ...
window.__webgd?.popMarker();
window.__webgd?.insertMarker('cube');
```

Markers apply to the given context, or to the last WebGL2 context that received a call. They appear as groups in Events, as bands in the Scrubber and as marker rows in GPU Trace. WebGPU pages can use the native `pushDebugGroup` / `popDebugGroup` / `insertDebugMarker`, which webgd treats the same way. Use optional chaining so the page still runs without the extension.

## Standalone mode

The UI also runs without the extension, e.g. `python3 -m http.server 8080 --directory extension/ui` then `http://localhost:8080`. The Capture menu and panel show `standalone mode: live capture unavailable`; **File → Open .webgd…** and every inspection panel still work.

## Troubleshooting

- **No targets / page not listed.** Reload the page after installing or reloading the extension. Use **Capture → Refresh targets** (or **Refresh** in the Capture panel).
- **`target does not render with requestAnimationFrame`.** The page did not call a `requestAnimationFrame` callback within 2 s of arming. Capture needs the page to render via rAF, and the tab must be visible: Chrome throttles rAF in background tabs. Put the target and the webgd tab in separate windows.
- **`snapshot readback timed out`.** WebGPU pre-frame readbacks did not finish within 10 s.
- **Trace timings in ~65 µs steps.** Chrome quantizes WebGPU timestamps. Enable `chrome://flags/#enable-webgpu-developer-features` for full resolution.
- **`timestamp-query unsupported` / `EXT_disjoint_timer_query_webgl2 unavailable`.** The replay device lacks the timing feature; items get a nominal layout.
- **Red rows / replay errors.** Some call failed on the replay device; the message is the JS exception text (see Log, or click the error count in the status bar). Inspection state after the error may be incomplete.

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
