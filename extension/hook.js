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

  if (typeof module === 'object') module.exports = { WireWriter };
})();
