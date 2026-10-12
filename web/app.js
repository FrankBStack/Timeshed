// Timeshed: one map, two modes.
//   Explore  - drop a pin, the engine draws everything reachable (live routing)
//   Findings - job access by census block, by departure window (precomputed)
// The engine is reached either through the native server's /api or, when the
// page names a bundle, through the same engine compiled to wasm in a worker.
// Findings only appear when the page says where the block data is.

const CFG = Object.assign({ base: '', mode: 'explore', bundleUrl: null, findings: null }, window.TIMESHED || {});
const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- shared --
const BAND = 5;   // minutes per isochrone band
const MAX = 60;   // outermost band requested
// one hue, light -> dark: far -> near, 12 bands
const ISO_RAMP = ['#0d366b', '#104281', '#184f95', '#1c5cab', '#256abf', '#2a78d6',
                  '#3987e5', '#5598e7', '#6da7ec', '#86b6ef', '#9ec5f4', '#b7d3f6'];
const BLUES = ['#cde2fb', '#9ec5f4', '#6da7ec', '#3987e5', '#256abf', '#184f95', '#0d366b'];
const ORANGES = ['#fbe4d8', '#f7bda2', '#f29568', '#eb6834', '#c04e21', '#8c3815'];

const state = {
  mode: null,
  map: null,
  firstSymbol: undefined,
  // explore
  lat: null, lon: null, marker: null, pending: null, info: null, exploreReady: false,
  // findings
  view: null, findingsLoaded: false, hovered: null,
};

function hashParams() { return new URLSearchParams(location.hash.slice(1)); }
function writeHash() {
  const h = new URLSearchParams();
  h.set('mode', state.mode);
  if (state.mode === 'explore' && state.lat !== null) {
    h.set('lat', state.lat.toFixed(5)); h.set('lon', state.lon.toFixed(5));
    h.set('d', $('date').value); h.set('t', $('time').value); h.set('m', $('cutoff').value);
  }
  if (state.mode === 'findings' && state.view) h.set('view', state.view.key);
  history.replaceState(null, '', '#' + h.toString());
}
function firstSymbolLayer(map) {
  const layer = map.getStyle().layers.find((l) => l.type === 'symbol');
  return layer ? layer.id : undefined;
}
function fmtInt(n) { return Math.round(n).toLocaleString(); }
function setVisible(id, on) { if (state.map.getLayer(id)) state.map.setLayoutProperty(id, 'visibility', on ? 'visible' : 'none'); }

// --------------------------------------------------------------- backend --
function httpBackend() {
  return {
    kind: 'server',
    async info() { return (await fetch('/api/info')).json(); },
    async isochrone(params) {
      const res = await fetch('/api/isochrone?' + new URLSearchParams(params).toString());
      if (!res.ok) throw new Error(await res.text());
      const fc = await res.json();
      return { fc, note: `routed in ${fc.properties.query_ms} ms, drawn in ${fc.properties.total_ms} ms` };
    },
  };
}

