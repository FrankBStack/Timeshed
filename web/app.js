// Timeshed: one map, two modes.
//   Explore  - drop a pin, scrub the time, the engine draws everything reachable
//   Findings - job access by census block, by departure window (precomputed)
// The engine is reached either through the native server's /api or, when the
// page names a bundle, through the same engine compiled to wasm in a worker.
// Findings only appear when the page says where the block data is.

const CFG = Object.assign({ base: '', mode: 'explore', city: null, cities: null, pkgUrl: 'pkg/timeshed.js', workerUrl: 'worker.js' }, window.TIMESHED || {});
const $ = (id) => document.getElementById(id);
// One or more cities. The dev server (no config) is a single unnamed city on
// the HTTP backend with no Findings; the published site lists each city's
// bundle and, when the analysis has been run for it, its Findings data.
const CITIES = CFG.cities || { default: { name: '', bundleUrl: null, bundleBytes: 0, findings: null } };
const cityCfg = () => CITIES[state.city];

// ----------------------------------------------------------------- theme --
// Light or dark follows the OS unless the visitor picks one with the button;
// the choice sticks in localStorage. The basemap and the color ramps follow.
let theme = (() => {
  try { const t = localStorage.getItem('timeshed-theme'); if (t === 'light' || t === 'dark') return t; } catch (e) { /* private mode */ }
  return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
})();
const isDark = () => theme === 'dark';
const mapStyle = () => (isDark() ? 'https://tiles.openfreemap.org/styles/dark' : 'https://tiles.openfreemap.org/styles/positron');
const outline = () => (isDark() ? '#181817' : '#f3f2ee');
function applyThemeClass() {
  document.documentElement.dataset.theme = theme;
  document.documentElement.classList.toggle('system-dark', isDark());
}
applyThemeClass();

// ---------------------------------------------------------------- shared --
const BAND = 5;   // minutes per isochrone band
const MAX = 60;   // outermost band requested
// On a light map "near" and "more" are the dark end of a ramp; on a dark map
// they have to be the bright end, or the strongest values disappear into it.
const LIGHT_ISO = ['#0d366b', '#104281', '#184f95', '#1c5cab', '#256abf', '#2a78d6',
                   '#3987e5', '#5598e7', '#6da7ec', '#86b6ef', '#9ec5f4', '#b7d3f6'];
const DARK_ISO = ['#e3eefb', '#cde2fb', '#b7d3f6', '#9ec5f4', '#86b6ef', '#6da7ec',
                  '#5598e7', '#3987e5', '#2a78d6', '#256abf', '#1c5cab', '#184f95'];
const isoRamp = () => (isDark() ? DARK_ISO : LIGHT_ISO);
const LIGHT_BLUES = ['#cde2fb', '#9ec5f4', '#6da7ec', '#3987e5', '#256abf', '#184f95', '#0d366b'];
const LIGHT_ORANGES = ['#fbe4d8', '#f7bda2', '#f29568', '#eb6834', '#c04e21', '#8c3815'];
const blues = () => (isDark() ? [...LIGHT_BLUES].reverse() : LIGHT_BLUES);
const oranges = () => (isDark() ? [...LIGHT_ORANGES].reverse() : LIGHT_ORANGES);

const state = {
  mode: null, map: null, firstSymbol: undefined, city: null,
  // explore
  lat: null, lon: null, cityOfPin: null, lastFlownCity: null, marker: null, info: null, exploreReady: false,
  inFlight: false, queued: false, consented: false,
  // findings
  window: null, share: false, findingsLoaded: false, findingsWired: false, findingsCity: null, hovered: null,
};

