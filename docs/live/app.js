// Timeshed live map. One server call per pin move / time change; the
// cutoff slider just filters the bands the server already returned.

const BAND = 5;      // minutes per band, must match the request
const MAX = 60;      // outermost band requested

// one hue, light -> dark: far -> near. 12 bands of 5 min up to 60.
const RAMP = ['#0d366b', '#104281', '#184f95', '#1c5cab', '#256abf', '#2a78d6',
              '#3987e5', '#5598e7', '#6da7ec', '#86b6ef', '#9ec5f4', '#b7d3f6'];

const $ = (id) => document.getElementById(id);
const state = { lat: null, lon: null, info: null, pending: null, timer: null };

// Two ways to reach the engine: the native server's HTTP API, or the same
// engine compiled to WebAssembly running in a worker (when the page sets
// window.TIMESHED_BUNDLE_URL). The rest of the page does not care which.
const backend = window.TIMESHED_BUNDLE_URL ? wasmBackend(window.TIMESHED_BUNDLE_URL) : httpBackend();

function httpBackend() {
  return {
    async info() { return (await fetch('/api/info')).json(); },
    async isochrone(params) {
      const res = await fetch('/api/isochrone?' + new URLSearchParams(params).toString());
      if (!res.ok) throw new Error(await res.text());
      const fc = await res.json();
      return { fc, note: `routed in ${fc.properties.query_ms} ms, drawn in ${fc.properties.total_ms} ms` };
    },
  };
}

function wasmBackend(bundleUrl) {
  const worker = new Worker('worker.js', { type: 'module' });
  const waiting = new Map();
  let nextId = 1;
  const ready = new Promise((resolve, reject) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'progress') {
        const pct = m.total ? Math.round(100 * m.loaded / m.total) : null;
        $('stats').textContent = pct === null
          ? `downloading the network… ${(m.loaded / 1e6).toFixed(1)} MB`
          : `downloading the network… ${pct}%`;
      } else if (m.type === 'ready') {
        $('stats').textContent = `${(m.bytes / 1e6).toFixed(0)} MB bundle loaded in ${m.ms.toFixed(0)} ms, routing in your browser`;
        resolve(m.info);
      } else if (m.type === 'isochrone' || m.type === 'error') {
        const w = waiting.get(m.id);
        waiting.delete(m.id);
        if (!w) return;
        if (m.type === 'error') w.reject(new Error(m.message));
        else w.resolve({ fc: JSON.parse(m.geojson), note: `routed and drawn in your browser in ${m.ms.toFixed(0)} ms` });
      }
    };
    worker.onerror = (e) => reject(new Error(e.message));
  });
  worker.postMessage({ type: 'load', bundleUrl });
  return {
    info() { return ready; },
    async isochrone(params) {
      await ready;
      const id = nextId++;
      return new Promise((resolve, reject) => {
        waiting.set(id, { resolve, reject });
        worker.postMessage({ type: 'isochrone', id, params });
      });
    },
  };
}

function readHash() {
  const h = new URLSearchParams(location.hash.slice(1));
  if (h.has('lat') && h.has('lon')) {
    state.lat = parseFloat(h.get('lat'));
    state.lon = parseFloat(h.get('lon'));
  }
  if (h.has('t')) $('time').value = h.get('t');
  if (h.has('d')) $('date').value = h.get('d');
  if (h.has('m')) $('cutoff').value = h.get('m');
}

function writeHash() {
  const h = new URLSearchParams({
    lat: state.lat.toFixed(5), lon: state.lon.toFixed(5),
    d: $('date').value, t: $('time').value, m: $('cutoff').value,
  });
  history.replaceState(null, '', '#' + h.toString());
}

function nextWeekday(from, first, last) {
  // the feed's first valid date on or after `from`, nudged to a weekday
  let d = new Date(Math.max(from, first));
  if (d > last) d = new Date(first);
  for (let i = 0; i < 7; i++) {
    const dow = d.getUTCDay();
    if (dow >= 1 && dow <= 5) break;
    d.setUTCDate(d.getUTCDate() + 1);
  }
  return d.toISOString().slice(0, 10);
}

function bandColor(from) {
  return RAMP[Math.min(RAMP.length - 1, Math.floor(from / BAND))];
}