function wasmBackend(bundleUrl, workerUrl) {
  const worker = new Worker(workerUrl, { type: 'module' });
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
        $('engine-note').textContent =
          `This page downloaded a ${(m.bytes / 1e6).toFixed(0)} MB bundle (the whole county's schedule and walking network) and routes in your browser. No server is involved.`;
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
    kind: 'wasm',
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

let backend = null;
function getBackend() {
  if (!backend) backend = CFG.bundleUrl ? wasmBackend(CFG.base + CFG.bundleUrl, CFG.base + 'worker.js') : httpBackend();
  return backend;
}

// --------------------------------------------------------------- explore --
function nextWeekday(from, first, last) {
  let d = new Date(Math.max(from, first));
  if (d > last) d = new Date(first);
  for (let i = 0; i < 7; i++) {
    const dow = d.getUTCDay();
    if (dow >= 1 && dow <= 5) break;
    d.setUTCDate(d.getUTCDate() + 1);
  }
  return d.toISOString().slice(0, 10);
}
function isoColor(from) { return ISO_RAMP[Math.min(ISO_RAMP.length - 1, Math.floor(from / BAND))]; }
function buildIsoLegend() {
  const bar = [];
  for (let m = 0; m < MAX; m += BAND) bar.push(`<span style="background:${isoColor(m)}" title="${m}–${m + BAND} min"></span>`);
  $('iso-legend').innerHTML = `<div class="bar">${bar.join('')}</div><div class="ticks"><span>0 min</span><span>near → far</span><span>60 min</span></div>`;
}
function applyCutoff() {
  const m = parseInt($('cutoff').value, 10);
  $('cutoff-out').textContent = m;
  for (const id of ['iso-fill', 'iso-line']) if (state.map.getLayer(id)) state.map.setFilter(id, ['<=', ['get', 'to'], m]);
}

async function exploreInit() {
  if (state.exploreReady) return;
  state.exploreReady = true;
  const map = state.map;
  $('stats').textContent = 'loading…';
  const info = await getBackend().info();
  state.info = info;
  $('feed').textContent = `${info.name} · ${info.stops.toLocaleString()} stops · ${info.walk_nodes.toLocaleString()} walking nodes`;
  const first = new Date(info.first_date), last = new Date(info.last_date);
  const dateEl = $('date');
  dateEl.min = info.first_date; dateEl.max = info.last_date;
  const h = hashParams();
  if (h.has('t')) $('time').value = h.get('t');
  if (h.has('d')) dateEl.value = h.get('d');
  if (h.has('m')) $('cutoff').value = h.get('m');
  if (!dateEl.value) dateEl.value = nextWeekday(new Date(), first, last);
  if (h.has('lat') && h.has('lon')) { state.lat = parseFloat(h.get('lat')); state.lon = parseFloat(h.get('lon')); }
  if (state.lat === null) { state.lon = info.center[0]; state.lat = info.center[1]; }
  buildIsoLegend();

  map.addSource('iso', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  const colorExpr = ['step', ['get', 'from'], ISO_RAMP[0]];
  for (let i = 1; i < ISO_RAMP.length; i++) colorExpr.push(i * BAND, ISO_RAMP[i]);
  map.addLayer({ id: 'iso-fill', type: 'fill', source: 'iso', paint: { 'fill-color': colorExpr, 'fill-opacity': 0.55 } }, state.firstSymbol);
  map.addLayer({ id: 'iso-line', type: 'line', source: 'iso', paint: { 'line-color': '#fcfcfb', 'line-width': 0.6, 'line-opacity': 0.7 } }, state.firstSymbol);
  applyCutoff();

  state.marker = new maplibregl.Marker({ draggable: true, color: '#a3262c' }).setLngLat([state.lon, state.lat]);
  state.marker.on('dragend', () => { const p = state.marker.getLngLat(); state.lon = p.lng; state.lat = p.lat; exploreQuery(); });
  $('cutoff').addEventListener('input', () => { applyCutoff(); writeHash(); });
  $('time').addEventListener('change', exploreQuery);
  $('date').addEventListener('change', exploreQuery);
  exploreShow(true);
  exploreQuery();
}

function exploreShow(on) {
  setVisible('iso-fill', on); setVisible('iso-line', on);
  if (state.marker) { if (on) state.marker.addTo(state.map); else state.marker.remove(); }
}

async function exploreQuery() {
  if (!$('date').value || !$('time').value) return;
  writeHash();
  const params = { lat: state.lat, lon: state.lon, date: $('date').value, time: $('time').value, max: MAX, band: BAND };
  const token = JSON.stringify(params);
  state.pending = token;
  $('stats').textContent = 'routing…';
  $('error').hidden = true;
  try {
    const { fc, note } = await getBackend().isochrone(params);
    if (state.pending !== token) return; // superseded
    state.map.getSource('iso').setData(fc);
    const p = fc.properties || {};
    $('stats').textContent = `${(p.stops_by_transit || 0).toLocaleString()} stops reachable by transit within ${MAX} min · ${note}`;
  } catch (err) {
    $('stats').textContent = '';
    $('error').textContent = err.message;
    $('error').hidden = false;
    state.map.getSource('iso').setData({ type: 'FeatureCollection', features: [] });
  }
}

// -------------------------------------------------------------- findings --
const JOB_BREAKS = [25000, 50000, 100000, 150000, 200000, 300000];
const kFmt = (k) => (k >= 1000 ? (k / 1000) + 'k' : k);
const pctFmt = (v) => Math.round(v * 100) + '%';
const VIEWS = [
  { key: 'jobs_weekday_am', label: 'Weekday 7–9 am', breaks: JOB_BREAKS, ramp: BLUES, fmt: kFmt, cap: 'Jobs reachable within 45 min' },
  { key: 'jobs_weekday_midday', label: 'Weekday noon–2 pm', breaks: JOB_BREAKS, ramp: BLUES, fmt: kFmt, cap: 'Jobs reachable within 45 min' },
  { key: 'jobs_weekday_night', label: 'Weekday 10 pm–midnight', breaks: JOB_BREAKS, ramp: BLUES, fmt: kFmt, cap: 'Jobs reachable within 45 min' },
  { key: 'jobs_weekday_owl', label: 'Weekday midnight–2 am', breaks: JOB_BREAKS, ramp: BLUES, fmt: kFmt, cap: 'Jobs reachable within 45 min' },
  { key: 'jobs_sunday_am', label: 'Sunday 7–9 am', breaks: JOB_BREAKS, ramp: BLUES, fmt: kFmt, cap: 'Jobs reachable within 45 min' },
  { key: 'weekday_night_over_am', label: '10 pm as share of 8 am', breaks: [0.1, 0.25, 0.5, 0.75, 0.9], ramp: ORANGES, fmt: pctFmt, cap: 'Late-night access ÷ morning access' },
  { key: 'weekday_owl_over_am', label: 'After midnight as share of 8 am', breaks: [0.1, 0.25, 0.5, 0.75, 0.9], ramp: ORANGES, fmt: pctFmt, cap: 'After-midnight access ÷ morning access' },
];
const JOB_VIEWS = VIEWS.filter((v) => v.key.startsWith('jobs_'));

function stepExpr(v) {
  const e = ['step', ['coalesce', ['get', v.key], -1], 'rgba(0,0,0,0)', 0, v.ramp[0]];
  v.breaks.forEach((b, i) => e.push(b, v.ramp[i + 1]));
  return e;
}
function findingsLegend(v) {
  const bar = v.ramp.map((c) => `<span style="background:${c}"></span>`).join('');
  const ticks = ['0', ...v.breaks.map(v.fmt)].map((t) => `<span>${t}</span>`).join('');
  $('f-legend').innerHTML = `<div class="bar">${bar}</div><div class="ticks">${ticks}</div><div class="cap">${v.cap}</div>`;
}
function setView(v) {
  state.view = v;
  if (state.map.getLayer('blocks')) state.map.setPaintProperty('blocks', 'fill-color', stepExpr(v));
  findingsLegend(v);
  const r = document.querySelector(`#scenarios input[value="${v.key}"]`);
  if (r) r.checked = true;
  writeHash();
}

async function findingsInit() {
  if (state.findingsLoaded) return;
  state.findingsLoaded = true;
  const map = state.map;
  const F = CFG.findings;
  const radios = $('scenarios');
  VIEWS.forEach((v) => {
    const l = document.createElement('label');
    l.innerHTML = `<input type="radio" name="view" value="${v.key}"><span>${v.label}</span>`;
    radios.appendChild(l);
  });
  radios.addEventListener('change', (e) => setView(VIEWS.find((v) => v.key === e.target.value)));
  const h = hashParams();
  state.view = VIEWS.find((v) => v.key === h.get('view')) || VIEWS[0];

  fetch(CFG.base + F.summary).then((r) => r.json()).then((s) => {
    const am = s.scenarios.weekday_am, night = s.scenarios.weekday_night, owl = s.scenarios.weekday_owl;
    const tiles = [
      [fmtInt(am.median), 'jobs within reach of the median resident, weekday 7–9 am'],
      [fmtInt(night.median), 'same budget, leaving 10 pm–midnight'],
      [owl ? fmtInt(owl.median) : '–', 'leaving midnight–2 am'],
      [pctFmt((s.owl_vs_am || s.night_vs_am).share_losing_half), 'of residents lose more than half their access after midnight'],
    ];
    $('tiles').innerHTML = tiles.map(([b, t]) => `<div class="tile"><b>${b}</b><span>${t}</span></div>`).join('');
  }).catch(() => {});

  map.addSource('blocks', { type: 'geojson', data: CFG.base + F.blocks, promoteId: 'id' });
  map.addLayer({ id: 'blocks', type: 'fill', source: 'blocks',
    paint: { 'fill-color': stepExpr(state.view), 'fill-opacity': ['case', ['boolean', ['feature-state', 'hover'], false], 0.95, 0.72] } }, state.firstSymbol);
  map.addLayer({ id: 'blocks-line', type: 'line', source: 'blocks',
    paint: { 'line-color': '#fcfcfb', 'line-width': ['interpolate', ['linear'], ['zoom'], 10, 0, 13, 0.5], 'line-opacity': 0.6 } }, state.firstSymbol);

  const tip = $('tip');
  map.on('mousemove', 'blocks', (e) => {
    if (state.mode !== 'findings') return;
    const f = e.features[0];
    if (state.hovered !== null && state.hovered !== f.id) map.setFeatureState({ source: 'blocks', id: state.hovered }, { hover: false });
    state.hovered = f.id;
    map.setFeatureState({ source: 'blocks', id: state.hovered }, { hover: true });
    map.getCanvas().style.cursor = 'crosshair';
    const p = f.properties;
    const rows = JOB_VIEWS.map((v) => `<tr class="${v.key === state.view.key ? 'cur' : ''}"><td>${v.label}</td><td>${fmtInt(p[v.key])}</td></tr>`).join('');
    const pct = (v) => (v == null ? '–' : pctFmt(v));
    tip.innerHTML = `<div>Block ${p.id}<br><span style="color:var(--ink-2)">${fmtInt(p.pop)} residents · ${fmtInt(p.jobs_here)} jobs here</span></div>
      <table>${rows}
      <tr class="${state.view.key === 'weekday_night_over_am' ? 'cur' : ''}"><td>10 pm ÷ 8 am</td><td>${pct(p.weekday_night_over_am)}</td></tr>
      <tr class="${state.view.key === 'weekday_owl_over_am' ? 'cur' : ''}"><td>After midnight ÷ 8 am</td><td>${pct(p.weekday_owl_over_am)}</td></tr></table>`;
    tip.style.display = 'block';
    tip.style.left = Math.min(e.point.x + 14, window.innerWidth - tip.offsetWidth - 8) + 'px';
    tip.style.top = Math.min(e.point.y + 14, window.innerHeight - tip.offsetHeight - 8) + 'px';
  });
  map.on('mouseleave', 'blocks', () => {
    if (state.hovered !== null) map.setFeatureState({ source: 'blocks', id: state.hovered }, { hover: false });
    state.hovered = null;
    map.getCanvas().style.cursor = '';
    tip.style.display = 'none';
  });
  setView(state.view);
  findingsShow(true);
}

function findingsShow(on) {
  setVisible('blocks', on); setVisible('blocks-line', on);
  if (!on) $('tip').style.display = 'none';
}

// ----------------------------------------------------------------- modes --
async function setMode(mode) {
  if (mode === 'findings' && !CFG.findings) mode = 'explore';
  if (state.mode === mode) return;
  state.mode = mode;
  for (const b of document.querySelectorAll('#tabs button')) b.setAttribute('aria-selected', b.dataset.mode === mode ? 'true' : 'false');
  $('explore').hidden = mode !== 'explore';
  $('findings').hidden = mode !== 'findings';
  if (mode === 'explore') {
    findingsShow(false);
    if (state.exploreReady) exploreShow(true); else await exploreInit();
  } else {
    exploreShow(false);
    if (state.findingsLoaded) findingsShow(true); else await findingsInit();
  }
  writeHash();
}

function init() {
  const h = hashParams();
  const startMode = h.get('mode') || CFG.mode;
  const map = new maplibregl.Map({
    container: 'map',
    style: 'https://tiles.openfreemap.org/styles/positron',
    center: [-87.95, 43.03],
    zoom: 10.6,
    attributionControl: { compact: true },
  });
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
  state.map = map;
  if (CFG.findings) {
    $('tabs').hidden = false;
    for (const b of document.querySelectorAll('#tabs button')) b.addEventListener('click', () => setMode(b.dataset.mode));
  }
  map.on('click', (e) => {
    if (state.mode !== 'explore' || !state.marker) return;
    if (e.originalEvent.target.closest('#panel')) return;
    state.marker.setLngLat(e.lngLat);
    state.lon = e.lngLat.lng; state.lat = e.lngLat.lat;
    exploreQuery();
  });
  map.on('load', () => {
    state.firstSymbol = firstSymbolLayer(map);
    setMode(startMode);
  });
}

init();