const hashParams = () => new URLSearchParams(location.hash.slice(1));
function writeHash() {
  const h = new URLSearchParams();
  h.set('mode', state.mode);
  if (state.mode === 'explore' && state.lat !== null) {
    h.set('lat', state.lat.toFixed(5)); h.set('lon', state.lon.toFixed(5));
    h.set('day', checked('day')); h.set('t', $('time').value); h.set('m', checked('cutoff'));
  }
  if (state.mode === 'findings' && state.window) { h.set('win', state.window); if (state.share) h.set('share', '1'); }
  if (CFG.cities) h.set('city', state.city);
  history.replaceState(null, '', '#' + h.toString());
}
const checked = (name) => (document.querySelector(`input[name="${name}"]:checked`) || {}).value;
function check(name, value) { const el = document.querySelector(`input[name="${name}"][value="${value}"]`); if (el) el.checked = true; }
const fmtInt = (n) => Math.round(n).toLocaleString();
const pctFmt = (v) => Math.round(v * 100) + '%';
function setVisible(id, on) { if (state.map.getLayer(id)) state.map.setLayoutProperty(id, 'visibility', on ? 'visible' : 'none'); }
function firstSymbolLayer(map) { const l = map.getStyle().layers.find((x) => x.type === 'symbol'); return l ? l.id : undefined; }

// --------------------------------------------------------------- backend --
function httpBackend() {
  return {
    async info() { return (await fetch('/api/info')).json(); },
    async isochrone(params) {
      const res = await fetch('/api/isochrone?' + new URLSearchParams(params).toString());
      if (!res.ok) throw new Error(await res.text());
      const fc = await res.json();
      return { fc, note: `${fc.properties.query_ms} ms on the server` };
    },
    async itinerary(params, to) {
      const res = await fetch('/api/itinerary?' + new URLSearchParams({ ...params, to_lat: to.lat, to_lon: to.lon }).toString());
      if (res.status === 404) return null;
      if (!res.ok) throw new Error(await res.text());
      return res.json();
    },
  };
}

function wasmBackend(bundleUrl, pkgUrl, workerUrl) {
  // Absolute URLs: the worker's own location is not the page's, and a
  // dynamic import() of a bare "pkg/..." string is a module name, not a path.
  const abs = (u) => new URL(u, location.href).href;
  bundleUrl = abs(bundleUrl); pkgUrl = abs(pkgUrl);
  const worker = new Worker(abs(workerUrl), { type: 'module' });
  const waiting = new Map();
  let nextId = 1;
  const bar = $('progress');
  const ready = new Promise((resolve, reject) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'progress') {
        bar.hidden = false;
        const pct = m.total ? Math.min(100, Math.round(100 * m.loaded / m.total)) : 50;
        bar.firstElementChild.style.width = pct + '%';
        $('stats').textContent = m.total ? `downloading the network… ${pct}%` : `downloading the network… ${(m.loaded / 1e6).toFixed(1)} MB`;
      } else if (m.type === 'ready') {
        bar.firstElementChild.style.width = '100%';
        setTimeout(() => { bar.hidden = true; }, 400);
        $('engine-note').textContent =
          `This page downloaded a ${(m.bytes / 1e6).toFixed(0)} MB bundle, the whole county's schedule and walking network, and routes in your browser. No server is involved.`;
        resolve(m.info);
      } else if (m.type === 'error' && m.id === 'load') {
        bar.hidden = true;
        reject(new Error('The routing engine failed to load: ' + m.message));
      } else if (m.type === 'isochrone' || m.type === 'itinerary' || m.type === 'error') {
        const w = waiting.get(m.id);
        waiting.delete(m.id);
        if (!w) return;
        if (m.type === 'error') w.reject(new Error(m.message));
        else if (m.type === 'itinerary') w.resolve(JSON.parse(m.json));
        else w.resolve({ fc: JSON.parse(m.geojson), note: `${m.ms.toFixed(0)} ms in your browser` });
      }
    };
    worker.onerror = (e) => reject(new Error(e.message));
  });
  worker.postMessage({ type: 'load', bundleUrl, pkgUrl });
  return {
    info() { return ready; },
    async isochrone(params) {
      await ready;
      const id = nextId++;
      return new Promise((resolve, reject) => { waiting.set(id, { resolve, reject }); worker.postMessage({ type: 'isochrone', id, params }); });
    },
    // the worker reconstructs against its most recent isochrone run, which is
    // always the one on screen because it answers messages in order
    async itinerary(_params, to) {
      await ready;
      const id = nextId++;
      return new Promise((resolve, reject) => {
        waiting.set(id, { resolve, reject: (e) => (/not reachable/.test(e.message) ? resolve(null) : reject(e)) });
        worker.postMessage({ type: 'itinerary', id, to_lat: to.lat, to_lon: to.lon });
      });
    },
  };
}

