#!/usr/bin/env node
// Regenerates tests/fixtures/sample.wire with the real WireWriter from extension/hook.js.
'use strict';
const fs = require('fs');
const path = require('path');
const { WireWriter } = require('../extension/hook.js');

const f32 = new Uint8Array(new Float32Array([1, 2, 3]).buffer);
const objects = [
  { id: 1, cls: 'WebGL2RenderingContext', parent: 0 },
  { id: 2, cls: 'WebGLBuffer', parent: 1 },
];
const bytes = new WireWriter('http://fixture/', objects).encode({
  setup: [{ thisId: 1, method: 'createBuffer', args: [], result: { t: 'obj', id: 2 } }],
  snapshot: [],
  frame: [
    { thisId: 1, method: 'bufferData', args: [34962, { t: 'typed', kind: 'Float32Array', bytes: f32 }, 35044], result: undefined },
    { thisId: 1, method: 'webgd:pushMarker', args: ['pass', { t: 'dict', entries: [['a', { t: 'array', items: [true, null] }]] }], result: undefined },
  ],
});
const out = path.join(__dirname, '..', 'tests', 'fixtures', 'sample.wire');
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, bytes);
console.log(`wrote ${out} (${bytes.length} bytes)`);
