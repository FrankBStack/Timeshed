// Runs the routing engine off the main thread. Messages in:
//   {type: 'load', bundleUrl}            -> progress {type:'progress', loaded, total}, then {type:'ready', info}
//   {type: 'isochrone', id, params}      -> {type:'isochrone', id, geojson, ms} or {type:'error', id, message}
import init, { WebEngine } from './pkg/timeshed.js';

let engine = null;

async function fetchBundle(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`bundle: HTTP ${res.status}`);
  const total = Number(res.headers.get('Content-Length')) || 0;
  // Count bytes as they arrive. If the body starts with the gzip magic the
  // host served it compressed as-is and we inflate here; if the host already
  // applied Content-Encoding the browser has inflated it for us.
  let loaded = 0;
  let gz = null;
  const counted = new ReadableStream({
    async start(controller) {
      const reader = res.body.getReader();
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (gz === null) gz = value.length >= 2 && value[0] === 0x1f && value[1] === 0x8b;
        loaded += value.byteLength;
        postMessage({ type: 'progress', loaded, total });
        controller.enqueue(value);
      }
      controller.close();
    },
  });
  // peek the first chunk to decide, then rebuild a stream from it
  const reader = counted.getReader();
  const first = await reader.read();
  const rest = new ReadableStream({
    async start(controller) {
      if (!first.done) controller.enqueue(first.value);
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        controller.enqueue(value);
      }
      controller.close();
    },
  });
  const stream = gz ? rest.pipeThrough(new DecompressionStream('gzip')) : rest;
  const buf = await new Response(stream).arrayBuffer();
  return new Uint8Array(buf);
}

onmessage = async (e) => {
  const msg = e.data;
  try {
    if (msg.type === 'load') {
      await init();
      const bytes = await fetchBundle(msg.bundleUrl);
      const t0 = performance.now();
      engine = new WebEngine(bytes);
      postMessage({ type: 'ready', info: JSON.parse(engine.info()), bytes: bytes.byteLength, ms: performance.now() - t0 });
    } else if (msg.type === 'isochrone') {
      if (!engine) throw new Error('engine not loaded');
      const p = msg.params;
      const t0 = performance.now();
      const geojson = engine.isochrone(p.lat, p.lon, p.date, p.time, p.max, p.band, p.cell || 100, p.walk_speed || 1.3);
      postMessage({ type: 'isochrone', id: msg.id, geojson, ms: performance.now() - t0 });
    }
  } catch (err) {
    postMessage({ type: 'error', id: msg.id, message: err.message || String(err) });
  }
};
