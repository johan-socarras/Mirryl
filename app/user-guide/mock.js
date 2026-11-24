// Stands in for the Rust side when the page is opened in a plain browser to
// take the pictures of the user guide (capture.mjs). Example phones only:
// nothing here talks to a phone or reads the PC.
(function () {
  const devices = [
    { id: 1, alias: 'Sales phone', host_port: '192.168.1.41:37000', serial: 'R58SALES01' },
    { id: 2, alias: 'Warehouse tablet', host_port: '', serial: 'R52WAREHOUSE2' },
    { id: 3, alias: 'Front desk', host_port: '192.168.1.60:41231', serial: 'R58FRONT03' },
  ];
  const status = 'List of devices attached\n'
    + '192.168.1.41:37000     device product:a54xnsxx model:SM_A546E device:a54x transport_id:3\n'
    + 'R52WAREHOUSE2          device product:gta9 model:SM_X210 device:gta9 transport_id:4\n';
  const blocked = ['com.google.android.youtube', 'com.android.chrome'];
  const pinned = ['com.android.settings', 'com.google.android.gm', 'com.google.android.packageinstaller'];
  let apps = null;
  async function loadApps() {
    if (!apps) {
      apps = await (await fetch('demo-apps.json')).json();
      apps = apps.filter((a) => a.package !== 'com.awadb.app');
      for (const a of apps) {
        a.suspended = blocked.includes(a.package);
        a.locked = pinned.includes(a.package);
        a.protected = ['com.mirryl.app', 'com.android.settings', 'com.android.vending', 'com.google.android.gms',
          'com.google.android.apps.nexuslauncher', 'com.android.systemui', 'com.android.phone'].includes(a.package);
      }
      apps.sort((x, y) => x.label.toLowerCase().localeCompare(y.label.toLowerCase()));
    }
    return apps;
  }
  // A picture that looks like a QR code (it encodes nothing): the real one
  // carries a pairing secret and is never put in a document.
  function exampleQr() {
    const n = 29;
    let seed = 7;
    const rnd = () => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed / 2147483648; };
    let cells = '';
    const inFinder = (x, y) => [[0, 0], [n - 7, 0], [0, n - 7]].some(([fx, fy]) => x >= fx && x < fx + 7 && y >= fy && y < fy + 7);
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        if (inFinder(x, y)) continue;
        if (rnd() < 0.48) cells += `<rect x="${x}" y="${y}" width="1" height="1"/>`;
      }
    }
    for (const [fx, fy] of [[0, 0], [n - 7, 0], [0, n - 7]]) {
      cells += `<rect x="${fx}" y="${fy}" width="7" height="7"/><rect x="${fx + 1}" y="${fy + 1}" width="5" height="5" fill="#fff"/><rect x="${fx + 2}" y="${fy + 2}" width="3" height="3"/>`;
    }
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-2 -2 ${n + 4} ${n + 4}" shape-rendering="crispEdges"><rect x="-2" y="-2" width="${n + 4}" height="${n + 4}" fill="#fff"/>${cells}</svg>`;
    return 'data:image/svg+xml;base64,' + btoa(svg);
  }
  const answers = {
    list_devices: () => devices,
    reconcile_devices: () => devices,
    get_status: () => status,
    device_serial: (a) => (a.hostPort === '192.168.1.41:37000' ? 'R58SALES01' : null),
    mdns_transports: () => [],
    list_open_mirrors: () => [],
    system_health: () => ({ needs_firewall_fix: false, needs_watcher_fix: false, firewall: 'ok', watcher: 'ok' }),
    get_theme: () => 'light',
    set_theme: () => null,
    get_keybindings: () => '',
    app_version: () => '1.0.12',
    pin_alerts: () => [],
    get_profiles: () => [
      { name: 'Employee', block: [{ package: 'com.google.android.youtube', label: 'YouTube' }, { package: 'com.android.chrome', label: 'Chrome' }],
        pin: [{ package: 'com.android.settings', label: 'Settings' }, { package: 'com.google.android.gm', label: 'Gmail' }] },
      { name: 'Kiosk', block: [{ package: 'com.android.vending', label: 'Google Play Store' }], pin: [{ package: 'com.android.settings', label: 'Settings' }] },
    ],
    list_apps: async () => ({ apps: await loadApps(), home: 'com.google.android.apps.nexuslauncher', lock: { supported: true, pin: true } }),
    device_info: () => ({ battery: '82%', storage: '41.3 GB free', android: 'Android 16', model: 'SM-A546E', serial: 'R58SALES01', uptime: '3d 4h', ip: '192.168.1.41', reachable: true }),
    get_third_party_licenses: () => 'Third-party licenses…',
    start_qr_pairing: () => exampleQr(),
    get_qr_pairing_status: () => ({ state: 'waiting' }),
  };
  window.__TAURI__ = {
    core: {
      invoke: async (command, args) => {
        const answer = answers[command];
        return answer ? answer(args || {}) : null;
      },
    },
    event: { listen: async () => () => {} },
  };
})();
