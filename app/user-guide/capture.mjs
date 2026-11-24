// Takes the pictures of the user guide: serves app/src with mock.js (example
// phones) on 127.0.0.1, opens it in a headless Edge, walks through the screens
// and saves each one in img/. Run: node capture.mjs  (from this folder).
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const src = path.join(here, '..', 'src');
const imgDir = path.join(here, 'img');
fs.mkdirSync(imgDir, { recursive: true });
const PORT = 8766;
const DEBUG = 9334;
const W = 1600;
const H = 1000;

const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.png': 'image/png', '.json': 'application/json', '.svg': 'image/svg+xml' };
const server = http.createServer((req, res) => {
  const name = decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '') || 'index.html';
  let file = ['mock.js', 'demo-apps.json'].includes(name) ? path.join(here, name) : path.join(src, name);
  if (!file.startsWith(src) && !file.startsWith(here)) { res.writeHead(403); res.end(); return; }
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  let body = fs.readFileSync(file);
  if (name === 'index.html') body = Buffer.from(body.toString('utf8').replace('<script src="main.js"></script>', '<script src="mock.js"></script><script src="main.js"></script>'));
  res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' });
  res.end(body);
});
await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

const edge = 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const profile = path.join(process.env.TEMP, 'mirryl-guide-edge');
const browser = spawn(edge, ['--headless=new', `--remote-debugging-port=${DEBUG}`, `--user-data-dir=${profile}`, `--window-size=${W},${H}`, '--hide-scrollbars', `http://127.0.0.1:${PORT}/index.html`], { stdio: 'ignore' });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

let page;
for (let i = 0; i < 40 && !page; i++) {
  await wait(500);
  try { page = (await (await fetch(`http://127.0.0.1:${DEBUG}/json`)).json()).find((t) => t.type === 'page'); } catch (_) { /* not up yet */ }
}
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let id = 0;
const send = (method, params = {}) => new Promise((resolve) => {
  const my = ++id;
  const on = (ev) => { const m = JSON.parse(ev.data); if (m.id === my) { ws.removeEventListener('message', on); resolve(m.result); } };
  ws.addEventListener('message', on);
  ws.send(JSON.stringify({ id: my, method, params }));
});
const run = async (js) => (await send('Runtime.evaluate', { expression: `(async()=>{${js}})()`, awaitPromise: true, returnByValue: true }))?.result?.value;
const shot = async (name, clip) => {
  await wait(400);
  const r = await send('Page.captureScreenshot', { format: 'png', ...(clip ? { clip: { ...clip, scale: 1 } } : {}) });
  fs.writeFileSync(path.join(imgDir, name + '.png'), Buffer.from(r.data, 'base64'));
  console.log('saved', name);
};

await send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false });
await send('Page.reload');
await wait(3000);

const helpers = `
  window.$row = (n) => [...document.querySelectorAll('.device-row')].find((r) => r.innerText.includes(n));
  window.$tile = (p) => [...document.querySelectorAll('.app-tile')].find((t) => t.title === p);
  window.$nav = (k, v) => [...document.querySelectorAll('.device-nav-item')].find((b) => b.dataset[k] === v);
  window.$ctx = (el) => { const r = el.getBoundingClientRect(); el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.x + 30, clientY: r.y + 20 })); };
  window.$sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  return 1;`;
await run(helpers);

await shot('01-home');
await run(`$ctx($row('Sales phone')); return 1;`);
await shot('02-row-menu', { x: 0, y: 0, width: 760, height: 520 });
await run(`document.body.click(); await $sleep(200); document.getElementById('dmSelectModeBtn').click(); await $sleep(200);
  $row('Sales phone').querySelector('.device-select-checkbox').click(); await $sleep(100);
  $row('Warehouse tablet').querySelector('.device-select-checkbox').click(); return 1;`);
await shot('03-select-phones', { x: 0, y: 0, width: 420, height: 560 });
await run(`document.getElementById('dmSelectionCancelBtn').click(); await $sleep(200);
  $row('Sales phone').querySelector('.device-menu-btn').click(); await $sleep(2500); return 1;`);
await shot('04-device-apps');
await run(`$ctx($tile('com.google.android.youtube')); return 1;`);
await shot('05-app-menu');
await run(`document.body.click(); await $sleep(200); document.getElementById('appsSelectBtn').click(); await $sleep(200);
  for (const p of ['com.google.android.calendar', 'com.google.android.gm', 'com.google.android.apps.maps']) { $tile(p).dispatchEvent(new MouseEvent('click', { bubbles: true })); await $sleep(100); }
  return 1;`);
await shot('06-apps-select');
await run(`document.getElementById('appsSelDone').click(); await $sleep(200); $nav('panel', 'info').click(); await $sleep(800); return 1;`);
await shot('07-info');
await run(`$nav('action', 'pin').click(); await $sleep(800); return 1;`);
await shot('08-pin');
await run(`document.getElementById('pinCancelBtn').click(); await $sleep(200); $nav('action', 'profile-apply').click(); await $sleep(800); return 1;`);
await shot('09-profile');
await run(`document.getElementById('profileCancelBtn').click(); await $sleep(200); $nav('panel', 'apps').click(); await $sleep(800);
  document.querySelector('#appsFilter button[data-filter=blocked]').click(); await $sleep(400); return 1;`);
await shot('10-blocked-filter');
await run(`document.getElementById('settingsBtn').click(); await $sleep(800); return 1;`);
await shot('11-settings');
await run(`document.getElementById('actAdd').click(); await $sleep(1000); return 1;`);
await shot('12-add-device');
await run(`document.getElementById('actLicenses').click(); await $sleep(800); document.getElementById('homePane').scrollTop = 99999; await $sleep(300); return 1;`);
await shot('13-about');

ws.close();
browser.kill();
server.close();
console.log('done');
process.exit(0);