function buildLegend() {
  const legend = $('legend');
  legend.innerHTML = '';
  for (let m = 0; m < MAX; m += BAND) {
    const s = document.createElement('span');
    s.style.background = bandColor(m);
    s.title = `${m}–${m + BAND} min`;
    legend.appendChild(s);
  }
  const labels = document.createElement('div');
  labels.id = 'legend-labels';
  labels.innerHTML = '<span>0 min</span><span>near → far</span><span>60 min</span>';
  legend.after(labels);
}

async function init() {
  $('stats').textContent = 'loading…';
  const info = await backend.info();
  state.info = info;
  $('feed').textContent = `${info.name} · ${info.stops.toLocaleString()} stops · ${info.walk_nodes.toLocaleString()} walking nodes`;

  const first = new Date(info.first_date), last = new Date(info.last_date);
  const dateEl = $('date');
  dateEl.min = info.first_date;
  dateEl.max = info.last_date;
  readHash();
  if (!dateEl.value) dateEl.value = nextWeekday(new Date(), first, last);
  if (state.lat === null) { state.lon = info.center[0]; state.lat = info.center[1]; }
  buildLegend();

  const map = new maplibregl.Map({
    container: 'map',
    style: 'https://tiles.openfreemap.org/styles/positron',
    center: [state.lon, state.lat],
    zoom: 11,
    attributionControl: { compact: true },
  });
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');

  const marker = new maplibregl.Marker({ draggable: true, color: '#a3262c' })
    .setLngLat([state.lon, state.lat])
    .addTo(map);

  map.on('load', () => {
    map.addSource('iso', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
    const colorExpr = ['step', ['get', 'from'], RAMP[0]];
    for (let i = 1; i < RAMP.length; i++) colorExpr.push(i * BAND, RAMP[i]);
    map.addLayer({
      id: 'iso-fill', type: 'fill', source: 'iso',
      paint: { 'fill-color': colorExpr, 'fill-opacity': 0.55 },
    }, firstSymbolLayer(map));
    map.addLayer({
      id: 'iso-line', type: 'line', source: 'iso',
      paint: { 'line-color': '#fcfcfb', 'line-width': 0.6, 'line-opacity': 0.7 },
    }, firstSymbolLayer(map));
    applyCutoff(map);
    query(map);
  });

  marker.on('dragend', () => {
    const p = marker.getLngLat();
    state.lon = p.lng; state.lat = p.lat;
    query(map);
  });
  map.on('click', (e) => {
    if (e.originalEvent.target.closest('#panel')) return;
    marker.setLngLat(e.lngLat);
    state.lon = e.lngLat.lng; state.lat = e.lngLat.lat;
    query(map);
  });
  $('cutoff').addEventListener('input', () => { applyCutoff(map); writeHash(); });
  $('time').addEventListener('change', () => query(map));
  $('date').addEventListener('change', () => query(map));
}

function firstSymbolLayer(map) {
  const layer = map.getStyle().layers.find((l) => l.type === 'symbol');
  return layer ? layer.id : undefined;
}

function applyCutoff(map) {
  const m = parseInt($('cutoff').value, 10);
  $('cutoff-out').textContent = m;
  for (const id of ['iso-fill', 'iso-line']) map.setFilter(id, ['<=', ['get', 'to'], m]);
}

async function query(map) {
  if (!$('date').value || !$('time').value) return;
  writeHash();
  const params = { lat: state.lat, lon: state.lon, date: $('date').value, time: $('time').value, max: MAX, band: BAND };
  const token = JSON.stringify(params);
  state.pending = token;
  $('stats').textContent = 'routing…';
  $('error').hidden = true;
  try {
    const { fc, note } = await backend.isochrone(params);
    if (state.pending !== token) return; // a newer query superseded this one
    map.getSource('iso').setData(fc);
    const p = fc.properties || {};
    $('stats').textContent =
      `${(p.stops_by_transit || 0).toLocaleString()} stops reachable by transit within ${MAX} min · ${note}`;
  } catch (err) {
    $('stats').textContent = '';
    $('error').textContent = err.message;
    $('error').hidden = false;
    map.getSource('iso').setData({ type: 'FeatureCollection', features: [] });
  }
}

init();
