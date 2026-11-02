# webgd wire format

A webgd capture is a single byte stream. `WireWriter` in `extension/hook.js` writes it, and
`webgd_core::wire::decode` in `crates/webgd-core/src/wire.rs` reads it. A `.webgd` file holds
the same bytes unchanged: the UI's `Save .webgd` writes exactly what the hook produced. No other
format exists.

`tests/fixtures/sample.wire` keeps the two implementations in sync. `tools/gen-wire-fixture.cjs`
regenerates it from the JS writer, and `crates/webgd-core/tests/wire.rs` decodes it. The
[worked example](#worked-example-testsfixturessamplewire) below walks through it byte by byte.
For how records are produced and consumed, see [architecture.md](architecture.md).

## Primitives

All integers are little-endian.

| Name | Encoding |
| --- | --- |
| `u8` | 1 byte |
| `u32` | 4 bytes LE |
| `f64` | 8 bytes LE IEEE-754 |
| `string` | `u32 byteLen` followed by `byteLen` bytes of UTF-8 |

## Header

| Offset | Field | Value |
| --- | --- | --- |
| 0 | magic | ASCII `WGDW` (`57 47 44 57`) |
| 4 | `u32 version` | `1` (`wire::VERSION`) |
| 8 | `string url` | `location.href` of the captured frame |

The decoder fails with `BadMagic` if the magic does not match, and with
`UnsupportedVersion(v)` for any other version.

## Records

After the header comes a sequence of records. Each one starts with a `u8` tag.

| Tag | Record | Body |
| --- | --- | --- |
| `0x01` | Str | `u32 strId`, `string` |
| `0x02` | Object | `u32 id`, `u32 classStrId`, `u32 parentId` (0 = none) |
| `0x03` | Blob | `u32 blobId`, `u32 len`, `len` raw bytes |
| `0x04` | Call | `u8 section`, `u32 thisId`, `u32 methodStrId`, `u32 argc`, `Value × argc`, `Value result` |
| `0xFF` | End | none |

Call sections:

| `section` | Name | `Capture` field |
| --- | --- | --- |
| 0 | setup | `cap.setup` |
| 1 | snapshot | `cap.snapshot` |
| 2 | frame | `cap.frame` |

The decoder stops at the first `0xFF` and ignores any bytes after it. A tag that is not a known
record tag or section number fails with `UnknownTag { offset, tag }`.

## Values

Each value starts with a `u8` tag.

| Tag | Value | Body | Rust `Value` |
| --- | --- | --- | --- |
| 0 | Undefined | none | `Undefined` |
| 1 | Null | none | `Null` |
| 2 | False | none | `Bool(false)` |
| 3 | True | none | `Bool(true)` |
| 4 | Number | `f64` | `Number(f64)` |
| 5 | String | `u32 strId` | `String(Sym)` |
| 6 | ObjRef | `u32 id` (must be non-zero) | `ObjRef(ObjId)` |
| 7 | Typed | `u8 kind`, `u32 blobId` | `Typed { kind, blob }` |
| 8 | Array | `u32 n`, `Value × n` | `Array(Vec<Value>)` |
| 9 | Dict | `u32 n`, then `n × (u32 keyStrId, Value)` | `Dict(Vec<(Sym, Value)>)` |
| 10 | Image | `u32 width`, `u32 height`, `u32 blobId` | `Image { width, height, blob }` |
| 11 | Pending | `u32 token` | `Pending(u32)` |
| 12 | Opaque | `u32 classStrId` | `Opaque(Sym)` |

The JS writer encodes `bigint` as Number. Dict keys keep their insertion order.

### Typed kinds

The blob holds a copy of the view's bytes: `byteOffset .. byteOffset + byteLength` of the
original buffer. An `ArrayBuffer` is copied whole.

| Kind | Type | Element size |
| --- | --- | --- |
| 0 | ArrayBuffer | 1 |
| 1 | Int8Array | 1 |
| 2 | Uint8Array | 1 |
| 3 | Uint8ClampedArray | 1 |
| 4 | Int16Array | 2 |
| 5 | Uint16Array | 2 |
| 6 | Int32Array | 4 |
| 7 | Uint32Array | 4 |
| 8 | Float32Array | 4 |
| 9 | Float64Array | 8 |
| 10 | DataView | 1 |
| 11 | BigInt64Array | 8 |
| 12 | BigUint64Array | 8 |
| 13 | Float16Array | 2 |

### Images

Image blobs hold `width × height` RGBA8 pixels in row-major order, non-premultiplied, as
returned by `getImageData`. `ImageData` sources are copied directly. `HTMLImageElement`,
`HTMLCanvasElement`, `OffscreenCanvas`, `ImageBitmap`, `HTMLVideoElement` and `VideoFrame` are
drawn at their natural size onto a scratch `OffscreenCanvas` and read back. A source that cannot
be drawn is encoded as `Opaque(className)`. A zero-sized source is encoded as a 0×0 image.

### Other conversions in `conv()`

- An object that already has an id becomes an ObjRef. An unknown object whose class name starts with `GPU` or `WebGL` gets a new id and becomes an ObjRef.
- Plain objects and null-prototype objects become a Dict over their own enumerable keys.
- `GPUSupportedLimits` becomes a Dict of its numeric members. `GPUSupportedFeatures` and `Set` become an Array.
- Functions, symbols, detached buffers, values nested deeper than 32 levels, and any other object become `Opaque(className)`.
- `GPUBuffer.getMappedRange` results are always recorded as `Opaque`. The mapped bytes are sent separately as `webgd:mappedWrite`.

## Invariants

The decoder enforces these rules. A violation fails with `DanglingRef { offset }`, where offset
is the byte offset of the reference.

- A `strId` used in an Object record, a Call method, a String value, a Dict key or an Opaque value must have been defined by an earlier Str record.
- A `blobId` used by Typed or Image must have been defined by an earlier Blob record.
- Object references must point to an earlier Object record: Call `thisId`, ObjRef values, and a non-zero Object `parentId`. Id 0 is allowed only as `thisId` and `parentId`, where it means "no object".

The writer guarantees more than the decoder checks:

- String ids are dense from 0, in first-use order. Each distinct string is emitted once.
- Blob ids are dense from 0. Every Typed or Image value gets its own blob, emitted just before the Call that uses it.
- Object ids come from the hook's `objectTable`, which is dense from 1 and never pruned. `encode()` emits **every** object before the first Call, and a parent is always emitted before its child.
- Calls appear in section order: all setup records, then all snapshot records, then all frame records, then End.

If the input ends early, the decoder fails with `Truncated { offset }`, where offset is the input
length, and never panics. `crates/webgd-core/tests/wire.rs` checks this for every prefix of the
fixture. Invalid UTF-8 in a string is reported as `UnknownTag { offset, tag: 0 }`.

## Calls and pseudo-calls

A Call record either mirrors a real API call or is a pseudo-call. A real call holds `this`, the
method name, the arguments converted by `conv()`, and the result. Results that are API objects
become ObjRefs whose object has `this` as parent. Promise results are recorded as
`Pending(token)`.

Pseudo-calls use method names that are not real API methods. Two families exist:

- accessor records, named `get:*` and `set:*`
- webgd records, named `webgd:*`

### Accessor records

| Method | Section | `this` | Args | Result | Consumer |
| --- | --- | --- | --- | --- | --- |
| `get:queue` | setup, frame | `GPUDevice` | none | ObjRef `GPUQueue` | Replay maps the id to `device.queue`. Recorded once per device. |
| `set:label` | setup, frame | any `GPU*` object | `[label]` | Undefined | Replay sets `label`. The resource list uses it for its label column. |
| `set:width` / `set:height` | setup, frame | `HTMLCanvasElement` / `OffscreenCanvas` with an id | `[Number]` (new value) | Undefined | Replay resizes the OffscreenCanvas. |

`getContext` is a real call. Its `this` is the canvas, and its result is the context ObjRef.

### `webgd:*` records outside the snapshot

| Method | Section | `this` | Args | Producer | Consumer |
| --- | --- | --- | --- | --- | --- |
| `webgd:resolved` | setup, frame | `this` of the call that returned the promise | `[Number token, Value resolved]` | Promise `then` handler in `trackPromise`. Goes into the frame section if capturing, and into the setup log if the issuing call was logged. | Replay maps an ObjRef `resolved` to the value it stored for `token`. |
| `webgd:bindForUpload` | setup | `WebGL2RenderingContext` | `[Number bindTarget, ObjRef texture]`. Cube face targets are already mapped to `TEXTURE_CUBE_MAP` (34067). | Shadow-class upload logging (`glUpload`) | Replay runs `activeTexture(TEXTURE0 + scratchUnit)` and `bindTexture`. `track::gl` treats it as a bind on a separate upload unit. |
| `webgd:pixelStore` | setup | `WebGL2RenderingContext` | `[Dict]`. Keys are decimal pnames: `3317` UNPACK_ALIGNMENT, `3314` UNPACK_ROW_LENGTH, `3315` UNPACK_SKIP_ROWS, `3316` UNPACK_SKIP_PIXELS, `32878` UNPACK_IMAGE_HEIGHT, `32877` UNPACK_SKIP_IMAGES, `37440` UNPACK_FLIP_Y_WEBGL, `37441` UNPACK_PREMULTIPLY_ALPHA_WEBGL, `37443` UNPACK_COLORSPACE_CONVERSION_WEBGL. | Same, read with `ORIG getParameter` just before the upload | Replay calls `pixelStorei(pname, value)` for each entry. |
| `webgd:mappedWrite` | frame | `GPUBuffer` | `[Number offset, Typed Uint8 bytes]`, one record per `getMappedRange` range | `GPUBuffer.unmap` wrapper, emitted before the `unmap` record, only while capturing, for buffers mapped for writing (`mappedAtCreation` or `mapAsync(MAP_WRITE)`) | Replay copies the bytes into the `ArrayBuffer` that replay's `getMappedRange` returned at the same offset. |
| `webgd:pushMarker` | frame | `ctx` argument, else the last WebGL2 context that received a call, else 0 | `[String name]` | `window.__webgd.pushMarker(name, ctx?)` | Command tree group, trace marker. Ignored by replay. |
| `webgd:popMarker` | frame | same | none | `window.__webgd.popMarker(ctx?)` | Closes the group opened on the same `this`. |
| `webgd:insertMarker` | frame | same | `[String name]` | `window.__webgd.insertMarker(name, ctx?)` | Zero-width trace marker. |

Two more frame records come from GL wrappers that need special handling:

- `getError` while capturing returns, and records, the GL errors the snapshot drained before the frame. Only after those does it call the original.
- Uploads (`texImage*`, `texSubImage*`, `compressedTex*`, `copyTex*`, `texStorage*`) are recorded like normal calls.

### Labels created only in replay

The trace runner and the replay engine label their own GPU objects `webgd:canvas`,
`webgd:readback`, `webgd:ts-resolve`, `webgd:occ-resolve`, `webgd:query-readback` and
`webgd:indirect`. These labels exist only on replay objects and never appear in a capture.

## Snapshot records

Section 1 holds the frame-start state. It is written in this order:

1. For each live WebGL2 context, with `this` = the context:
   1. `webgd:glTexture` ×n
   2. `webgd:glBuffer` ×n
   3. `webgd:glRenderbuffer` ×n
   4. `webgd:glFramebuffer` ×n
   5. `webgd:glSampler` ×n
   6. `webgd:glVertexArray` ×n, the default VAO first
   7. `webgd:glProgram` ×n, linked programs only
   8. `webgd:glDefaultFramebuffer`
   9. `webgd:glState`
2. `webgd:gpuBuffer` / `webgd:gpuTexture`, one per live WebGPU buffer and texture, in setup-log order, with `this` = the resource.
3. `webgd:canvasSize`, one per canvas with an id, with `this` = the canvas.

All snapshot records have result Undefined. "Live" means the object's creation entry is still in
the pruned setup log, its object is still reachable, and it has not been deleted (GL) or
destroyed (GPU).

GL dict keys that are GL enums are written as decimal strings. For example, `"34016"` is
`ACTIVE_TEXTURE` (0x84E0). Values that are WebGL objects are ObjRefs, or Null when nothing is
bound.

### `webgd:glTexture(ObjRef texture, Dict)`

| Key | Value |
| --- | --- |
| `target` | Bind target: `TEXTURE_2D`, `TEXTURE_CUBE_MAP`, `TEXTURE_3D` or `TEXTURE_2D_ARRAY` |
| `internalformat` | Number, or Null if the texture was never defined |
| `format`, `type` | Number from the last `texImage*` definition. Null for `texStorage*`, `copyTexImage2D` and `compressedTexImage*` definitions. |
| `immutable` | Bool (`TEXTURE_IMMUTABLE_FORMAT`) |
| `immutableLevels` | Number (`TEXTURE_IMMUTABLE_LEVELS`) |
| `params` | Dict of `getTexParameter`: `33084` BASE_LEVEL, `33085` MAX_LEVEL, `10240` MAG_FILTER, `10241` MIN_FILTER, `10242` WRAP_S, `10243` WRAP_T, `32882` WRAP_R, `34892` COMPARE_MODE, `34893` COMPARE_FUNC, `33082` MIN_LOD, `33083` MAX_LOD |
| `levels` | Array of level Dicts, sorted by level then face. One entry per layer for 3D and 2D-array textures. |

Each level Dict:

| Key | Value |
| --- | --- |
| `level`, `face`, `layer` | Number. Face is 0–5 for cube maps and 0 otherwise. |
| `width`, `height`, `depth` | Number |
| `readClass` | `"unorm"`, `"srgb"`, `"float"`, `"int"` or `"uint"` |
| `data` | `readPixels` of the full level as RGBA in GL row order (bottom row first): Uint8Array for unorm and srgb, Float32Array for float, Int32Array for int, Uint32Array for uint. Null if not captured. |
| `note` | Null, `"uploadLog"` (shadow class: the setup section re-uploads the contents) or `"notCaptured"` (depth/stencil, uncapturable shadow texture, zero size, incomplete attachment or read error) |

### `webgd:glBuffer(ObjRef buffer, Dict)`

`{size, usage, isIndex, data}`:

- `isIndex` is Bool: the buffer was first bound to `ELEMENT_ARRAY_BUFFER`.
- `data` is a Uint8Array from `getBufferSubData`, or Null when the size is 0 or the read fails.

Only buffers that have been bound at least once are recorded.

### `webgd:glRenderbuffer(ObjRef renderbuffer, Dict)`

`{width, height, internalformat, samples, data, readClass}`. `data` is read only for
single-sample color formats, with the same classes and layout as texture levels. Otherwise it is
Null.

### `webgd:glFramebuffer(ObjRef framebuffer, Dict)`

| Key | Value |
| --- | --- |
| `attachments` | Dict keyed by the decimal attachment enum (`COLOR_ATTACHMENTi`, `36096` DEPTH_ATTACHMENT, `36128` STENCIL_ATTACHMENT, `33306` DEPTH_STENCIL_ATTACHMENT). Each value is `{objectType, object, level, face, layer}`. Attachments of type `NONE` are omitted. |
| `drawBuffers` | Array of Number, `DRAW_BUFFERi` for `i < MAX_DRAW_BUFFERS` |
| `readBuffer` | Number |

### `webgd:glSampler(ObjRef sampler, Dict)`

Dict of `getSamplerParameter`: `10241` MIN_FILTER, `10240` MAG_FILTER, `10242` WRAP_S, `10243`
WRAP_T, `32882` WRAP_R, `33082` MIN_LOD, `33083` MAX_LOD, `34892` COMPARE_MODE, `34893`
COMPARE_FUNC.

### `webgd:glVertexArray(ObjRef vao | Null, Dict)`

The first argument is Null for the default VAO. The Dict has two keys:

- `attribs`: Array with one Dict per `i < MAX_VERTEX_ATTRIBS`: `{enabled, size, stride, type, normalized, integer, divisor, buffer, offset}`.
- `elementBuffer`: ObjRef or Null.

### `webgd:glProgram(ObjRef program, Array uniforms, Dict blockBindings)`

- `uniforms` covers each active uniform outside a uniform block. Arrays are expanded to `name[i]`. Each entry is an Array `[String name, Number type, Value value]`, with the value from `getUniform`: a Number, a Bool, or a typed array for vectors and matrices.
- `blockBindings` maps each uniform block name to its `UNIFORM_BLOCK_BINDING`.

### `webgd:glDefaultFramebuffer(Dict)`

`{width, height, data}`:

- `width` and `height` are the drawing buffer size.
- `data` is an RGBA8 Uint8Array from `readPixels`, only when the context has `preserveDrawingBuffer`. Otherwise it is Null.

### `webgd:glState(Dict)`

| Keys | Content |
| --- | --- |
| Caps | Bool from `isEnabled` for BLEND, CULL_FACE, DEPTH_TEST, DITHER, POLYGON_OFFSET_FILL, SAMPLE_ALPHA_TO_COVERAGE, SAMPLE_COVERAGE, SCISSOR_TEST, STENCIL_TEST, RASTERIZER_DISCARD |
| State pnames | `getParameter` values for blend color/equations/funcs, color clear value and write mask, cull face mode, depth func/range/mask/clear value, front face, line width, polygon offset, sample coverage, scissor box, front and back stencil func/ref/masks/ops, stencil clear value, viewport |
| Bindings | ACTIVE_TEXTURE, CURRENT_PROGRAM, ARRAY_BUFFER_BINDING, VERTEX_ARRAY_BINDING, draw/read framebuffer, renderbuffer, copy read/write, pixel pack/unpack, uniform buffer and transform feedback buffer bindings |
| Pixel store and hints | all `PACK_*`/`UNPACK_*` parameters including the `*_WEBGL` ones, GENERATE_MIPMAP_HINT, FRAGMENT_SHADER_DERIVATIVE_HINT |
| `units` | Array, one Dict per texture unit: `{"2d", "cube", "3d", "array", "sampler"}` |
| `ubo` | Array, one Dict per indexed uniform buffer binding: `{buffer, start, size}` |
| `currentAttribs` | Array of `CURRENT_VERTEX_ATTRIB` values (typed arrays) |

### `webgd:gpuBuffer(Typed | Null data, String | Null note)`

`this` is the `GPUBuffer`. `data` is a Uint8Array of the buffer's full size, read with
`copyBufferToBuffer` of the 4-byte-aligned prefix. Any trailing bytes are zero.

| Note | Meaning |
| --- | --- |
| `mapped` | Buffer was mapped at frame start |
| `mapRead` | `MAP_READ` buffer, cannot be a copy source |
| `noCopySrc` | No `COPY_SRC` usage |
| `copyFailed: …` | Validation error from the copy |
| `readbackFailed: …` | `mapAsync` failed. Data is Null. |

### `webgd:gpuTexture(Array | Null subresources, String | Null note)`

`this` is the `GPUTexture`. Each subresource, per mip and array layer (per depth slice for
`3d`), is a Dict:

| Key | Value |
| --- | --- |
| `mip`, `layer` | Number |
| `width`, `height` | Mip size rounded up to whole blocks |
| `depthOrLayers` | Always 1 |
| `bytesPerRow` | Tight row size in bytes (rows are de-padded from 256-byte alignment) |
| `rowsPerImage` | Block rows |
| `data` | Uint8Array, `bytesPerRow × rowsPerImage` |

Notes: `multisampled`, `depthStencil`, `noCopySrc`, `unknownFormat` (no entry in the hook's
bytes-per-block table), `copyFailed: …`, `readbackFailed: …`.

### `webgd:canvasSize(Number width, Number height)`

`this` is the canvas. Replay uses it as the initial OffscreenCanvas size. Without it the size is
300×150.

## Worked example: `tests/fixtures/sample.wire`

`xxd tests/fixtures/sample.wire` (308 bytes):

```text
00000000: 5747 4457 0100 0000 0f00 0000 6874 7470  WGDW........http
00000010: 3a2f 2f66 6978 7475 7265 2f01 0000 0000  ://fixture/.....
00000020: 1600 0000 5765 6247 4c32 5265 6e64 6572  ....WebGL2Render
00000030: 696e 6743 6f6e 7465 7874 0201 0000 0000  ingContext......
00000040: 0000 0000 0000 0001 0100 0000 0b00 0000  ................
00000050: 5765 6247 4c42 7566 6665 7202 0200 0000  WebGLBuffer.....
00000060: 0100 0000 0100 0000 0102 0000 000c 0000  ................
00000070: 0063 7265 6174 6542 7566 6665 7204 0001  .createBuffer...
00000080: 0000 0002 0000 0000 0000 0006 0200 0000  ................
00000090: 0103 0000 000a 0000 0062 7566 6665 7244  .........bufferD
000000a0: 6174 6103 0000 0000 0c00 0000 0000 803f  ata............?
000000b0: 0000 0040 0000 4040 0402 0100 0000 0300  ...@..@@........
000000c0: 0000 0300 0000 0400 0000 0040 12e1 4007  ...........@..@.
000000d0: 0800 0000 0004 0000 0000 801c e140 0001  .............@..
000000e0: 0400 0000 1000 0000 7765 6267 643a 7075  ........webgd:pu
000000f0: 7368 4d61 726b 6572 0105 0000 0004 0000  shMarker........
00000100: 0070 6173 7301 0600 0000 0100 0000 6104  .pass.........a.
00000110: 0201 0000 0004 0000 0002 0000 0005 0500  ................
00000120: 0000 0901 0000 0006 0000 0008 0200 0000  ................
00000130: 0301 00ff                                ....
```

Decoded (offsets in hex):

| Offset | Bytes | Meaning |
| --- | --- | --- |
| `00` | `57 47 44 57` | magic `WGDW` |
| `04` | `01 00 00 00` | version 1 |
| `08` | `0f 00 00 00` + 15 bytes | url `http://fixture/` |
| `1b` | `01` `00000000` `16000000` + 22 bytes | Str 0 = `WebGL2RenderingContext` |
| `3a` | `02` `01000000` `00000000` `00000000` | Object 1, class Str 0, no parent |
| `47` | `01` `01000000` `0b000000` + 11 bytes | Str 1 = `WebGLBuffer` |
| `5b` | `02` `02000000` `01000000` `01000000` | Object 2, class Str 1, parent object 1 |
| `68` | `01` `02000000` `0c000000` + 12 bytes | Str 2 = `createBuffer` |
| `7d` | `04` `00` | Call, section 0 (setup) |
| `7f` | `01000000` | this = object 1 |
| `83` | `02000000` | method = Str 2 `createBuffer` |
| `87` | `00000000` | argc 0 |
| `8b` | `06` `02000000` | result ObjRef 2 |
| `90` | `01` `03000000` `0a000000` + 10 bytes | Str 3 = `bufferData` |
| `a3` | `03` `00000000` `0c000000` | Blob 0, 12 bytes |
| `ac` | `0000803f 00000040 00004040` | blob data: f32 1.0, 2.0, 3.0 |
| `b8` | `04` `02` | Call, section 2 (frame) |
| `ba` | `01000000` `03000000` `03000000` | this = 1, method = Str 3 `bufferData`, argc 3 |
| `c6` | `04` `00000000 4012e140` | Number 34962 (`ARRAY_BUFFER`) |
| `cf` | `07` `08` `00000000` | Typed, kind 8 Float32Array, blob 0 |
| `d5` | `04` `00000000 801ce140` | Number 35044 (`STATIC_DRAW`) |
| `de` | `00` | result Undefined |
| `df` | `01` `04000000` `10000000` + 16 bytes | Str 4 = `webgd:pushMarker` |
| `f8` | `01` `05000000` `04000000` + 4 bytes | Str 5 = `pass` |
| `105` | `01` `06000000` `01000000` `61` | Str 6 = `a` |
| `10f` | `04` `02` | Call, section 2 (frame) |
| `111` | `01000000` `04000000` `02000000` | this = 1, method = Str 4 `webgd:pushMarker`, argc 2 |
| `11d` | `05` `05000000` | String Str 5 `pass` |
| `122` | `09` `01000000` | Dict, 1 entry |
| `127` | `06000000` | key Str 6 `a` |
| `12b` | `08` `02000000` `03` `01` | Array of 2: True, Null |
| `132` | `00` | result Undefined |
| `133` | `ff` | End |

This example shows the writer's ordering rules:

- Both Object records come before any Call, because `encode()` emits the whole object table first. Object 1's class string is therefore Str 0.
- `createBuffer` is interned only when its Call is written.
- Blob 0 comes right before the `bufferData` call that references it.
- Strings used only inside the marker call (`pass` and `a`) are emitted just before that call.

`webgd_core::display::call_label` renders the two frame calls as
`WebGL2RenderingContext#1.bufferData(34962, Float32Array[3], 35044)` and
`WebGL2RenderingContext#1.webgd:pushMarker("pass", {…})`. The fixture test in
`crates/webgd-core/tests/wire.rs` asserts these labels.