const backends = {};
function getBackend() {
  const key = state.city;
  if (!backends[key]) {
    const c = CITIES[key];
    backends[key] = c.bundleUrl ? wasmBackend(CFG.base + c.bundleUrl, CFG.base + CFG.pkgUrl, CFG.base + CFG.workerUrl) : httpBackend();
  }
  return backends[key];
}

// --------------------------------------------------------------- explore --
function timeLabel(mins) {
  const h24 = Math.floor(mins / 60), m = mins % 60;
  const h = h24 % 24, ap = h < 12 ? 'am' : 'pm', h12 = h % 12 === 0 ? 12 : h % 12;
  const s = `${h12}:${String(m).padStart(2, '0')} ${ap}`;
  return h24 >= 24 ? `${s}, next day` : s;
}
function timeParam(mins) { return `${Math.floor(mins / 60)}:${String(mins % 60).padStart(2, '0')}`; }
function buildTicks() {
  const el = $('time-ticks'), lo = +$('time').min, hi = +$('time').max;
  el.innerHTML = '';
  for (const h of [6, 9, 12, 15, 18, 21, 24]) {
    const s = document.createElement('span');
    s.style.left = (100 * (h * 60 - lo) / (hi - lo)) + '%';
    s.textContent = h === 12 ? 'noon' : h === 24 ? 'midnight' : h < 12 ? `${h} am` : `${h - 12} pm`;
    el.appendChild(s);
  }
}
/// The next date of the wanted kind inside the feed's range.
function pickDate(kind, first, last) {
  const today = new Date(); today.setUTCHours(0, 0, 0, 0);
  let d = new Date(Math.max(today, first));
  if (d > last) d = new Date(first);
  for (let i = 0; i < 8; i++) {
    const dow = d.getUTCDay();
    const ok = kind === 'saturday' ? dow === 6 : kind === 'sunday' ? dow === 0 : dow >= 1 && dow <= 5;
    if (ok) break;
    d.setUTCDate(d.getUTCDate() + 1);
  }
  if (d > last) d = new Date(first);
  return d.toISOString().slice(0, 10);
}
function prettyDate(iso) {
  return new Date(iso + 'T12:00:00Z').toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric', timeZone: 'UTC' });
}
function isoColor(from) { const r = isoRamp(); return r[Math.min(r.length - 1, Math.floor(from / BAND))]; }
function buildIsoLegend() {
  const bar = [];
  for (let m = 0; m < MAX; m += BAND) bar.push(`<span style="background:${isoColor(m)}" title="${m}–${m + BAND} min"></span>`);
  $('iso-legend').innerHTML = `<div class="bar">${bar.join('')}</div><div class="ticks-row"><span>here</span><span>travel time</span><span>60 min</span></div>`;
}
let lastIso = { type: 'FeatureCollection', features: [] };
let lastTrip = { type: 'FeatureCollection', features: [] };
function ensureTripLayers() {
  const map = state.map;
  if (!map.getSource('trip')) map.addSource('trip', { type: 'geojson', data: lastTrip });
  if (!map.getLayer('trip-walk')) {
    map.addLayer({ id: 'trip-casing', type: 'line', source: 'trip', filter: ['==', ['get', 'kind'], 'ride'],
      layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': outline(), 'line-width': 9, 'line-opacity': 0.9 } });
    map.addLayer({ id: 'trip-ride', type: 'line', source: 'trip', filter: ['==', ['get', 'kind'], 'ride'],
      layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': ['get', 'color'], 'line-width': 5 } });
    map.addLayer({ id: 'trip-walk', type: 'line', source: 'trip', filter: ['==', ['get', 'kind'], 'walk'],
      layout: { 'line-cap': 'round' }, paint: { 'line-color': ['get', 'color'], 'line-width': 3.5, 'line-dasharray': [0.2, 1.6] } });
    map.addLayer({ id: 'trip-end', type: 'circle', source: 'trip', filter: ['==', ['get', 'kind'], 'end'],
      paint: { 'circle-radius': 7, 'circle-color': '#d9452b', 'circle-stroke-color': outline(), 'circle-stroke-width': 2.5 } });
  } else {
    map.setPaintProperty('trip-casing', 'line-color', outline());
    map.setPaintProperty('trip-end', 'circle-stroke-color', outline());
  }
}
function clearTrip() {
  lastTrip = { type: 'FeatureCollection', features: [] };
  if (state.map.getSource('trip')) state.map.getSource('trip').setData(lastTrip);
  $('trip').hidden = true;
  $('trip-hint').hidden = false;
}
function clock(secs) {
  const h24 = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60);
  const h = h24 % 24, ap = h < 12 ? 'am' : 'pm', h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${String(m).padStart(2, '0')} ${ap}${h24 >= 24 ? ' +1' : ''}`;
}
function showTrip(it, to) {
  const ink = getComputedStyle(document.documentElement).getPropertyValue('--ink-2').trim() || '#5c5b55';
  const feats = it.legs.map((leg) => ({
    type: 'Feature',
    properties: { kind: leg.kind, color: leg.kind === 'ride' ? (leg.color ? '#' + leg.color.replace('#', '') : getComputedStyle(document.documentElement).getPropertyValue('--accent').trim()) : ink },
    geometry: { type: 'LineString', coordinates: leg.path.length > 1 ? leg.path : [leg.path[0], leg.path[0]] },
  }));
  feats.push({ type: 'Feature', properties: { kind: 'end' }, geometry: { type: 'Point', coordinates: [to.lon, to.lat] } });
  lastTrip = { type: 'FeatureCollection', features: feats };
  state.map.getSource('trip').setData(lastTrip);
  const mins = (s) => Math.max(1, Math.round(s / 60));
  $('trip-title').textContent = `${mins(it.total_secs)} min door to door · arrive ${clock(it.arrive)}`;
  const items = it.legs.map((leg) => {
    if (leg.kind === 'walk') {
      return `<li><span class="dot"></span><div>Walk <b>${mins(leg.secs)} min</b> <span class="when">· ${Math.round(leg.meters)} m</span></div></li>`;
    }
    const color = leg.color ? '#' + leg.color.replace('#', '') : 'var(--accent)';
    return `<li><span class="dot ride" style="background:${color}"></span><div><b>${leg.route}</b> ${leg.long_name && leg.long_name !== leg.route ? '· ' + leg.long_name : ''}<br>
      <span class="when">${clock(leg.depart)} from ${leg.from}<br>${clock(leg.arrive)} at ${leg.to} · ${leg.stops} stop${leg.stops === 1 ? '' : 's'}</span></div></li>`;
  });
  $('trip-legs').innerHTML = items.join('');
  $('trip').hidden = false;
  $('trip-hint').hidden = true;
}
async function tripTo(lngLat) {
  if (!state.exploreReady || !state.info) return;
  const info = state.info;
  const date = pickDate(checked('day'), new Date(info.first_date), new Date(info.last_date));
  const params = { lat: state.lat, lon: state.lon, date, time: timeParam(+$('time').value), max: MAX, band: BAND };
  try {
    const it = await getBackend().itinerary(params, { lat: lngLat.lat, lon: lngLat.lng });
    if (!it) {
      lastTrip = { type: 'FeatureCollection', features: [{ type: 'Feature', properties: { kind: 'end' }, geometry: { type: 'Point', coordinates: [lngLat.lng, lngLat.lat] } }] };
      state.map.getSource('trip').setData(lastTrip);
      $('trip-title').textContent = `Not reachable within ${MAX} min from the pin`;
      $('trip-legs').innerHTML = '';
      $('trip').hidden = false;
      $('trip-hint').hidden = true;
      return;
    }
    showTrip(it, { lat: lngLat.lat, lon: lngLat.lng });
  } catch (err) {
    $('error').textContent = err.message;
    $('error').hidden = false;
  }
}
function ensureExploreLayers() {
  const map = state.map, ramp = isoRamp();
  if (!map.getSource('iso')) map.addSource('iso', { type: 'geojson', data: lastIso });
  const colorExpr = ['step', ['get', 'from'], ramp[0]];
  for (let i = 1; i < ramp.length; i++) colorExpr.push(i * BAND, ramp[i]);
  if (!map.getLayer('iso-fill')) {
    map.addLayer({ id: 'iso-fill', type: 'fill', source: 'iso',
      paint: { 'fill-color': colorExpr, 'fill-opacity': 0.58, 'fill-opacity-transition': { duration: 220 } } }, state.firstSymbol);
    map.addLayer({ id: 'iso-line', type: 'line', source: 'iso', paint: { 'line-color': outline(), 'line-width': 0.6, 'line-opacity': 0.7 } }, state.firstSymbol);
  } else {
    map.setPaintProperty('iso-fill', 'fill-color', colorExpr);
    map.setPaintProperty('iso-line', 'line-color', outline());
  }
  applyCutoff();
  buildIsoLegend();
  ensureTripLayers();
}
function applyCutoff() {
  const m = parseInt(checked('cutoff'), 10);
  for (const id of ['iso-fill', 'iso-line']) if (state.map.getLayer(id)) state.map.setFilter(id, ['<=', ['get', 'to'], m]);
}

// People who have asked their browser to save data, or are on a 2G-class
// connection, get a button instead of an automatic download.
function connectionWantsToAsk() {
  const c = navigator.connection;
  return !!(c && (c.saveData || /(^|-)2g$/.test(c.effectiveType || '')));
}
function showLoadGate() {
  const mb = cityCfg().bundleBytes ? ` (${(cityCfg().bundleBytes / 1e6).toFixed(0)} MB)` : '';
  $('stats').innerHTML = `<button class="load" id="load">Load the routing engine${mb}</button>
    <span class="muted">You're on a data-saving or slow connection, so the download waits for you.</span>`;
  $('load').addEventListener('click', () => { state.consented = true; exploreInit(); });
}

async function exploreInit() {
  if (state.exploreReady) return;
  if (cityCfg().bundleUrl && !state.consented && connectionWantsToAsk()) { showLoadGate(); return; }
  state.exploreReady = true;
  const h = hashParams();
  if (h.has('t')) $('time').value = h.get('t');
  if (h.has('day')) check('day', h.get('day'));
  if (h.has('m')) check('cutoff', h.get('m'));
  if (h.has('lat') && h.has('lon')) { state.lat = parseFloat(h.get('lat')); state.lon = parseFloat(h.get('lon')); }
  $('time-out').textContent = timeLabel(+$('time').value);
  buildTicks();
  ensureExploreLayers();
  state.marker = new maplibregl.Marker({ draggable: true, color: '#d9452b' });
  state.marker.on('dragend', () => { const p = state.marker.getLngLat(); state.lon = p.lng; state.lat = p.lat; exploreQuery(); });
  $('time').addEventListener('input', () => { $('time-out').textContent = timeLabel(+$('time').value); exploreQuery(); });
  $('day').addEventListener('change', exploreQuery);
  $('cutoff').addEventListener('change', () => { applyCutoff(); writeHash(); });
  await exploreLoadCity();
}

// Load (or switch to) the current city's engine: its bundle, dates, centre.
async function exploreLoadCity() {
  const map = state.map;
  clearTrip();
  $('stats').textContent = 'loading…';
  $('error').hidden = true;
  let info;
  try {
    info = await getBackend().info();
  } catch (err) {
    state.exploreReady = false;
    $('stats').textContent = '';
    $('error').textContent = err.message;
    $('error').hidden = false;
    return;
  }
  state.info = info;
  $('feed').textContent = `${info.name} · ${info.stops.toLocaleString()} stops · ${info.walk_nodes.toLocaleString()} walking nodes`;
  if (state.lat === null || state.cityOfPin !== state.city) { state.lon = info.center[0]; state.lat = info.center[1]; }
  state.cityOfPin = state.city;
  state.marker.setLngLat([state.lon, state.lat]);
  exploreShow(state.mode === 'explore');
  if (state.mode === 'explore' && state.cityOfPin !== state.lastFlownCity) {
    map.flyTo({ center: cityCfg().center || [state.lon, state.lat], zoom: cityCfg().zoom || 10.6, duration: 900 });
    state.lastFlownCity = state.city;
  }
  exploreQuery();
}

function exploreShow(on) {
  for (const id of ['iso-fill', 'iso-line', 'trip-casing', 'trip-ride', 'trip-walk', 'trip-end']) setVisible(id, on);
  if (state.marker) { if (on) state.marker.addTo(state.map); else state.marker.remove(); }
}

// One query in flight at a time; a change while routing is answered once
// the current one lands, so scrubbing never builds a backlog.
async function exploreQuery() {
  if (state.inFlight) { state.queued = true; return; }
  state.inFlight = true;
  writeHash();
  const info = state.info;
  const date = pickDate(checked('day'), new Date(info.first_date), new Date(info.last_date));
  const params = { lat: state.lat, lon: state.lon, date, time: timeParam(+$('time').value), max: MAX, band: BAND };
  $('error').hidden = true;
  if (state.map.getLayer('iso-fill')) state.map.setPaintProperty('iso-fill', 'fill-opacity', 0.35);
  clearTrip();
  try {
    const { fc, note } = await getBackend().isochrone(params);
    lastIso = fc;
    state.map.getSource('iso').setData(fc);
    state.map.setPaintProperty('iso-fill', 'fill-opacity', 0.58);
    const p = fc.properties || {};
    $('stats').textContent = `${(p.stops_by_transit || 0).toLocaleString()} stops reachable within ${MAX} min · ${note}`;
    $('date-note').textContent = `Routing on ${prettyDate(date)}, the next ${checked('day')} in the feed's ${info.first_date} to ${info.last_date} range.`;
  } catch (err) {
    $('stats').textContent = '';
    $('error').textContent = err.message;
    $('error').hidden = false;
    state.map.getSource('iso').setData({ type: 'FeatureCollection', features: [] });
  } finally {
    state.inFlight = false;
    if (state.queued) { state.queued = false; exploreQuery(); }
  }
}

// -------------------------------------------------------------- findings --
const JOB_BREAKS = [25000, 50000, 100000, 150000, 200000, 300000];
const kFmt = (k) => (k >= 1000 ? (k / 1000) + 'k' : k);
const WINDOWS = [
  { key: 'weekday_am', label: '7–9 am' },
  { key: 'weekday_midday', label: 'Noon–2 pm' },
  { key: 'weekday_night', label: '10 pm–12', ratio: 'weekday_night_over_am', ratioLabel: '10 pm–midnight as a share of 7–9 am' },
  { key: 'weekday_owl', label: '12–2 am', ratio: 'weekday_owl_over_am', ratioLabel: 'After midnight as a share of 7–9 am' },
  { key: 'sunday_am', label: 'Sun 7–9 am' },
];
const WINDOW_LABELS = { weekday_am: 'Weekday 7–9 am', weekday_midday: 'Weekday noon–2 pm', weekday_night: 'Weekday 10 pm–midnight', weekday_owl: 'Weekday midnight–2 am', sunday_am: 'Sunday 7–9 am' };

function currentView() {
  const w = WINDOWS.find((x) => x.key === state.window) || WINDOWS[0];
  if (state.share && w.ratio) {
    return { key: w.ratio, breaks: [0.1, 0.25, 0.5, 0.75, 0.9], ramp: oranges(), fmt: pctFmt, cap: w.ratioLabel };
  }
  return { key: 'jobs_' + w.key, breaks: JOB_BREAKS, ramp: blues(), fmt: kFmt, cap: `Jobs reachable within 45 min, ${WINDOW_LABELS[w.key].toLowerCase()}` };
}
function stepExpr(v) {
  const e = ['step', ['coalesce', ['get', v.key], -1], 'rgba(0,0,0,0)', 0, v.ramp[0]];
  v.breaks.forEach((b, i) => e.push(b, v.ramp[i + 1]));
  return e;
}
function findingsRender() {
  const v = currentView();
  if (state.map.getLayer('blocks')) state.map.setPaintProperty('blocks', 'fill-color', stepExpr(v));
  const bar = v.ramp.map((c) => `<span style="background:${c}"></span>`).join('');
  const ticks = ['0', ...v.breaks.map(v.fmt)].map((t) => `<span>${t}</span>`).join('');
  $('f-legend').innerHTML = `<div class="bar">${bar}</div><div class="ticks-row">${ticks}</div><div class="cap">${v.cap}</div>`;
  const w = WINDOWS.find((x) => x.key === state.window);
  $('share-wrap').classList.toggle('disabled', !w || !w.ratio);
  writeHash();
}

async function findingsInit() {
  if (state.findingsLoaded) return;
  state.findingsLoaded = true;
  const seg = $('windows');
  for (const w of WINDOWS) {
    const l = document.createElement('label');
    l.innerHTML = `<input type="radio" name="win" value="${w.key}"><span>${w.label}</span>`;
    seg.appendChild(l);
  }
  const h = hashParams();
  state.window = WINDOWS.some((w) => w.key === h.get('win')) ? h.get('win') : 'weekday_am';
  state.share = h.get('share') === '1';
  check('win', state.window);
  $('share').checked = state.share;
  seg.addEventListener('change', () => { state.window = checked('win'); findingsRender(); });
  $('share').addEventListener('change', () => { state.share = $('share').checked; findingsRender(); });
  await findingsLoadCity();
}

// Load (or switch to) the current city's block data and headline numbers.
async function findingsLoadCity() {
  const map = state.map;
  const F = cityCfg().findings;
  if (!F) return;
  $('f-title').textContent = F.title;
  $('f-lede').textContent = `Jobs reachable within 45 minutes, walking plus transit, from every inhabited census block in ${F.area}.`;
  $('tiles').innerHTML = '';
  if (state.findingsCity !== state.city) {
    for (const id of ['blocks', 'blocks-line']) if (map.getLayer(id)) map.removeLayer(id);
    if (map.getSource('blocks')) map.removeSource('blocks');
    const moved = state.findingsCity !== null;
    state.findingsCity = state.city;
    if (moved && state.mode === 'findings' && cityCfg().center) map.flyTo({ center: cityCfg().center, zoom: cityCfg().zoom || 10.6, duration: 900 });
  }
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

  ensureFindingsLayers();
  if (!state.findingsWired) {
    state.findingsWired = true;
    wireFindingsHover();
  }
  findingsRender();
  findingsShow(state.mode === 'findings');
}

function wireFindingsHover() {
  const map = state.map;
  const tip = $('tip');
  map.on('mousemove', 'blocks', (e) => {
    if (state.mode !== 'findings') return;
    const f = e.features[0];
    if (state.hovered !== null && state.hovered !== f.id) map.setFeatureState({ source: 'blocks', id: state.hovered }, { hover: false });
    state.hovered = f.id;
    map.setFeatureState({ source: 'blocks', id: state.hovered }, { hover: true });
    map.getCanvas().style.cursor = 'crosshair';
    const p = f.properties, cur = currentView().key;
    const rows = WINDOWS.map((w) => `<tr class="${'jobs_' + w.key === cur ? 'cur' : ''}"><td>${WINDOW_LABELS[w.key]}</td><td>${fmtInt(p['jobs_' + w.key])}</td></tr>`).join('');
    const pct = (v) => (v == null ? '–' : pctFmt(v));
    tip.innerHTML = `<div><b>Block ${p.id}</b><br><span style="color:var(--ink-2)">${fmtInt(p.pop)} residents · ${fmtInt(p.jobs_here)} jobs here</span></div>
      <table>${rows}
      <tr class="${cur === 'weekday_night_over_am' ? 'cur' : ''}"><td>10 pm ÷ 8 am</td><td>${pct(p.weekday_night_over_am)}</td></tr>
      <tr class="${cur === 'weekday_owl_over_am' ? 'cur' : ''}"><td>After midnight ÷ 8 am</td><td>${pct(p.weekday_owl_over_am)}</td></tr></table>`;
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
}

function ensureFindingsLayers() {
  const map = state.map;
  if (!cityCfg().findings) return;
  if (!map.getSource('blocks')) map.addSource('blocks', { type: 'geojson', data: CFG.base + cityCfg().findings.blocks, promoteId: 'id' });
  if (!map.getLayer('blocks')) {
    map.addLayer({ id: 'blocks', type: 'fill', source: 'blocks',
      paint: { 'fill-color': stepExpr(currentView()), 'fill-opacity': ['case', ['boolean', ['feature-state', 'hover'], false], 0.95, 0.74] } }, state.firstSymbol);
    map.addLayer({ id: 'blocks-line', type: 'line', source: 'blocks',
      paint: { 'line-color': outline(), 'line-width': ['interpolate', ['linear'], ['zoom'], 10, 0, 13, 0.5], 'line-opacity': 0.6 } }, state.firstSymbol);
  } else {
    map.setPaintProperty('blocks-line', 'line-color', outline());
  }
}

function findingsShow(on) {
  setVisible('blocks', on); setVisible('blocks-line', on);
  if (!on) $('tip').style.display = 'none';
}

// ----------------------------------------------------------------- modes --
async function setMode(mode) {
  if (mode === 'findings' && !cityCfg().findings) mode = 'explore';
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

async function setCity(key) {
  if (!CITIES[key] || key === state.city) return;
  state.city = key;
  $('city').value = key;
  $('tabs').hidden = !cityCfg().findings;
  if (state.mode === 'findings' && !cityCfg().findings) { await setMode('explore'); }
  if (state.exploreReady) await exploreLoadCity();
  if (state.findingsLoaded) await findingsLoadCity();
  writeHash();
}

function init() {
  const h = hashParams();
  const startMode = h.get('mode') || CFG.mode;
  state.city = CITIES[h.get('city')] ? h.get('city') : (CFG.city && CITIES[CFG.city] ? CFG.city : Object.keys(CITIES)[0]);
  const sel = $('city');
  for (const [key, c] of Object.entries(CITIES)) {
    const o = document.createElement('option');
    o.value = key; o.textContent = c.name || 'Local';
    sel.appendChild(o);
  }
  sel.value = state.city;
  sel.hidden = Object.keys(CITIES).length < 2;
  sel.addEventListener('change', () => setCity(sel.value));
  const start = cityCfg();
  const map = new maplibregl.Map({ container: 'map', style: mapStyle(), center: start.center || [-87.95, 43.03], zoom: start.zoom || 10.6, attributionControl: { compact: true } });
  window.timeshed = state; // test hook: the smoke test reads the map position through it
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
  state.map = map;
  $('theme').addEventListener('click', () => {
    theme = isDark() ? 'light' : 'dark';
    try { localStorage.setItem('timeshed-theme', theme); } catch (e) { /* ignore */ }
    applyThemeClass();
    // diff: false forces a full reload, so style.load fires and our layers
    // are rebuilt; a diff between the two basemaps would silently drop them.
    map.setStyle(mapStyle(), { diff: false });
    map.once('style.load', () => {
      state.firstSymbol = firstSymbolLayer(map);
      if (state.exploreReady) ensureExploreLayers();
      if (state.findingsLoaded) { ensureFindingsLayers(); findingsRender(); }
      exploreShow(state.mode === 'explore' && state.exploreReady);
      findingsShow(state.mode === 'findings' && state.findingsLoaded);
    });
  });
  $('tabs').hidden = !cityCfg().findings;
  for (const b of document.querySelectorAll('#tabs button')) b.addEventListener('click', () => setMode(b.dataset.mode));
  // drag the pin to move the origin; click anywhere to ask how to get there
  map.on('click', (e) => {
    if (state.mode !== 'explore' || !state.marker) return;
    if (e.originalEvent.target.closest('#panel, #bar')) return;
    tripTo(e.lngLat);
  });
  $('trip-close').addEventListener('click', clearTrip);
  map.on('load', () => { state.firstSymbol = firstSymbolLayer(map); setMode(startMode); });
}

init();
