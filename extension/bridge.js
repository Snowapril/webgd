// webgd bridge. Runs in the extension's ISOLATED world at document_start and relays between
// hook.js (window.postMessage, keyed by __webgd) and the webgd UI (chrome.runtime Port 'webgd-target').
(() => {
  'use strict';
  if (globalThis.__webgdBridge) return;
  globalThis.__webgdBridge = true;

  // Raw bytes per chunk message. Extension messaging is JSON-serialized, hence base64.
  const CHUNK_BYTES = 4 << 20;
  const B64_SUBCHUNK = 32 << 10;

  let port = null;
  let status = { gl2: 0, webgl1: 0, gpuDevices: 0 };

  function send(msg) {
    if (!port) return false;
    try {
      port.postMessage(msg);
      return true;
    } catch (_) {
      port = null;
      return false;
    }
  }

  function connect() {
    if (port) {
      const old = port;
      port = null;
      try { old.disconnect(); } catch (_) { /* already gone */ }
    }
    let p;
    try {
      p = chrome.runtime.connect({ name: 'webgd-target' });
    } catch (_) {
      return; // extension context invalidated
    }
    port = p;
    p.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      if (port === p) port = null;
    });
    p.onMessage.addListener((msg) => {
      if (msg && msg.type === 'capture') window.postMessage({ __webgd: 'to-hook', type: 'arm' }, '*');
    });
    send({ type: 'hello', url: location.href, title: document.title, isFrame: window !== window.top, status: { ...status } });
    send({ type: 'status', ...status });
  }

  function toBase64(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += B64_SUBCHUNK) {
      s += String.fromCharCode.apply(null, bytes.subarray(i, i + B64_SUBCHUNK));
    }
    return btoa(s);
  }

  function sendCapture(bytes) {
    if (!(bytes instanceof Uint8Array)) bytes = new Uint8Array(bytes);
    if (!send({ type: 'captureBegin', total: bytes.length })) return;
    let index = 0;
    for (let off = 0; off < bytes.length; off += CHUNK_BYTES) {
      if (!send({ type: 'chunk', index: index++, b64: toBase64(bytes.subarray(off, off + CHUNK_BYTES)) })) return;
    }
    send({ type: 'captureEnd' });
  }

  window.addEventListener('message', (ev) => {
    if (ev.source !== window) return;
    const d = ev.data;
    if (!d || d.__webgd !== 'to-bridge') return;
    switch (d.type) {
      case 'status':
        status = { gl2: d.gl2 | 0, webgl1: d.webgl1 | 0, gpuDevices: d.gpuDevices | 0 };
        send({ type: 'status', ...status });
        break;
      case 'capture':
        sendCapture(d.bytes);
        break;
      case 'captureError':
        send({ type: 'captureError', message: String(d.message) });
        break;
    }
  });

  chrome.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === 'webgd-ui-hello') connect();
  });

  // document.title is empty at document_start; say hello once the DOM exists.
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', connect, { once: true });
  else connect();
})();
