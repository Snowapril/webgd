// webgd capture hook. Runs in the page's MAIN world at document_start.
// Must stay byte-compatible with crates/webgd-core/src/wire.rs.
(function () {
  'use strict';

  // ---------------------------------------------------------------------------
  // Wire encoding
  //
  // Value objects (produced by conv()):
  //   undefined | null | boolean | number | string        primitives
  //   {t:'obj', id}                                        ObjRef
  //   {t:'typed', kind, bytes: Uint8Array}                 Typed copy (kind = TYPED_KINDS name)
  //   {t:'array', items: [Value]}
  //   {t:'dict', entries: [[key, Value]]}
  //   {t:'image', w, h, bytes: Uint8Array}                 RGBA8 non-premultiplied
  //   {t:'pending', token}
  //   {t:'opaque', cls}
  // Records: {thisId, method, args: [Value], result: Value}
  // ---------------------------------------------------------------------------

  const TYPED_KINDS = {
    ArrayBuffer: 0, Int8Array: 1, Uint8Array: 2, Uint8ClampedArray: 3, Int16Array: 4,
    Uint16Array: 5, Int32Array: 6, Uint32Array: 7, Float32Array: 8, Float64Array: 9,
    DataView: 10, BigInt64Array: 11, BigUint64Array: 12, Float16Array: 13,
  };
  const SECTION = { setup: 0, snapshot: 1, frame: 2 };
  const textEncoder = new TextEncoder();

  class WireWriter {
    // objects: array of {id, cls, parent}; entries are emitted lazily on first reference.
    constructor(url, objects) {
      this.buf = new Uint8Array(1 << 16);
      this.len = 0;
      this.view = new DataView(this.buf.buffer);
      this.strIds = new Map();
      this.objDefs = new Map();
      for (const o of objects || []) this.objDefs.set(o.id, o);
      this.objEmitted = new Set();
      this.nextBlob = 0;
      this.bytes([0x57, 0x47, 0x44, 0x57]); // WGDW
      this.u32(1);
      this.string(String(url));
    }
    reserve(n) {
      if (this.len + n <= this.buf.length) return;
      let cap = this.buf.length * 2;
      while (cap < this.len + n) cap *= 2;
      const nb = new Uint8Array(cap);
      nb.set(this.buf.subarray(0, this.len));
      this.buf = nb;
      this.view = new DataView(nb.buffer);
    }
    u8(v) { this.reserve(1); this.buf[this.len++] = v; }
    u32(v) { this.reserve(4); this.view.setUint32(this.len, v >>> 0, true); this.len += 4; }
    f64(v) { this.reserve(8); this.view.setFloat64(this.len, v, true); this.len += 8; }
    bytes(b) { this.reserve(b.length); this.buf.set(b, this.len); this.len += b.length; }
    string(s) { const b = textEncoder.encode(s); this.u32(b.length); this.bytes(b); }

    // Interns `s`, emitting a Str record on first use. Returns its id.
    str(s) {
      s = String(s);
      let id = this.strIds.get(s);
      if (id === undefined) {
        id = this.strIds.size;
        this.strIds.set(s, id);
        this.u8(0x01); this.u32(id); this.string(s);
      }
      return id;
    }
    // Emits the Object record for `id` (and its parent chain) if not yet written.
    obj(id) {
      if (!id || this.objEmitted.has(id)) return id || 0;
      const def = this.objDefs.get(id);
      if (!def) throw new Error('webgd: unknown object id ' + id);
      this.objEmitted.add(id);
      const parent = def.parent ? this.obj(def.parent) : 0;
      const cls = this.str(def.cls);
      this.u8(0x02); this.u32(id); this.u32(cls); this.u32(parent);
      return id;
    }
    blob(bytes) {
      const id = this.nextBlob++;
      this.u8(0x03); this.u32(id); this.u32(bytes.length); this.bytes(bytes);
      return id;
    }
    // Emits every Str/Object/Blob a value needs before the call record.
    prepare(v) {
      if (v === null || typeof v !== 'object') {
        if (typeof v === 'string') this.str(v);
        return;
      }
      switch (v.t) {
        case 'obj': this.obj(v.id); break;
        case 'typed': case 'image': v._blob = this.blob(v.bytes); break;
        case 'array': for (const x of v.items) this.prepare(x); break;
        case 'dict': for (const [k, x] of v.entries) { this.str(k); this.prepare(x); } break;
        case 'opaque': this.str(v.cls); break;
        case 'pending': break;
        default: throw new Error('webgd: bad value tag ' + v.t);
      }
    }
    value(v) {
      if (v === undefined) return this.u8(0);
      if (v === null) return this.u8(1);
      switch (typeof v) {
        case 'boolean': return this.u8(v ? 3 : 2);
        case 'number': this.u8(4); return this.f64(v);
        case 'bigint': this.u8(4); return this.f64(Number(v));
        case 'string': this.u8(5); return this.u32(this.str(v));
      }
      switch (v.t) {
        case 'obj': this.u8(6); return this.u32(v.id);
        case 'typed': {
          const kind = TYPED_KINDS[v.kind];
          if (kind === undefined) throw new Error('webgd: bad typed kind ' + v.kind);
          this.u8(7); this.u8(kind); return this.u32(v._blob);
        }
        case 'array': this.u8(8); this.u32(v.items.length); for (const x of v.items) this.value(x); return;
        case 'dict':
          this.u8(9); this.u32(v.entries.length);
          for (const [k, x] of v.entries) { this.u32(this.str(k)); this.value(x); }
          return;
        case 'image': this.u8(10); this.u32(v.w); this.u32(v.h); return this.u32(v._blob);
        case 'pending': this.u8(11); return this.u32(v.token);
        case 'opaque': this.u8(12); return this.u32(this.str(v.cls));
      }
      throw new Error('webgd: bad value ' + v);
    }
    call(section, rec) {
      this.obj(rec.thisId);
      const m = this.str(rec.method);
      for (const a of rec.args) this.prepare(a);
      this.prepare(rec.result);
      this.u8(0x04); this.u8(section); this.u32(rec.thisId || 0); this.u32(m);
      this.u32(rec.args.length);
      for (const a of rec.args) this.value(a);
      this.value(rec.result);
    }
    // sections: {setup: [rec], snapshot: [rec], frame: [rec]}. Returns the finished bytes.
    encode(sections) {
      for (const o of this.objDefs.keys()) this.obj(o);
      for (const name of ['setup', 'snapshot', 'frame']) {
        for (const rec of sections[name] || []) this.call(SECTION[name], rec);
      }
      this.u8(0xFF);
      return this.buf.slice(0, this.len);
    }
  }

  // ---------------------------------------------------------------------------
  // Browser hook (only when loaded as a content script)
  // ---------------------------------------------------------------------------
  function install() {
    const IDLE = 0, ARMED = 1, CAPTURING = 2, FINALIZING = 3;
    const GL2 = 'WebGL2RenderingContext';
    const PRUNE_EVERY = 5000;

    let mode = IDLE;
    const ids = new WeakMap();
    let nextId = 1;
    const objectTable = [];
    const ORIG = Object.create(null);
    let setupLog = [];
    let lastPruneLen = 0;
    let setupCopy = [];
    let snapshotRecs = [];
    let snapshotPromises = [];
    let frameRecs = [];
    let captureTs = -1;
    let armTimer = 0;
    let nextToken = 1;
    let lastGL = null;
    let warned = false;

    const deleted = new WeakSet(); // WebGL delete*
    const destroyed = new WeakSet(); // GPU destroy() + expired canvas textures
    const viewOf = new WeakMap(); // GPUTextureView -> GPUTexture
    const canvasTex = new WeakMap(); // GPUCanvasContext -> latest getCurrentTexture()
    const texInfo = new WeakMap();
    const bufInfo = new WeakMap();
    const devOf = new WeakMap();
    const glContexts = [];
    const canvases = [];
    const queueLogged = new WeakSet();
    const writableMapped = new WeakSet();
    const mappedRanges = new WeakMap();
    const pendingErrors = new WeakMap();
    const wrappedExtProtos = new WeakSet();
    const status = { gl2: 0, webgl1: 0, gpuDevices: 0 };
    const post = window.postMessage.bind(window);
    const origSetTimeout = window.setTimeout.bind(window);
    const origClearTimeout = window.clearTimeout.bind(window);

    const weak = (o) => new WeakRef(o);
    const isObj = (v) => v !== null && (typeof v === 'object' || typeof v === 'function');
    const obj = (id) => ({ t: 'obj', id });
    const dict = (o) => ({ t: 'dict', entries: Object.keys(o).map((k) => [k, o[k]]) });
    const arr = (items) => ({ t: 'array', items });

    function internalError(e) {
      if (!warned) { warned = true; console.warn('webgd: internal hook error', e); }
    }
    function clsName(o) {
      try { const c = o.constructor; return (c && c.name) || 'Object'; } catch (_) { return 'Object'; }
    }
    function idOf(o, parent) {
      let id = ids.get(o);
      if (id === undefined) {
        id = nextId++;
        ids.set(o, id);
        objectTable.push({ id, cls: clsName(o), parent: parent || 0 });
      }
      return id;
    }

    // --- value conversion ----------------------------------------------------
    const IMAGE_CLASSES = ['ImageData', 'HTMLImageElement', 'HTMLCanvasElement', 'OffscreenCanvas',
      'ImageBitmap', 'HTMLVideoElement', 'VideoFrame'].map((n) => window[n]).filter((c) => typeof c === 'function');
    const isImage = (v) => IMAGE_CLASSES.some((C) => v instanceof C);
    const isBufferLike = (v) => v instanceof ArrayBuffer ||
      (typeof SharedArrayBuffer === 'function' && v instanceof SharedArrayBuffer);

    function typedCopy(v) {
      try {
        if (isBufferLike(v)) return { t: 'typed', kind: 'ArrayBuffer', bytes: new Uint8Array(v).slice() };
        let kind = 'Uint8Array';
        for (let p = Object.getPrototypeOf(v); p; p = Object.getPrototypeOf(p)) {
          const n = p.constructor && p.constructor.name;
          if (n in TYPED_KINDS) { kind = n; break; }
        }
        return { t: 'typed', kind, bytes: new Uint8Array(v.buffer, v.byteOffset, v.byteLength).slice() };
      } catch (_) {
        return { t: 'opaque', cls: clsName(v) }; // detached
      }
    }

    function sourceSize(v) {
      if (window.HTMLImageElement && v instanceof HTMLImageElement) return [v.naturalWidth, v.naturalHeight];
      if (window.HTMLVideoElement && v instanceof HTMLVideoElement) return [v.videoWidth, v.videoHeight];
      if (window.VideoFrame && v instanceof VideoFrame) return [v.displayWidth, v.displayHeight];
      return [v.width >>> 0, v.height >>> 0];
    }

    function imageOf(v) {
      try {
        if (v instanceof ImageData) {
          const d = v.data;
          return { t: 'image', w: v.width, h: v.height, bytes: new Uint8Array(d.buffer, d.byteOffset, d.byteLength).slice() };
        }
        const [w, h] = sourceSize(v);
        if (!w || !h) return { t: 'image', w: 0, h: 0, bytes: new Uint8Array(0) };
        const c = new OffscreenCanvas(w, h);
        const ctx = ORIG['OffscreenCanvas.getContext'].call(c, '2d', { willReadFrequently: true });
        ctx.drawImage(v, 0, 0, w, h);
        const d = ctx.getImageData(0, 0, w, h).data;
        return { t: 'image', w, h, bytes: new Uint8Array(d.buffer, d.byteOffset, d.byteLength) };
      } catch (_) {
        return { t: 'opaque', cls: clsName(v) };
      }
    }

    function conv(v, depth) {
      switch (typeof v) {
        case 'undefined': case 'boolean': case 'number': case 'string': return v;
        case 'bigint': return Number(v);
        case 'symbol': return { t: 'opaque', cls: 'Symbol' };
        case 'function': return { t: 'opaque', cls: 'Function' };
      }
      if (v === null) return null;
      depth = depth || 0;
      if (ArrayBuffer.isView(v) || isBufferLike(v)) return typedCopy(v);
      if (isImage(v)) return imageOf(v);
      const id = ids.get(v);
      if (id !== undefined) return obj(id);
      if (depth > 32) return { t: 'opaque', cls: clsName(v) };
      if (Array.isArray(v)) return arr(v.map((x) => conv(x, depth + 1)));
      const proto = Object.getPrototypeOf(v);
      if (proto === Object.prototype || proto === null) {
        return { t: 'dict', entries: Object.keys(v).map((k) => [k, conv(v[k], depth + 1)]) };
      }
      const cls = clsName(v);
      if (cls === 'GPUSupportedLimits') {
        const entries = [];
        for (const k in v) if (typeof v[k] === 'number') entries.push([k, v[k]]);
        return { t: 'dict', entries };
      }
      if (cls === 'GPUSupportedFeatures' || v instanceof Set) return arr([...v].map((x) => conv(x, depth + 1)));
      if (/^(GPU|WebGL)/.test(cls)) return obj(idOf(v, 0));
      return { t: 'opaque', cls };
    }

    function convResult(res, thisId) {
      if (!isObj(res) || typeof res === 'function') return conv(res);
      if (ArrayBuffer.isView(res) || isBufferLike(res) || Array.isArray(res)) return conv(res);
      const proto = Object.getPrototypeOf(res);
      if (proto === Object.prototype || proto === null) return conv(res);
      return obj(idOf(res, thisId));
    }

    function visitIds(v, fn) {
      if (!isObj(v)) return;
      switch (v.t) {
        case 'obj': fn(v.id); break;
        case 'array': for (const x of v.items) visitIds(x, fn); break;
        case 'dict': for (const [, x] of v.entries) visitIds(x, fn); break;
      }
    }

    // --- setup log -------------------------------------------------------------
    function logEntry(rec, ref, subj) {
      rec.ref = ref;
      rec.subj = subj || 0;
      rec.inSetup = true;
      setupLog.push(rec);
      if (setupLog.length - lastPruneLen >= PRUNE_EVERY) prune();
    }

    function logSetup(rec, self, args, res) {
      if (/^delete/.test(rec.method) && !isObj(args[0])) return;
      if (res instanceof Promise) return logEntry(rec, null, 0); // root until resolved
      if (isObj(res)) return logEntry(rec, weak(res), 0);
      const target = self instanceof WebGL2RenderingContext ? args[0] : self;
      if (isObj(target)) return logEntry(rec, weak(target), ids.get(target) || 0);
      logEntry(rec, null, 0);
    }

    function createdId(e) {
      if (e.result && e.result.t === 'obj') return e.result.id;
      if (e.method === 'webgd:resolved' && e.args[1] && e.args[1].t === 'obj') return e.args[1].id;
      return 0;
    }

    // Mark-sweep over the setup log (plan Step 2 item 6).
    function prune() {
      const creators = new Map(), issuers = new Map(), bySubj = new Map();
      for (const e of setupLog) {
        const c = createdId(e);
        if (c && !creators.has(c)) creators.set(c, e);
        if (e.result && e.result.t === 'pending') issuers.set(e.result.token, e);
        if (e.subj) {
          let l = bySubj.get(e.subj);
          if (!l) bySubj.set(e.subj, (l = []));
          l.push(e);
        }
      }
      const keep = new Set(), work = [];
      const add = (e) => { if (e && !keep.has(e)) { keep.add(e); work.push(e); } };
      // Roots: entries without a ref, or whose object is alive and not GPU-destroyed/expired
      // (a texture view counts as dead once its texture is). Deleted-but-alive WebGL objects stay
      // roots: GL keeps them alive while attached to containers.
      for (const e of setupLog) {
        if (!e.ref) { add(e); continue; }
        const o = e.ref.deref();
        if (o !== undefined && !destroyed.has(o) && !destroyed.has(viewOf.get(o))) add(e);
      }
      const need = (id) => add(creators.get(id));
      while (work.length) {
        const e = work.pop();
        need(e.thisId);
        for (const a of e.args) visitIds(a, need);
        if (e.method === 'webgd:resolved') add(issuers.get(e.args[0]));
        const c = createdId(e);
        if (c) for (const s of bySubj.get(c) || []) add(s);
      }
      setupLog = setupLog.filter((e) => keep.has(e));
      lastPruneLen = setupLog.length;
    }

    function removeUploads(texId, key) {
      setupLog = setupLog.filter((e) => !(e.upTex === texId && (key === null || e.upKey === key)));
      lastPruneLen = Math.min(lastPruneLen, setupLog.length);
    }

    // --- generic recording -------------------------------------------------------
    function trackPromise(p, token, rec, s) {
      p.then((v) => {
        try {
          const r = { thisId: rec.thisId, method: 'webgd:resolved', args: [token, convResult(v, rec.thisId)], result: undefined };
          if (mode === CAPTURING) frameRecs.push(r);
          if (rec.inSetup) {
            const ref = isObj(v) ? weak(v) : null;
            if (ref) rec.ref = ref;
            logEntry(r, ref, 0);
          }
          if (s.onResolve) s.onResolve(v);
        } catch (e) { internalError(e); }
      }, () => {});
    }

    function resultOf(rec, res, s) {
      if (s.opaqueResult) return { t: 'opaque', cls: clsName(res) };
      if (res instanceof Promise) {
        const token = nextToken++;
        trackPromise(res, token, rec, s);
        return { t: 'pending', token };
      }
      return convResult(res, rec.thisId);
    }

    function invoke(self, name, orig, argl, s) {
      const capturing = mode === CAPTURING;
      if (s.custom) return s.custom(self, argl, orig, capturing);
      if (!capturing && !s.tracked) {
        const res = orig.apply(self, s.patchArgs ? s.patchArgs(Array.prototype.slice.call(argl)) : argl);
        if (s.after) { try { s.after(self, argl, res, false); } catch (e) { internalError(e); } }
        return res;
      }
      const args = Array.prototype.slice.call(argl);
      let rec = null;
      try {
        if (s.before) s.before(self, args, capturing);
        rec = { thisId: idOf(self, 0), method: name, args: args.map((a) => conv(a)), result: undefined };
      } catch (e) { internalError(e); }
      const res = orig.apply(self, s.patchArgs ? s.patchArgs(args) : args);
      try {
        if (rec) {
          rec.result = resultOf(rec, res, s);
          if (capturing) frameRecs.push(rec);
          if (s.tracked && (!s.logIf || s.logIf(self, args, res))) logSetup(rec, self, args, res);
        }
        if (s.after) s.after(self, args, res, capturing);
      } catch (e) { internalError(e); }
      return res;
    }

    function wrapProto(proto, cls, specFor) {
      if (!proto) return;
      const isGL = cls === GL2;
      for (const name of Object.getOwnPropertyNames(proto)) {
        if (name === 'constructor') continue;
        const d = Object.getOwnPropertyDescriptor(proto, name);
        if (!d || typeof d.value !== 'function') continue;
        const orig = d.value;
        ORIG[cls + '.' + name] = orig;
        const s = specFor(name);
        const always = !!(s.tracked || s.always || s.custom);
        const w = isGL
          ? function () {
            lastGL = this;
            if (mode !== CAPTURING && !always) return orig.apply(this, arguments);
            return invoke(this, name, orig, arguments, s);
          }
          : function () {
            if (mode !== CAPTURING && !always) return orig.apply(this, arguments);
            return invoke(this, name, orig, arguments, s);
          };
        Object.defineProperty(proto, name, { value: w, writable: d.writable, enumerable: d.enumerable, configurable: d.configurable });
      }
    }

    function wrapSetter(proto, prop, onSet) {
      const d = proto && Object.getOwnPropertyDescriptor(proto, prop);
      if (!d || !d.set || !d.configurable) return;
      const set = d.set;
      Object.defineProperty(proto, prop, {
        get: d.get, enumerable: d.enumerable, configurable: true,
        set: function (v) {
          set.call(this, v);
          try { onSet(this, v); } catch (e) { internalError(e); }
        },
      });
    }

    function postStatus() {
      post({ __webgd: 'to-bridge', type: 'status', gl2: status.gl2, webgl1: status.webgl1, gpuDevices: status.gpuDevices }, '*');
      try { overlayRender(); } catch (e) { internalError(e); }
    }
    function sendError(message) {
      post({ __webgd: 'to-bridge', type: 'captureError', message: String(message) }, '*');
    }

    // --- WebGL constants & format tables --------------------------------------------
    const TEXTURE_2D = 0x0DE1, TEXTURE_CUBE_MAP = 0x8513, CUBE_PX = 0x8515, TEXTURE_3D = 0x806F, TEXTURE_2D_ARRAY = 0x8C1A;
    const BINDING_FOR = { [TEXTURE_2D]: 0x8069, [TEXTURE_CUBE_MAP]: 0x8514, [TEXTURE_3D]: 0x806A, [TEXTURE_2D_ARRAY]: 0x8C1D };
    const READ_FB = 0x8CA8, DRAW_FB = 0x8CA9, COLOR0 = 0x8CE0, RENDERBUFFER = 0x8D41;
    const SHADOW = new Set([0x1909, 0x1906, 0x190A, 0x8C3D, 0x8F94, 0x8F95, 0x8F96, 0x8F97, 0x881B, 0x8815, 0x8C41,
      0x8D8F, 0x8D7D, 0x8D89, 0x8D77, 0x8D83, 0x8D71]);
    const DEPTH_STENCIL = new Set([0x1902, 0x84F9, 0x81A5, 0x81A6, 0x8CAC, 0x88F0, 0x8CAD, 0x8D48]);
    const FLOAT_FORMATS = new Set([0x822D, 0x822F, 0x881A, 0x822E, 0x8230, 0x8814, 0x8C3A, 0x881B, 0x8815, 0x8C3D]);
    const INT_FORMATS = new Set([0x8231, 0x8233, 0x8235, 0x8237, 0x8239, 0x823B, 0x8D8E, 0x8D88, 0x8D82, 0x8D8F, 0x8D89, 0x8D83]);
    const UINT_FORMATS = new Set([0x8232, 0x8234, 0x8236, 0x8238, 0x823A, 0x823C, 0x8D7C, 0x8D76, 0x8D70, 0x906F, 0x8D7D, 0x8D77, 0x8D71]);
    function formatClass(f) {
      if (FLOAT_FORMATS.has(f)) return 'float';
      if (INT_FORMATS.has(f)) return 'int';
      if (UINT_FORMATS.has(f)) return 'uint';
      if (f === 0x8C43 || f === 0x8C41) return 'srgb';
      return 'unorm';
    }
    const READ_AS = {
      unorm: [0x1908, 0x1401, Uint8Array], srgb: [0x1908, 0x1401, Uint8Array], float: [0x1908, 0x1406, Float32Array],
      int: [0x8D99, 0x1404, Int32Array], uint: [0x8D99, 0x1405, Uint32Array],
    };
    const UNPACK_PNAMES = [0x0CF5, 0x0CF2, 0x0CF3, 0x0CF4, 0x806E, 0x806D, 0x9240, 0x9241, 0x9243];
    const CAPS = [0x0BE2, 0x0B44, 0x0B71, 0x0BD0, 0x8037, 0x809E, 0x80A0, 0x0C11, 0x0B90, 0x8C89];
    const STATE_PNAMES = [
      // blend
      0x8005, 0x8009, 0x883D, 0x80C9, 0x80C8, 0x80CB, 0x80CA,
      // color, cull, depth, front face, line width, polygon offset, sample coverage, scissor
      0x0C22, 0x0C23, 0x0B45, 0x0B74, 0x0B70, 0x0B72, 0x0B73, 0x0B46, 0x0B21, 0x8038, 0x2A00, 0x80AA, 0x80AB, 0x0C10,
      // stencil front/back + clear
      0x0B92, 0x0B97, 0x0B93, 0x0B94, 0x0B95, 0x0B96, 0x0B98, 0x0B91,
      0x8800, 0x8CA3, 0x8CA4, 0x8801, 0x8802, 0x8803, 0x8CA5,
      // viewport
      0x0BA2,
      // bindings
      0x84E0, 0x8B8D, 0x8894, 0x85B5, 0x8CA6, 0x8CAA, 0x8CA7, 0x8F36, 0x8F37, 0x88ED, 0x88EF, 0x8A28, 0x8C8F, 0x8E25,
      // pixel store
      0x0D05, 0x0D02, 0x0D03, 0x0D04, 0x0CF5, 0x0CF2, 0x0CF3, 0x0CF4, 0x806E, 0x806D, 0x9240, 0x9241, 0x9243,
      // hints
      0x8192, 0x8B8B,
    ];
    const TEX_PARAMS = [0x813C, 0x813D, 0x2800, 0x2801, 0x2802, 0x2803, 0x8072, 0x884C, 0x884D, 0x813A, 0x813B];
    const SAMPLER_PARAMS = [0x2801, 0x2800, 0x2802, 0x2803, 0x8072, 0x813A, 0x813B, 0x884C, 0x884D];

    function newTexInfo(target) {
      return { target, internalformat: null, format: null, type: null, immutable: false, levels: new Map(),
        uncapturable: false, shadow: false, hasUploads: false };
    }
    function setLevel(info, level, face, w, h, d) {
      info.levels.set(level + '/' + face, { level, face, w: w >>> 0, h: h >>> 0, d: d >>> 0 });
    }

    const DEFINE = new Set(['texImage2D', 'texImage3D', 'texStorage2D', 'texStorage3D', 'copyTexImage2D',
      'compressedTexImage2D', 'compressedTexImage3D']);
    const UPLOADS = new Set([...DEFINE, 'texSubImage2D', 'texSubImage3D', 'compressedTexSubImage2D',
      'compressedTexSubImage3D', 'copyTexSubImage2D', 'copyTexSubImage3D']);

    function defineLevels(info, name, args, face) {
      info.internalformat = args[2];
      switch (name) {
        case 'texImage2D':
          if (args.length === 6) {
            info.format = args[3]; info.type = args[4];
            const [w, h] = sourceSize(args[5]);
            setLevel(info, args[1], face, w, h, 1);
          } else {
            info.format = args[6]; info.type = args[7];
            setLevel(info, args[1], face, args[3], args[4], 1);
          }
          break;
        case 'texImage3D':
          info.format = args[7]; info.type = args[8];
          setLevel(info, args[1], 0, args[3], args[4], args[5]);
          break;
        case 'copyTexImage2D':
          info.format = null; info.type = null;
          setLevel(info, args[1], face, args[5], args[6], 1);
          break;
        case 'compressedTexImage2D':
          info.format = null; info.type = null;
          setLevel(info, args[1], face, args[3], args[4], 1);
          break;
        case 'compressedTexImage3D':
          info.format = null; info.type = null;
          setLevel(info, args[1], 0, args[3], args[4], args[5]);
          break;
        case 'texStorage2D': case 'texStorage3D': {
          info.immutable = true; info.format = null; info.type = null;
          info.levels.clear();
          let w = args[3], h = args[4], d = name === 'texStorage3D' ? args[5] : 1;
          const faces = info.target === TEXTURE_CUBE_MAP ? 6 : 1;
          for (let l = 0; l < args[1]; l++) {
            for (let f = 0; f < faces; f++) setLevel(info, l, f, w, h, d);
            w = Math.max(1, w >> 1); h = Math.max(1, h >> 1);
            if (info.target === TEXTURE_3D) d = Math.max(1, d >> 1);
          }
          break;
        }
      }
    }

    // texImage*/texStorage*/texSubImage*/copyTex*/compressedTex*: dimension tracking + shadow uploads.
    function glUpload(name) {
      const isDefine = DEFINE.has(name);
      const isCopy = name.startsWith('copy');
      const isStorage = name.startsWith('texStorage');
      return function (self, argl, orig, capturing) {
        const args = Array.prototype.slice.call(argl);
        let info, tex, face = 0, bindTarget = args[0], logUpload = false, pre = null, thisId = 0, cargs = null;
        try {
          const isFace = args[0] >= CUBE_PX && args[0] <= CUBE_PX + 5;
          face = isFace ? args[0] - CUBE_PX : 0;
          bindTarget = isFace ? TEXTURE_CUBE_MAP : args[0];
          const bp = BINDING_FOR[bindTarget];
          tex = bp ? ORIG[GL2 + '.getParameter'].call(self, bp) : null;
          if (tex) {
            info = texInfo.get(tex);
            if (!info) texInfo.set(tex, (info = newTexInfo(bindTarget)));
            if (isDefine) info.shadow = SHADOW.has(args[2]) || name.startsWith('compressed');
            if (info.shadow) {
              if (isCopy || ORIG[GL2 + '.getParameter'].call(self, 0x88EF)) info.uncapturable = true;
              else logUpload = true;
            }
          }
          thisId = idOf(self, 0);
          if (logUpload) {
            const texId = idOf(tex, thisId);
            const ps = UNPACK_PNAMES.map((p) => [String(p), conv(ORIG[GL2 + '.getParameter'].call(self, p))]);
            pre = [
              { thisId, method: 'webgd:bindForUpload', args: [bindTarget, obj(texId)], result: undefined },
              { thisId, method: 'webgd:pixelStore', args: [{ t: 'dict', entries: ps }], result: undefined },
            ];
          }
          if (capturing || logUpload) cargs = args.map((a) => conv(a));
        } catch (e) { internalError(e); }
        const res = orig.apply(self, args);
        try {
          if (info && isDefine) {
            defineLevels(info, name, args, face);
            if (info.hasUploads) {
              removeUploads(ids.get(tex), isStorage ? null : args[1] + '/' + face);
              info.hasUploads = false;
            }
          }
          if (!cargs) return res;
          const rec = { thisId, method: name, args: cargs, result: conv(res) };
          if (capturing) frameRecs.push(rec);
          if (logUpload) {
            const texId = ids.get(tex);
            const key = isStorage ? '*' : args[1] + '/' + face;
            const ref = weak(tex);
            for (const e of [...pre, rec]) { e.upTex = texId; e.upKey = key; logEntry(e, ref, texId); }
            info.hasUploads = true;
          }
        } catch (e) { internalError(e); }
        return res;
      };
    }

    function wrapExtension(ext) {
      const proto = Object.getPrototypeOf(ext);
      if (!proto || proto === Object.prototype || wrappedExtProtos.has(proto)) return;
      wrappedExtProtos.add(proto);
      wrapProto(proto, clsName(ext), () => ({}));
    }

    const GL_TRACKED = new Set(['shaderSource', 'compileShader', 'attachShader', 'detachShader', 'bindAttribLocation',
      'transformFeedbackVaryings', 'linkProgram', 'getUniformLocation', 'getExtension']);

    function glSpec(name) {
      const s = {};
      if (/^(create|delete)/.test(name) || GL_TRACKED.has(name)) s.tracked = true;
      if (UPLOADS.has(name)) s.custom = glUpload(name);
      if (/^delete/.test(name)) {
        s.after = (self, args) => {
          const o = args[0];
          if (!isObj(o)) return;
          deleted.add(o);
          if (name === 'deleteTexture' && ids.has(o)) removeUploads(ids.get(o), null);
        };
      }
      switch (name) {
        case 'getExtension':
          s.after = (self, args, res) => { if (isObj(res)) { idOf(res, idOf(self, 0)); wrapExtension(res); } };
          break;
        case 'bindBuffer':
          s.always = true;
          s.after = (self, args) => {
            const b = args[1];
            if (isObj(b) && !bufInfo.has(b)) bufInfo.set(b, { isIndex: args[0] === 0x8893 });
          };
          break;
        case 'bindTexture':
          s.always = true;
          s.after = (self, args) => {
            const t = args[1];
            if (isObj(t) && !texInfo.has(t)) texInfo.set(t, newTexInfo(args[0]));
          };
          break;
        case 'getError':
          s.custom = (self, argl, orig, capturing) => {
            const pend = pendingErrors.get(self);
            const res = pend && pend.length ? pend.shift() : orig.call(self);
            if (capturing) frameRecs.push({ thisId: idOf(self, 0), method: 'getError', args: [], result: res });
            return res;
          };
          break;
      }
      return s;
    }

    // --- WebGPU specs ---------------------------------------------------------------------
    const BU = { MAP_READ: 1, MAP_WRITE: 2, COPY_SRC: 4, COPY_DST: 8 };
    const TU_COPY_SRC = 1;
    const TU_TRANSIENT = (window.GPUTextureUsage && window.GPUTextureUsage.TRANSIENT_ATTACHMENT) || 0;

    function patchFirst(fn) {
      return (args) => {
        const d = args[0];
        if (!d || typeof d !== 'object') return args;
        const usage = fn(d);
        if (usage === undefined) return args;
        const a = args.slice();
        a[0] = Object.assign({}, d, { usage });
        return a;
      };
    }

    function gpuSpec(cls, name) {
      const s = {};
      switch (cls) {
        case 'GPU':
          if (name === 'requestAdapter') s.tracked = true;
          break;
        case 'GPUAdapter':
          if (name === 'requestDevice') {
            s.tracked = true;
            s.onResolve = (dev) => { if (dev) { status.gpuDevices++; postStatus(); } };
          }
          break;
        case 'GPUDevice':
          if (/^create/.test(name)) s.tracked = true;
          if (name === 'createBuffer') {
            s.patchArgs = patchFirst((d) => ((d.usage >>> 0) & BU.MAP_READ ? undefined : (d.usage >>> 0) | BU.COPY_SRC));
            s.after = (self, args, res) => {
              if (!isObj(res)) return;
              devOf.set(res, self);
              if (args[0] && args[0].mappedAtCreation) writableMapped.add(res);
            };
          } else if (name === 'createTexture') {
            s.patchArgs = patchFirst((d) => {
              const u = d.usage >>> 0;
              return (d.sampleCount ?? 1) === 1 && !(u & TU_TRANSIENT) ? u | TU_COPY_SRC : undefined;
            });
            s.after = (self, args, res) => { if (isObj(res)) devOf.set(res, self); };
          }
          break;
        case 'GPUCanvasContext':
          if (name === 'configure' || name === 'unconfigure' || name === 'getCurrentTexture') s.tracked = true;
          if (name === 'configure') s.patchArgs = patchFirst((d) => ((d.usage ?? 0x10) >>> 0) | TU_COPY_SRC);
          if (name === 'getCurrentTexture') {
            // The previous canvas texture expired when it was presented.
            s.after = (self, args, res) => {
              const prev = canvasTex.get(self);
              if (prev && prev !== res) destroyed.add(prev);
              if (isObj(res)) canvasTex.set(self, res);
            };
          }
          break;
        case 'GPUTexture':
          if (name === 'createView') {
            s.tracked = true;
            s.after = (self, args, res) => { if (isObj(res)) viewOf.set(res, self); };
          }
          if (name === 'destroy') { s.tracked = true; s.after = (self) => destroyed.add(self); }
          break;
        case 'GPUCommandEncoder':
          // A finished encoder can no longer be used, so its setup entry stops being a prune root.
          if (name === 'finish') { s.always = true; s.after = (self) => destroyed.add(self); }
          break;
        case 'GPURenderBundleEncoder':
          // Every bundle-encoder call goes to the setup log (subject = the encoder). The finish()
          // entry is rooted by the resulting GPURenderBundle; marking it keeps the encoder's creation
          // entry and with it every entry about the encoder, even though the encoder itself is dead.
          s.tracked = true;
          if (name === 'finish') s.after = (self) => destroyed.add(self);
          break;
        case 'GPUQuerySet':
          if (name === 'destroy') { s.tracked = true; s.after = (self) => destroyed.add(self); }
          break;
        case 'GPURenderPipeline': case 'GPUComputePipeline':
          if (name === 'getBindGroupLayout') s.tracked = true;
          break;
        case 'GPUBuffer':
          switch (name) {
            case 'destroy':
              s.tracked = true;
              s.after = (self) => { destroyed.add(self); mappedRanges.delete(self); writableMapped.delete(self); };
              break;
            case 'mapAsync':
              s.always = true;
              s.after = (self, args) => { if ((args[0] >>> 0) & 2) writableMapped.add(self); };
              break;
            case 'getMappedRange':
              s.always = true;
              s.opaqueResult = true;
              s.after = (self, args, res) => {
                if (!writableMapped.has(self) || !isObj(res)) return;
                let l = mappedRanges.get(self);
                if (!l) mappedRanges.set(self, (l = []));
                l.push({ offset: Number(args[0] ?? 0), ab: res });
              };
              break;
            case 'unmap':
              s.tracked = true;
              s.before = (self, args, capturing) => {
                const l = mappedRanges.get(self);
                if (!capturing || !l) return;
                const thisId = idOf(self, 0);
                for (const r of l) {
                  frameRecs.push({ thisId, method: 'webgd:mappedWrite', args: [r.offset, conv(new Uint8Array(r.ab))], result: undefined });
                }
              };
              s.after = (self) => { mappedRanges.delete(self); writableMapped.delete(self); };
              break;
          }
          break;
      }
      return s;
    }

    // --- install wrappers ---------------------------------------------------------------
    if (window.WebGL2RenderingContext) wrapProto(WebGL2RenderingContext.prototype, GL2, glSpec);
    for (const cls of ['GPU', 'GPUAdapter', 'GPUDevice', 'GPUQueue', 'GPUBuffer', 'GPUTexture', 'GPUCommandEncoder',
      'GPURenderPassEncoder', 'GPUComputePassEncoder', 'GPURenderBundleEncoder', 'GPUCanvasContext',
      'GPUShaderModule', 'GPURenderPipeline', 'GPUComputePipeline', 'GPUQuerySet']) {
      if (typeof window[cls] === 'function') wrapProto(window[cls].prototype, cls, (name) => gpuSpec(cls, name));
    }

    // device.queue getter: logged once per device.
    if (window.GPUDevice) {
      const qd = Object.getOwnPropertyDescriptor(GPUDevice.prototype, 'queue');
      if (qd && qd.get) {
        const get = qd.get;
        ORIG['GPUDevice.get:queue'] = get;
        Object.defineProperty(GPUDevice.prototype, 'queue', {
          enumerable: qd.enumerable, configurable: true,
          get: function () {
            const q = get.call(this);
            try {
              if (q && !queueLogged.has(this)) {
                queueLogged.add(this);
                const thisId = idOf(this, 0);
                const rec = { thisId, method: 'get:queue', args: [], result: obj(idOf(q, thisId)) };
                if (mode === CAPTURING) frameRecs.push(rec);
                logEntry(rec, weak(q), 0);
              }
            } catch (e) { internalError(e); }
            return q;
          },
        });
      }
    }

    // label setters on every GPU* prototype that defines one.
    for (const k of Object.getOwnPropertyNames(window)) {
      if (!/^GPU/.test(k)) continue;
      const C = window[k];
      if (typeof C !== 'function' || !C.prototype) continue;
      wrapSetter(C.prototype, 'label', (self, v) => {
        const known = ids.has(self);
        if (!known && mode !== CAPTURING) return;
        const rec = { thisId: idOf(self, 0), method: 'set:label', args: [conv(v)], result: undefined };
        if (mode === CAPTURING) frameRecs.push(rec);
        if (known) logEntry(rec, weak(self), rec.thisId);
      });
    }

    // canvas width/height setters (only canvases with an id).
    for (const C of [window.HTMLCanvasElement, window.OffscreenCanvas]) {
      if (!C) continue;
      for (const prop of ['width', 'height']) {
        wrapSetter(C.prototype, prop, (self) => {
          const id = ids.get(self);
          if (id === undefined) return;
          const rec = { thisId: id, method: 'set:' + prop, args: [self[prop]], result: undefined };
          if (mode === CAPTURING) frameRecs.push(rec);
          logEntry(rec, weak(self), id);
        });
      }
    }

    // getContext on both canvas kinds.
    for (const [C, cls] of [[window.HTMLCanvasElement, 'HTMLCanvasElement'], [window.OffscreenCanvas, 'OffscreenCanvas']]) {
      if (!C) continue;
      const orig = C.prototype.getContext;
      ORIG[cls + '.getContext'] = orig;
      Object.defineProperty(C.prototype, 'getContext', {
        writable: true, enumerable: true, configurable: true,
        value: function getContext(type) {
          const ctx = orig.apply(this, arguments);
          try {
            const t = String(type);
            if (!ctx) return ctx;
            if (t === 'webgl' || t === 'experimental-webgl') {
              if (!ids.has(ctx)) { idOf(ctx, idOf(this, 0)); status.webgl1++; postStatus(); }
              return ctx;
            }
            if (t !== 'webgl2' && t !== 'webgpu') return ctx;
            const isNew = !ids.has(ctx);
            if (!isNew && mode !== CAPTURING) return ctx;
            const canvasId = idOf(this, 0);
            const rec = { thisId: canvasId, method: 'getContext', args: Array.prototype.map.call(arguments, (a) => conv(a)),
              result: obj(idOf(ctx, canvasId)) };
            if (mode === CAPTURING) frameRecs.push(rec);
            if (isNew) {
              logEntry(rec, weak(ctx), 0);
              canvases.push(weak(this));
              if (t === 'webgl2') { glContexts.push(weak(ctx)); status.gl2++; postStatus(); }
            }
          } catch (e) { internalError(e); }
          return ctx;
        },
      });
    }

    if (navigator.gpu) idOf(navigator.gpu, 0);

    const GL = Object.create(null);
    for (const k in ORIG) {
      if (k.startsWith(GL2 + '.')) GL[k.slice(GL2.length + 1)] = ORIG[k];
    }
    const gpu = (cls, m) => ORIG[cls + '.' + m];

    // --- WebGL2 snapshot (plan Step 3) ------------------------------------------------------
    function snapshotGL(gl) {
      if (GL.isContextLost.call(gl)) return;
      const ctxId = ids.get(gl);
      const P = (p) => GL.getParameter.call(gl, p);
      const push = (method, args) => snapshotRecs.push({ thisId: ctxId, method, args, result: undefined });
      const drain = () => { for (let i = 0; i < 64 && GL.getError.call(gl); i++); };

      const appErrors = [];
      for (let i = 0; i < 64; i++) { const e = GL.getError.call(gl); if (!e) break; appErrors.push(e); }

      const sv = {
        draw: P(0x8CA6), read: P(0x8CAA), vao: P(0x85B5), active: P(0x84E0), copyRead: P(0x8F36), pack: P(0x88ED),
        rb: P(0x8CA7), packAlign: P(0x0D05), packRow: P(0x0D02), packSkipRows: P(0x0D03), packSkipPix: P(0x0D04),
      };
      const maxUnits = P(0x8B4D);
      const scratchUnit = maxUnits - 1;
      const UNIT_BINDS = [[TEXTURE_2D, 0x8069], [TEXTURE_CUBE_MAP, 0x8514], [TEXTURE_3D, 0x806A], [TEXTURE_2D_ARRAY, 0x8C1D]];
      GL.activeTexture.call(gl, 0x84C0 + scratchUnit);
      const unitSave = UNIT_BINDS.map(([, b]) => P(b));
      GL.activeTexture.call(gl, sv.active);
      GL.getExtension.call(gl, 'EXT_color_buffer_float');

      // Live objects of this context, from the (just pruned) setup log.
      const live = Object.create(null);
      for (const e of setupLog) {
        if (e.thisId !== ctxId || !e.ref || !e.result || e.result.t !== 'obj' || !/^create/.test(e.method)) continue;
        const o = e.ref.deref();
        if (!o || deleted.has(o)) continue;
        (live[e.method] || (live[e.method] = [])).push(o);
      }
      const L = (m) => live[m] || [];

      // glState (read before anything is touched).
      const st = [];
      for (const c of CAPS) st.push([String(c), GL.isEnabled.call(gl, c)]);
      for (const p of STATE_PNAMES) st.push([String(p), conv(P(p))]);
      const units = [];
      for (let u = 0; u < maxUnits; u++) {
        GL.activeTexture.call(gl, 0x84C0 + u);
        units.push(dict({ '2d': conv(P(0x8069)), cube: conv(P(0x8514)), '3d': conv(P(0x806A)), array: conv(P(0x8C1D)), sampler: conv(P(0x8919)) }));
      }
      GL.activeTexture.call(gl, sv.active);
      st.push(['units', arr(units)]);
      const ubo = [];
      const maxUbo = P(0x8A2F);
      for (let i = 0; i < maxUbo; i++) {
        ubo.push(dict({
          buffer: conv(GL.getIndexedParameter.call(gl, 0x8A28, i)),
          start: Number(GL.getIndexedParameter.call(gl, 0x8A29, i)),
          size: Number(GL.getIndexedParameter.call(gl, 0x8A2A, i)),
        }));
      }
      st.push(['ubo', arr(ubo)]);
      const maxAttribs = P(0x8869);
      const cur = [];
      for (let i = 0; i < maxAttribs; i++) cur.push(conv(GL.getVertexAttrib.call(gl, i, 0x8626)));
      st.push(['currentAttribs', arr(cur)]);

      // Scratch objects (no ids, never recorded).
      const fbo = GL.createFramebuffer.call(gl);
      const svao = GL.createVertexArray.call(gl);
      GL.bindBuffer.call(gl, 0x88EB, null);
      GL.pixelStorei.call(gl, 0x0D05, 4);
      GL.pixelStorei.call(gl, 0x0D02, 0);
      GL.pixelStorei.call(gl, 0x0D03, 0);
      GL.pixelStorei.call(gl, 0x0D04, 0);

      function readAttached(w, h, cls) {
        if (GL.checkFramebufferStatus.call(gl, READ_FB) !== 0x8CD5) return null;
        const [fmt, type, Arr] = READ_AS[cls];
        const out = new Arr(w * h * 4);
        drain();
        GL.readPixels.call(gl, 0, 0, w, h, fmt, type, out);
        if (GL.getError.call(gl)) return null;
        return conv(out);
      }

      // Textures.
      GL.activeTexture.call(gl, 0x84C0 + scratchUnit);
      for (const tex of L('createTexture')) {
        const info = texInfo.get(tex);
        if (!info || !info.target) continue;
        const tgt = info.target;
        GL.bindTexture.call(gl, tgt, tex);
        const params = TEX_PARAMS.map((p) => [String(p), conv(GL.getTexParameter.call(gl, tgt, p))]);
        const immutable = !!GL.getTexParameter.call(gl, tgt, 0x912F);
        const immutableLevels = GL.getTexParameter.call(gl, tgt, 0x82DF) || 0;
        const intf = info.internalformat;
        const cls = formatClass(intf);
        const layered = tgt === TEXTURE_3D || tgt === TEXTURE_2D_ARRAY;
        const levels = [];
        const sorted = [...info.levels.values()].sort((a, b) => a.level - b.level || a.face - b.face);
        for (const lv of sorted) {
          const layers = layered ? lv.d : 1;
          for (let z = 0; z < layers; z++) {
            let data = null, note = null;
            if (info.shadow) note = info.uncapturable ? 'notCaptured' : 'uploadLog';
            else if (DEPTH_STENCIL.has(intf) || !lv.w || !lv.h) note = 'notCaptured';
            else {
              GL.bindFramebuffer.call(gl, READ_FB, fbo);
              if (layered) GL.framebufferTextureLayer.call(gl, READ_FB, COLOR0, tex, lv.level, z);
              else GL.framebufferTexture2D.call(gl, READ_FB, COLOR0, tgt === TEXTURE_CUBE_MAP ? CUBE_PX + lv.face : TEXTURE_2D, tex, lv.level);
              data = readAttached(lv.w, lv.h, cls);
              GL.framebufferTexture2D.call(gl, READ_FB, COLOR0, TEXTURE_2D, null, 0);
              if (!data) note = 'notCaptured';
            }
            levels.push(dict({ level: lv.level, face: lv.face, layer: z, width: lv.w, height: lv.h, depth: lv.d,
              readClass: cls, data, note }));
          }
        }
        push('webgd:glTexture', [obj(ids.get(tex)), dict({
          target: tgt, internalformat: intf, format: info.format, type: info.type, immutable, immutableLevels,
          params: { t: 'dict', entries: params }, levels: arr(levels),
        })]);
      }
      GL.activeTexture.call(gl, 0x84C0 + scratchUnit);
      UNIT_BINDS.forEach(([t], i) => GL.bindTexture.call(gl, t, unitSave[i]));
      GL.activeTexture.call(gl, sv.active);

      // Buffers.
      for (const buf of L('createBuffer')) {
        const bi = bufInfo.get(buf);
        if (!bi) continue;
        let target = 0x8F36;
        if (bi.isIndex) { GL.bindVertexArray.call(gl, svao); target = 0x8893; }
        GL.bindBuffer.call(gl, target, buf);
        const size = GL.getBufferParameter.call(gl, target, 0x8764) || 0;
        const usage = GL.getBufferParameter.call(gl, target, 0x8765) || 0x88E4;
        let data = null;
        if (size > 0) {
          const out = new Uint8Array(size);
          drain();
          GL.getBufferSubData.call(gl, target, 0, out);
          if (!GL.getError.call(gl)) data = conv(out);
        }
        GL.bindBuffer.call(gl, target, null);
        push('webgd:glBuffer', [obj(ids.get(buf)), dict({ size, usage, isIndex: bi.isIndex, data })]);
      }
      GL.bindVertexArray.call(gl, sv.vao);

      // Renderbuffers.
      for (const rb of L('createRenderbuffer')) {
        GL.bindRenderbuffer.call(gl, RENDERBUFFER, rb);
        const q = (p) => GL.getRenderbufferParameter.call(gl, RENDERBUFFER, p) || 0;
        const width = q(0x8D42), height = q(0x8D43), internalformat = q(0x8D44), samples = q(0x8CAB);
        const readClass = formatClass(internalformat);
        let data = null;
        if (samples === 0 && width > 0 && height > 0 && !DEPTH_STENCIL.has(internalformat)) {
          GL.bindFramebuffer.call(gl, READ_FB, fbo);
          GL.framebufferRenderbuffer.call(gl, READ_FB, COLOR0, RENDERBUFFER, rb);
          data = readAttached(width, height, readClass);
          GL.framebufferRenderbuffer.call(gl, READ_FB, COLOR0, RENDERBUFFER, null);
        }
        push('webgd:glRenderbuffer', [obj(ids.get(rb)), dict({ width, height, internalformat, samples, data, readClass })]);
      }

      // Framebuffers.
      const maxColor = P(0x8CDF), maxDraw = P(0x8824);
      const attList = [];
      for (let i = 0; i < maxColor; i++) attList.push(COLOR0 + i);
      attList.push(0x8D00, 0x8D20, 0x821A);
      for (const fb of L('createFramebuffer')) {
        GL.bindFramebuffer.call(gl, DRAW_FB, fb);
        GL.bindFramebuffer.call(gl, READ_FB, fb);
        const atts = [];
        for (const a of attList) {
          const q = (p) => GL.getFramebufferAttachmentParameter.call(gl, DRAW_FB, a, p);
          const objectType = q(0x8CD0);
          if (!objectType) continue;
          let object = null, level = 0, face = 0, layer = 0;
          object = q(0x8CD1);
          if (objectType === 0x1702) {
            level = q(0x8CD2) || 0;
            const f = q(0x8CD3);
            face = f ? f - CUBE_PX : 0;
            layer = q(0x8CD4) || 0;
          }
          atts.push([String(a), dict({ objectType, object: conv(object), level, face, layer })]);
        }
        const drawBuffers = [];
        for (let i = 0; i < maxDraw; i++) drawBuffers.push(P(0x8825 + i));
        push('webgd:glFramebuffer', [obj(ids.get(fb)), dict({
          attachments: { t: 'dict', entries: atts }, drawBuffers: arr(drawBuffers), readBuffer: P(0x0C02),
        })]);
      }

      // Samplers.
      for (const s of L('createSampler')) {
        push('webgd:glSampler', [obj(ids.get(s)),
          { t: 'dict', entries: SAMPLER_PARAMS.map((p) => [String(p), conv(GL.getSamplerParameter.call(gl, s, p))]) }]);
      }

      // Vertex arrays (null = default VAO).
      for (const v of [null, ...L('createVertexArray')]) {
        GL.bindVertexArray.call(gl, v);
        const attribs = [];
        for (let i = 0; i < maxAttribs; i++) {
          const g = (p) => GL.getVertexAttrib.call(gl, i, p);
          attribs.push(dict({
            enabled: !!g(0x8622), size: g(0x8623), stride: g(0x8624), type: g(0x8625), normalized: !!g(0x886A),
            integer: !!g(0x88FD), divisor: g(0x88FE), buffer: conv(g(0x889F)), offset: GL.getVertexAttribOffset.call(gl, i, 0x8645),
          }));
        }
        push('webgd:glVertexArray', [v ? obj(ids.get(v)) : null, dict({ attribs: arr(attribs), elementBuffer: conv(P(0x8895)) })]);
      }
      GL.bindVertexArray.call(gl, sv.vao);

      // Programs.
      for (const p of L('createProgram')) {
        if (!GL.getProgramParameter.call(gl, p, 0x8B82)) continue;
        const n = GL.getProgramParameter.call(gl, p, 0x8B86) || 0;
        const all = [];
        for (let i = 0; i < n; i++) all.push(i);
        const blockIdx = n ? GL.getActiveUniforms.call(gl, p, all, 0x8A3A) : [];
        const uniforms = [];
        for (let i = 0; i < n; i++) {
          if (blockIdx[i] !== -1) continue;
          const info = GL.getActiveUniform.call(gl, p, i);
          if (!info) continue;
          const isArr = info.name.endsWith('[0]');
          const base = isArr ? info.name.slice(0, -3) : info.name;
          for (let j = 0; j < (isArr ? info.size : 1); j++) {
            const name = isArr ? base + '[' + j + ']' : base;
            const loc = GL.getUniformLocation.call(gl, p, name);
            if (!loc) continue;
            uniforms.push(arr([name, info.type, conv(GL.getUniform.call(gl, p, loc))]));
          }
        }
        const blocks = [];
        const nb = GL.getProgramParameter.call(gl, p, 0x8A36) || 0;
        for (let i = 0; i < nb; i++) {
          blocks.push([GL.getActiveUniformBlockName.call(gl, p, i), GL.getActiveUniformBlockParameter.call(gl, p, i, 0x8A3F)]);
        }
        push('webgd:glProgram', [obj(ids.get(p)), arr(uniforms), { t: 'dict', entries: blocks }]);
      }

      // Default framebuffer.
      const attrs = GL.getContextAttributes.call(gl);
      const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
      let fbData = null;
      if (attrs && attrs.preserveDrawingBuffer && w && h) {
        GL.bindFramebuffer.call(gl, READ_FB, null);
        const out = new Uint8Array(w * h * 4);
        drain();
        GL.readPixels.call(gl, 0, 0, w, h, 0x1908, 0x1401, out);
        if (!GL.getError.call(gl)) fbData = conv(out);
      }
      push('webgd:glDefaultFramebuffer', [dict({ width: w, height: h, data: fbData })]);
      push('webgd:glState', [{ t: 'dict', entries: st }]);

      // Restore.
      GL.bindFramebuffer.call(gl, DRAW_FB, sv.draw);
      GL.bindFramebuffer.call(gl, READ_FB, sv.read);
      GL.bindVertexArray.call(gl, sv.vao);
      GL.activeTexture.call(gl, sv.active);
      GL.bindBuffer.call(gl, 0x8F36, sv.copyRead);
      GL.bindBuffer.call(gl, 0x88EB, sv.pack);
      GL.pixelStorei.call(gl, 0x0D05, sv.packAlign);
      GL.pixelStorei.call(gl, 0x0D02, sv.packRow);
      GL.pixelStorei.call(gl, 0x0D03, sv.packSkipRows);
      GL.pixelStorei.call(gl, 0x0D04, sv.packSkipPix);
      GL.bindRenderbuffer.call(gl, RENDERBUFFER, sv.rb);
      GL.deleteFramebuffer.call(gl, fbo);
      GL.deleteVertexArray.call(gl, svao);
      drain();
      if (appErrors.length) pendingErrors.set(gl, appErrors);
    }

    // --- WebGPU snapshot -------------------------------------------------------------------------
    const GPU_DS = new Set(['stencil8', 'depth16unorm', 'depth24plus', 'depth24plus-stencil8', 'depth32float', 'depth32float-stencil8']);
    // format -> [bytesPerBlock, blockWidth, blockHeight]
    function gpuFormatInfo(f) {
      const plain = {
        r8unorm: 1, r8snorm: 1, r8uint: 1, r8sint: 1,
        r16uint: 2, r16sint: 2, r16float: 2, r16unorm: 2, r16snorm: 2, rg8unorm: 2, rg8snorm: 2, rg8uint: 2, rg8sint: 2,
        r32uint: 4, r32sint: 4, r32float: 4, rg16uint: 4, rg16sint: 4, rg16float: 4, rg16unorm: 4, rg16snorm: 4,
        rgba8unorm: 4, 'rgba8unorm-srgb': 4, rgba8snorm: 4, rgba8uint: 4, rgba8sint: 4, bgra8unorm: 4, 'bgra8unorm-srgb': 4,
        rgb9e5ufloat: 4, rgb10a2uint: 4, rgb10a2unorm: 4, rg11b10ufloat: 4,
        rg32uint: 8, rg32sint: 8, rg32float: 8, rgba16uint: 8, rgba16sint: 8, rgba16float: 8, rgba16unorm: 8, rgba16snorm: 8,
        rgba32uint: 16, rgba32sint: 16, rgba32float: 16,
      };
      if (f in plain) return [plain[f], 1, 1];
      if (/^bc(1|4)-/.test(f)) return [8, 4, 4];
      if (/^bc[235-7]-/.test(f)) return [16, 4, 4];
      if (/^etc2-rgb8(a1)?unorm/.test(f) || /^eac-r11/.test(f)) return [8, 4, 4];
      if (/^etc2-rgba8unorm/.test(f) || /^eac-rg11/.test(f)) return [16, 4, 4];
      const m = /^astc-(\d+)x(\d+)-/.exec(f);
      if (m) return [16, +m[1], +m[2]];
      return null;
    }

    function gpuReadback(dev, size, encode, onData, rec) {
      const staging = gpu('GPUDevice', 'createBuffer').call(dev, { size, usage: BU.MAP_READ | BU.COPY_DST });
      gpu('GPUDevice', 'pushErrorScope').call(dev, 'validation');
      const enc = gpu('GPUDevice', 'createCommandEncoder').call(dev);
      encode(enc, staging);
      const queue = ORIG['GPUDevice.get:queue'].call(dev);
      gpu('GPUQueue', 'submit').call(queue, [gpu('GPUCommandEncoder', 'finish').call(enc)]);
      const errP = gpu('GPUDevice', 'popErrorScope').call(dev);
      const mapP = gpu('GPUBuffer', 'mapAsync').call(staging, 1);
      snapshotPromises.push(Promise.all([errP, mapP]).then(([err]) => {
        if (err) rec.args[1] = 'copyFailed: ' + err.message;
        else onData(new Uint8Array(gpu('GPUBuffer', 'getMappedRange').call(staging)));
        gpu('GPUBuffer', 'unmap').call(staging);
        gpu('GPUBuffer', 'destroy').call(staging);
      }).catch((e) => {
        rec.args[0] = null;
        rec.args[1] = 'readbackFailed: ' + (e && e.message);
        try { gpu('GPUBuffer', 'destroy').call(staging); } catch (_) { /* device gone */ }
      }));
    }

    function snapshotGpuBuffer(dev, buf) {
      const rec = { thisId: ids.get(buf), method: 'webgd:gpuBuffer', args: [null, null], result: undefined };
      snapshotRecs.push(rec);
      const size = Number(buf.size);
      if (buf.mapState !== 'unmapped') { rec.args[1] = 'mapped'; return; }
      if (buf.usage & BU.MAP_READ) { rec.args[1] = 'mapRead'; return; }
      if (!(buf.usage & BU.COPY_SRC)) { rec.args[1] = 'noCopySrc'; return; }
      const copySize = size - (size % 4);
      if (copySize === 0) { rec.args[0] = { t: 'typed', kind: 'Uint8Array', bytes: new Uint8Array(size) }; return; }
      gpuReadback(dev, copySize, (enc, staging) => {
        gpu('GPUCommandEncoder', 'copyBufferToBuffer').call(enc, buf, 0, staging, 0, copySize);
      }, (mapped) => {
        const out = new Uint8Array(size);
        out.set(mapped.subarray(0, copySize));
        rec.args[0] = { t: 'typed', kind: 'Uint8Array', bytes: out };
      }, rec);
    }

    function snapshotGpuTexture(dev, tex) {
      const rec = { thisId: ids.get(tex), method: 'webgd:gpuTexture', args: [null, null], result: undefined };
      snapshotRecs.push(rec);
      const fmt = tex.format;
      if (tex.sampleCount > 1) { rec.args[1] = 'multisampled'; return; }
      if (GPU_DS.has(fmt)) { rec.args[1] = 'depthStencil'; return; }
      if (!(tex.usage & TU_COPY_SRC)) { rec.args[1] = 'noCopySrc'; return; }
      const fi = gpuFormatInfo(fmt);
      if (!fi) { rec.args[1] = 'unknownFormat'; return; }
      const [bpb, bw, bh] = fi;
      const subs = [];
      let total = 0;
      for (let mip = 0; mip < tex.mipLevelCount; mip++) {
        const w = Math.max(1, tex.width >> mip), h = Math.max(1, tex.height >> mip);
        const pw = Math.ceil(w / bw) * bw, ph = Math.ceil(h / bh) * bh;
        const tight = (pw / bw) * bpb, rows = ph / bh;
        const paddedRow = Math.ceil(tight / 256) * 256;
        const layers = tex.dimension === '3d' ? Math.max(1, tex.depthOrArrayLayers >> mip) : tex.depthOrArrayLayers;
        for (let layer = 0; layer < layers; layer++) {
          subs.push({ mip, layer, pw, ph, tight, rows, paddedRow, offset: total });
          total += paddedRow * rows;
        }
      }
      if (!total) { rec.args[0] = arr([]); return; }
      gpuReadback(dev, total, (enc, staging) => {
        for (const s of subs) {
          gpu('GPUCommandEncoder', 'copyTextureToBuffer').call(enc,
            { texture: tex, mipLevel: s.mip, origin: [0, 0, s.layer] },
            { buffer: staging, offset: s.offset, bytesPerRow: s.paddedRow, rowsPerImage: s.rows },
            [s.pw, s.ph, 1]);
        }
      }, (mapped) => {
        rec.args[0] = arr(subs.map((s) => {
          const data = new Uint8Array(s.tight * s.rows);
          for (let r = 0; r < s.rows; r++) {
            const o = s.offset + r * s.paddedRow;
            data.set(mapped.subarray(o, o + s.tight), r * s.tight);
          }
          return dict({ mip: s.mip, layer: s.layer, width: s.pw, height: s.ph, depthOrLayers: 1, bytesPerRow: s.tight,
            rowsPerImage: s.rows, data: { t: 'typed', kind: 'Uint8Array', bytes: data } });
        }));
      }, rec);
    }

    function snapshotGPU() {
      for (const e of setupLog) {
        if (e.method !== 'createBuffer' && e.method !== 'createTexture') continue;
        const o = e.ref && e.ref.deref();
        if (!o || destroyed.has(o)) continue;
        const dev = devOf.get(o);
        if (!dev) continue; // WebGL buffer
        try {
          if (e.method === 'createBuffer') snapshotGpuBuffer(dev, o);
          else snapshotGpuTexture(dev, o);
        } catch (err) { internalError(err); }
      }
    }

    function runSnapshot() {
      prune();
      setupCopy = setupLog.slice();
      for (const r of glContexts) {
        const gl = r.deref();
        if (gl) snapshotGL(gl);
      }
      snapshotGPU();
      for (const r of canvases) {
        const c = r.deref();
        if (c && ids.has(c)) snapshotRecs.push({ thisId: ids.get(c), method: 'webgd:canvasSize', args: [c.width, c.height], result: undefined });
      }
    }

    // --- capture lifecycle --------------------------------------------------------------------------------
    function resetCapture() {
      setupCopy = []; snapshotRecs = []; snapshotPromises = []; frameRecs = [];
      captureTs = -1;
    }

    async function finalize() {
      try {
        let timer = 0;
        const timeout = new Promise((_, rej) => { timer = origSetTimeout(() => rej(new Error('snapshot readback timed out')), 10000); });
        await Promise.race([Promise.all(snapshotPromises), timeout]);
        origClearTimeout(timer);
        const bytes = new WireWriter(location.href, objectTable).encode({ setup: setupCopy, snapshot: snapshotRecs, frame: frameRecs });
        post({ __webgd: 'to-bridge', type: 'capture', bytes }, '*', [bytes.buffer]);
      } catch (e) {
        sendError('capture failed: ' + (e && e.message ? e.message : e));
      } finally {
        resetCapture();
        mode = IDLE;
      }
    }

    function onFrame(ts) {
      if (mode === ARMED) {
        origClearTimeout(armTimer);
        try {
          runSnapshot();
        } catch (e) {
          resetCapture();
          mode = IDLE;
          sendError('snapshot failed: ' + (e && e.message ? e.message : e));
          return;
        }
        captureTs = ts;
        mode = CAPTURING;
      } else if (mode === CAPTURING && ts !== captureTs) {
        mode = FINALIZING;
        finalize();
      }
    }

    function arm() {
      if (mode !== IDLE) { sendError('capture already in progress'); return; }
      resetCapture();
      mode = ARMED;
      armTimer = origSetTimeout(() => {
        if (mode !== ARMED) return;
        mode = IDLE;
        sendError('target does not render with requestAnimationFrame');
      }, 2000);
    }

    // --- page overlay ----------------------------------------------------------------------------------
    // Small "capturable" badge with FPS / frame counters. Built only with DOM originals captured here,
    // lives in a closed shadow root, never touches canvases or GPU objects and never records anything.
    // Updated from the rAF wrapper below (throttled to ~4 Hz) and on status changes; no own timers/loops.
    const OV_INTERVAL_MS = 250;
    const OV_WINDOW_MS = 1000;
    const ovDom = {
      createElement: Document.prototype.createElement,
      attachShadow: Element.prototype.attachShadow,
      appendChild: Node.prototype.appendChild,
      setAttribute: Element.prototype.setAttribute,
      setText: Object.getOwnPropertyDescriptor(Node.prototype, 'textContent').set,
      isConnected: Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected').get,
      documentElement: Object.getOwnPropertyDescriptor(Document.prototype, 'documentElement').get,
    };
    const OV_HOST_STYLE = 'all: initial !important; display: block !important; position: fixed !important; '
      + 'top: 8px !important; right: 8px !important; z-index: 2147483647 !important; pointer-events: none !important; '
      + 'margin: 0 !important; padding: 0 !important; border: 0 !important; width: auto !important; height: auto !important; '
      + 'transform: none !important; opacity: 1 !important; visibility: visible !important;';
    const OV_BOX_STYLE = 'margin: 0; padding: 4px 6px; border-radius: 4px; background: rgba(0, 0, 0, 0.65); '
      + 'color: #e6e6e6; font: 11px/1.35 ui-monospace, Menlo, Consolas, monospace; white-space: pre; '
      + 'pointer-events: none; user-select: none;';
    let ovHost = null;
    let ovBox = null;
    let ovText = '';
    let ovLastRender = -Infinity;
    let ovFrames = 0;
    let ovLastTs = -1;
    const ovTimes = [];
    let ovTimesHead = 0;

    function ovHeadline() {
      if (status.gl2 > 0 || status.gpuDevices > 0) return 'Webgd capturable';
      if (status.webgl1 > 0) return 'Webgd: WebGL1 not supported';
      return null;
    }

    function ovFps() {
      const n = ovTimes.length - ovTimesHead;
      if (n < 2) return '--';
      const span = ovTimes[ovTimes.length - 1] - ovTimes[ovTimesHead];
      return span > 0 ? String(Math.round(((n - 1) * 1000) / span)) : '--';
    }

    // Ensures the host is attached to the current document element; returns false when not possible yet
    // (document_start before <html> exists).
    function ovEnsureHost() {
      const root = ovDom.documentElement.call(document);
      if (!root) return false;
      if (!ovHost) {
        const host = ovDom.createElement.call(document, 'div');
        ovDom.setAttribute.call(host, 'style', OV_HOST_STYLE);
        const shadow = ovDom.attachShadow.call(host, { mode: 'closed' });
        const box = ovDom.createElement.call(document, 'div');
        ovDom.setAttribute.call(box, 'style', OV_BOX_STYLE);
        ovDom.appendChild.call(shadow, box);
        ovHost = host;
        ovBox = box;
        ovText = '';
      }
      if (!ovDom.isConnected.call(ovHost)) ovDom.appendChild.call(root, ovHost);
      return true;
    }

    function overlayRender() {
      const head = ovHeadline();
      if (head === null) return;
      if (!ovEnsureHost()) return;
      const busy = mode !== IDLE ? ' capturing\u2026' : '';
      const text = head + busy + '\nFPS ' + ovFps() + '\nFrame ' + ovFrames;
      if (text !== ovText) {
        ovDom.setText.call(ovBox, text);
        ovText = text;
      }
    }

    // Called once per rAF callback; several callbacks in one frame share a timestamp.
    function overlayTick(ts) {
      if (ts === ovLastTs) return;
      const capturingChanged = (mode !== IDLE) !== ovText.includes('capturing\u2026');
      ovLastTs = ts;
      ovFrames++;
      ovTimes.push(ts);
      while (ovTimesHead < ovTimes.length && ovTimes[ovTimesHead] < ts - OV_WINDOW_MS) ovTimesHead++;
      if (ovTimesHead > 512) { ovTimes.splice(0, ovTimesHead); ovTimesHead = 0; }
      if (!capturingChanged && ts - ovLastRender < OV_INTERVAL_MS && ts >= ovLastRender) return;
      ovLastRender = ts;
      overlayRender();
    }

    const origRAF = window.requestAnimationFrame;
    window.requestAnimationFrame = function requestAnimationFrame(cb) {
      if (typeof cb !== 'function') return origRAF.call(this, cb);
      return origRAF.call(this, function (ts) {
        try { onFrame(ts); } catch (e) { internalError(e); }
        try { overlayTick(ts); } catch (e) { internalError(e); }
        return cb.call(this, ts);
      });
    };

    window.addEventListener('message', (ev) => {
      if (ev.source !== window) return;
      const d = ev.data;
      if (!d || d.__webgd !== 'to-hook') return;
      if (d.type === 'arm') arm();
    });

    // --- user marker API ---------------------------------------------------------------------------------
    function marker(method, args, ctx) {
      if (mode !== CAPTURING) return;
      const c = ctx || lastGL;
      frameRecs.push({ thisId: c ? idOf(c, 0) : 0, method, args, result: undefined });
    }
    Object.defineProperty(window, '__webgd', {
      configurable: false, enumerable: false, writable: false,
      value: Object.freeze({
        pushMarker(name, ctx) { marker('webgd:pushMarker', [String(name)], ctx); },
        popMarker(ctx) { marker('webgd:popMarker', [], ctx); },
        insertMarker(name, ctx) { marker('webgd:insertMarker', [String(name)], ctx); },
      }),
    });
  }

  if (typeof window === 'object' && typeof document === 'object') {
    const INSTALLED = Symbol.for('webgd.hook');
    if (!window[INSTALLED]) {
      Object.defineProperty(window, INSTALLED, { value: true });
      try { install(); } catch (e) { console.error('webgd: hook install failed', e); }
    }
  }

  if (typeof module === 'object') module.exports = { WireWriter };
})();
