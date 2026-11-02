// Shared geometry/math for the webgd fixture pages.
'use strict';
window.webgdFixture = (() => {
  // 24 vertices (vec3 pos + vec2 uv, interleaved), 36 Uint16 indices.
  function cube() {
    const faces = [
      [[1, 0, 0], [0, 1, 0], [0, 0, 1]], [[-1, 0, 0], [0, 1, 0], [0, 0, -1]],
      [[0, 1, 0], [0, 0, 1], [1, 0, 0]], [[0, -1, 0], [0, 0, -1], [1, 0, 0]],
      [[0, 0, 1], [1, 0, 0], [0, 1, 0]], [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
    ];
    const v = [], idx = [];
    faces.forEach(([n, u, w], f) => {
      for (const [a, b] of [[-1, -1], [1, -1], [1, 1], [-1, 1]]) {
        v.push(n[0] + a * u[0] + b * w[0], n[1] + a * u[1] + b * w[1], n[2] + a * u[2] + b * w[2], (a + 1) / 2, (b + 1) / 2);
      }
      const o = f * 4;
      idx.push(o, o + 1, o + 2, o, o + 2, o + 3);
    });
    return { vertices: new Float32Array(v), indices: new Uint16Array(idx) };
  }

  // RGBA8 checkerboard, 8x8-texel cells, values 0 or 255.
  function checker(n) {
    const d = new Uint8Array(n * n * 4);
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        const on = ((x >> 3) ^ (y >> 3)) & 1;
        const i = (y * n + x) * 4;
        d[i] = d[i + 1] = d[i + 2] = on ? 255 : 0;
        d[i + 3] = 255;
      }
    }
    return d;
  }

  // Column-major 4x4 helpers.
  function mul(a, b) {
    const r = new Float32Array(16);
    for (let c = 0; c < 4; c++) {
      for (let row = 0; row < 4; row++) {
        let s = 0;
        for (let k = 0; k < 4; k++) s += a[k * 4 + row] * b[c * 4 + k];
        r[c * 4 + row] = s;
      }
    }
    return r;
  }
  // zeroToOne: WebGPU clip-space depth [0,1]; otherwise GL [-1,1].
  function perspective(fovy, aspect, near, far, zeroToOne) {
    const f = 1 / Math.tan(fovy / 2);
    const m = new Float32Array(16);
    m[0] = f / aspect;
    m[5] = f;
    m[11] = -1;
    if (zeroToOne) {
      m[10] = far / (near - far);
      m[14] = (near * far) / (near - far);
    } else {
      m[10] = (far + near) / (near - far);
      m[14] = (2 * far * near) / (near - far);
    }
    return m;
  }
  function rotation(t) {
    const ca = Math.cos(t), sa = Math.sin(t), cb = Math.cos(t * 0.7), sb = Math.sin(t * 0.7);
    const ry = new Float32Array([ca, 0, -sa, 0, 0, 1, 0, 0, sa, 0, ca, 0, 0, 0, 0, 1]);
    const rx = new Float32Array([1, 0, 0, 0, 0, cb, sb, 0, 0, -sb, cb, 0, 0, 0, 0, 1]);
    return mul(ry, rx);
  }
  function mvp(t, aspect, zeroToOne) {
    const view = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, -4, 1]);
    return mul(perspective(Math.PI / 3, aspect, 0.1, 100, zeroToOne), mul(view, rotation(t)));
  }

  return { cube, checker, mvp };
})();
