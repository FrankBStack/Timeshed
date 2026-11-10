// Headless-browser smoke test for the built site in docs/.
//   node scripts/smoke.mjs [docs-dir]
// Serves the directory, opens the page in a real browser, and fails if the
// engine does not load and route, if the Findings tab or the theme toggle
// breaks, or if anything logs an error. Runs in CI before every deploy.
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import puppeteer from 'puppeteer-core';

const dir = resolve(process.argv[2] || 'docs');
const port = 8765 + Math.floor(Math.random() * 1000);
const base = `http://127.0.0.1:${port}`;
const chrome = process.env.CHROME_PATH
  || ['/usr/bin/google-chrome', '/usr/bin/chromium-browser', '/usr/bin/chromium',
      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      '/Applications/Brave Browser.app/Contents/MacOS/Brave Browser'].find((p) => existsSync(p));
if (!chrome) { console.log('no browser found; set CHROME_PATH'); process.exit(2); }

const server = spawn('python3', ['-m', 'http.server', String(port), '--directory', dir], { stdio: 'ignore' });
const failures = [];
const fail = (m) => { failures.push(m); console.log('FAIL', m); };
const ok = (m) => console.log('ok  ', m);

try {
  for (let i = 0; i < 50; i++) { try { await fetch(base + '/'); break; } catch { await new Promise((r) => setTimeout(r, 200)); } }
  const browser = await puppeteer.launch({
    executablePath: chrome, headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--no-first-run', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--window-size=1200,820'],
  });
  const page = await browser.newPage();
  await page.setViewport({ width: 1200, height: 820 });
  page.on('console', (m) => { if (m.type() === 'error') fail('console error: ' + m.text().slice(0, 200)); });
  page.on('pageerror', (e) => fail('page error: ' + e.message));
  page.on('requestfailed', (r) => { if (!/tiles\.openfreemap|fonts\./.test(r.url())) fail('request failed: ' + r.url()); });
  const until = async (fn, ms) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { if (await page.evaluate(fn)) return true; await new Promise((r) => setTimeout(r, 250)); } return false; };
  const stats = () => page.$eval('#stats', (el) => el.textContent);

  await page.goto(base + '/#lat=43.0389&lon=-87.91&t=480', { waitUntil: 'networkidle0', timeout: 120000 });
  if (await until(() => /stops reachable/.test(document.querySelector('#stats').textContent), 120000)) ok('explore routed: ' + await stats());
  else fail('explore did not route: ' + await stats());
  const pinAt = () => page.evaluate(() => { const p = window.timeshed.marker.getLngLat(); return [p.lng, p.lat]; });
  const near = (p, lon, lat) => Math.abs(p[0] - lon) < 5e-4 && Math.abs(p[1] - lat) < 5e-4;
  const linked = await pinAt();
  if (near(linked, -87.91, 43.0389)) ok('the pin is where the link put it');
  else fail(`the link's pin was ignored: marker at ${linked.map((v) => v.toFixed(4))}`);

  // click a point a little north of the pin: a journey card with at least one leg
  await page.mouse.click(700, 300);
  if (await until(() => !document.querySelector('#trip').hidden && document.querySelectorAll('#trip-legs li').length >= 1, 20000)) {
    ok('journey card: ' + await page.$eval('#trip-title', (el) => el.textContent) + ' / ' + await page.$$eval('#trip-legs li', (els) => els.length) + ' legs');
  } else fail('no journey card after clicking the map: ' + await page.$eval('#trip-title', (el) => el.textContent));
  await page.click('#trip-close');
  if (await until(() => document.querySelector('#trip').hidden, 3000)) ok('journey card closes'); else fail('journey card did not close');

  // a pin dropped far off the network comes back, and what was on the map stays
  const toastText = () => page.$eval('#toast', (el) => (el.classList.contains('show') ? el.textContent : ''));
  await page.evaluate(() => { const m = window.timeshed.marker, p = m.getLngLat(); m.fire('dragstart'); m.setLngLat([p.lng + 3, p.lat]); m.fire('dragend'); });
  await new Promise((r) => setTimeout(r, 1200));
  const back = await pinAt();
  const kept = await page.evaluate(() => window.timeshed.map.getSource('iso').serialize().data.features.length);
  if (near(back, -87.91, 43.0389) && /went back/.test(await toastText()) && kept > 0 && /stops reachable/.test(await stats()) && /lat=43\.0389/.test(page.url())) {
    ok('a pin off the network goes back and says so');
  } else fail(`off-network pin: marker at ${back.map((v) => v.toFixed(4))}, toast "${await toastText()}", ${kept} bands, stats "${await stats()}", url ${page.url()}`);
  // the engine still answers for the run on screen
  await page.mouse.click(700, 300);
  if (await until(() => !document.querySelector('#trip').hidden && document.querySelectorAll('#trip-legs li').length >= 1, 20000)) ok('journeys still work after it');
  else fail('no journey after an off-network pin');
  // a destination out in the lake is not a journey and leaves the last one alone
  await page.mouse.click(1150, 400);
  if (await until(() => /nowhere to route/.test(document.querySelector('#toast.show')?.textContent || ''), 5000)
      && await page.$$eval('#trip-legs li', (els) => els.length) >= 1) ok('a destination off the network says so');
  else fail(`off-network destination: toast "${await toastText()}", card "${await page.$eval('#trip-title', (el) => el.textContent)}"`);
  await page.click('#trip-close');

  await page.click('#tabs button[data-mode="findings"]');
  if (await until(() => document.querySelectorAll('#tiles .tile').length === 4, 30000)) ok('findings tiles rendered'); else fail('findings tiles missing');
  if (await until(() => !document.querySelector('#findings').hidden && document.querySelector('#explore').hidden, 5000)) ok('findings panel shown'); else fail('panel state wrong after switching to findings');

  await page.click('#theme');
  await new Promise((r) => setTimeout(r, 5000));
  const theme = await page.evaluate(() => document.documentElement.dataset.theme);
  if (theme === 'dark' || theme === 'light') ok('theme toggled to ' + theme); else fail('theme toggle did nothing');

  await page.click('#tabs button[data-mode="explore"]');
  if (await until(() => document.querySelectorAll('.maplibregl-marker').length === 1, 5000)) ok('pin back after returning to explore'); else fail('pin missing after theme switch');

  // the edge of the walking network is drawn, and the theme switch did not drop it
  if (await page.evaluate(() => { const m = window.timeshed.map; return !!m.getLayer('extent-line') && m.getSource('extent').serialize().data.features.length === 2; })) ok('network edge drawn');
  else fail('network edge missing after the theme switch');

  // switch city if the page offers more than one: the other engine loads and routes
  const cityKeys = await page.$$eval('#city option', (els) => els.map((o) => o.value));
  if (cityKeys.length > 1) {
    await page.select('#city', cityKeys[1]);
    if (await until(() => /stops reachable/.test(document.querySelector('#stats').textContent) && !/routing/.test(document.querySelector('#stats').textContent), 120000)) {
      ok(`switched to ${cityKeys[1]}: ` + await stats());
    } else fail(`city switch to ${cityKeys[1]} did not route: ` + await stats());
    await page.select('#city', cityKeys[0]);
    if (await until(() => /stops reachable/.test(document.querySelector('#stats').textContent), 120000)) ok(`back to ${cityKeys[0]}`); else fail('switch back did not route');
  } else ok('single city, no switch to test');

  // in Findings, switching city must move the map and swap the data
  if (cityKeys.length > 1) {
    await page.click('#tabs button[data-mode="findings"]');
    await until(() => document.querySelectorAll('#tiles .tile').length === 4, 30000);
    const before = await page.evaluate(() => window.timeshed.map.getCenter().lng);
    await page.select('#city', cityKeys[1]);
    await new Promise((r) => setTimeout(r, 2500));
    const after = await page.evaluate(() => window.timeshed.map.getCenter().lng);
    const title = await page.$eval('#f-title', (el) => el.textContent);
    if (Math.abs(after - before) > 0.5 && /Cincinnati/.test(title)) ok(`findings follows the city: ${before.toFixed(2)} -> ${after.toFixed(2)}, "${title}"`);
    else fail(`findings did not follow the city switch: lng ${before.toFixed(2)} -> ${after.toFixed(2)}, title "${title}"`);
    await page.select('#city', cityKeys[0]);
    await new Promise((r) => setTimeout(r, 2500));
  }

  await page.goto(base + '/findings/', { waitUntil: 'networkidle0', timeout: 120000 });
  if (await until(() => !document.querySelector('#findings').hidden, 10000)) ok('/findings/ opens in findings'); else fail('/findings/ did not open in findings mode');
  await page.click('#tabs button[data-mode="explore"]');
  if (await until(() => /stops reachable/.test(document.querySelector('#stats').textContent), 120000)) ok('explore routes from /findings/'); else fail('explore did not route from /findings/: ' + await stats());

  // a second city's findings entry page opens on that city
  if (cityKeys.length > 1) {
    await page.goto(`${base}/${cityKeys[1]}/findings/`, { waitUntil: 'networkidle0', timeout: 120000 });
    await until(() => document.querySelectorAll('#tiles .tile').length === 4, 30000);
    const lng = await page.evaluate(() => window.timeshed.map.getCenter().lng);
    const expected = await page.evaluate((k) => window.TIMESHED.cities[k].center[0], cityKeys[1]);
    if (Math.abs(lng - expected) < 0.3) ok(`/${cityKeys[1]}/findings/ opens on ${cityKeys[1]}`); else fail(`/${cityKeys[1]}/findings/ opened at lng ${lng.toFixed(2)}, expected ${expected}`);
  }

  // a link whose pin is nowhere near the city starts from the centre and says so
  await page.goto(`${base}/${cityKeys[0]}/#lat=10&lon=10&t=480`, { waitUntil: 'networkidle0', timeout: 120000 });
  if (await until(() => /stops reachable/.test(document.querySelector('#stats').textContent) && /outside/.test(document.querySelector('#toast.show')?.textContent || ''), 120000)
      && Math.abs((await pinAt())[1] - 10) > 1) ok('a link with a pin off the network starts from the centre');
  else fail(`bad link pin: marker at ${(await pinAt()).map((v) => v.toFixed(4))}, toast "${await toastText()}", stats "${await stats()}"`);

  const res = await fetch(base + '/live/');
  if (res.ok && /url=\.\.\//.test(await res.text())) ok('/live/ redirects'); else fail('/live/ redirect missing');
  await browser.close();
} catch (e) {
  fail('exception: ' + (e && e.message));
} finally {
  server.kill();
}
if (failures.length) { console.log(`\n${failures.length} failure(s)`); process.exit(1); }
console.log('\nall good');
