// ---------- Mirryl frontend ----------
// Talks to the Rust commands registered in src-tauri/src/main.rs via
// window.__TAURI__.core.invoke (enabled by "app.withGlobalTauri" in
// tauri.conf.json, so no bundler/ES import is required).
//
// Guarded against double execution: WebView2 can run main.js twice on a
// single page load, and the second run would throw "Identifier already
// declared" and silently kill every button. The guard makes it a no-op.
if (!window.__AWADB_MAIN_LOADED__) {
  window.__AWADB_MAIN_LOADED__ = true;

  const isTauri = typeof window.__TAURI__ !== 'undefined';

  const callBackend = async (command, args) => {
    if (!isTauri) {
      return '(Not running inside the Tauri app - open this page through the desktop app, not a plain browser.)';
    }
    try {
      return await window.__TAURI__.core.invoke(command, args || {});
    } catch (e) {
      return 'Error: ' + e;
    }
  };

  // The WebView2 default right-click menu (Reload, Inspect Element, ...) has
  // no relation to Mirryl and would show up everywhere. Device rows replace
  // it with their own menu below; every other spot just gets nothing.
  document.addEventListener('contextmenu', (e) => e.preventDefault());

  // ---------- View navigation ----------
  // Every screen that is not a mirror lives in the Home tab, so showing one
  // brings that tab to the front.
  function showView(id) {
    document.querySelectorAll('.view').forEach(v => v.classList.toggle('active', v.id === id));
    document.getElementById('homePane').scrollTop = 0;
    activateTab('home');
  }

  // ---------- Theme (manual, persisted - defaults to light regardless of
  // OS setting, and stays how the user last left it across restarts/reboots) ----------
  function applyTheme(theme) {
    document.documentElement.dataset.theme = theme;
    document.querySelectorAll('#themeChoice button').forEach(button => {
      button.classList.toggle('active', button.dataset.themeValue === theme);
    });
  }
  document.querySelectorAll('#themeChoice button').forEach(button => {
    button.addEventListener('click', async () => {
      const next = button.dataset.themeValue;
      applyTheme(next);
      await callBackend('set_theme', { theme: next });
    });
  });
  (async () => {
    const saved = await callBackend('get_theme');
    applyTheme(saved === 'dark' ? 'dark' : 'light');
  })();

  // ---------- Settings ----------
  document.getElementById('settingsBtn').addEventListener('click', () => showView('view-settings'));
  document.getElementById('settingsBackLink').addEventListener('click', () => showView('view-home'));

  // ---------- Keyboard shortcuts ----------
  // THE registry. Everything about an action lives on one line here: its id (the
  // key in keybindings.json, and the name the mirror window looks it up by), how
  // it reads on screen, and the shortcut it ships with.
  //
  // A new action is an ENTRY IN THIS LIST, not a new screen: adding one touches
  // this array and nothing else.
  //
  // `defaultBinding` has a twin in the engine (viewer.rs, DEFAULT_KEYBINDINGS).
  // The engine needs its own copy for the case where this app has never saved
  // the file. If you change one, change the other.
  //
  // Not actions and not rebindable: what the mirror window does with these is
  // fixed, and they are refused when picking a shortcut (see the capture below).
  const RESERVED_SHORTCUTS = {
    'Ctrl+A': 'Select all',
    'Ctrl+C': 'Copy',
    'Ctrl+V': 'Paste',
    'Ctrl+X': 'Cut',
    'Ctrl+Z': 'Undo',
  };
  const SHORTCUT_ACTIONS = [
    {
      id: 'panel_power',
      name: 'Turn the phone screen off',
      description: 'Blanks the phone while the mirror keeps running. Press it again to bring the screen back.',
      defaultBinding: 'Ctrl+S',
    },
    // Android's other two nav buttons - BACK already has its own fixed
    // gesture (right-click in the mirror window) and is not in this list.
    // Home also has a fixed gesture of its own (middle-click) - F1/F2 here
    // are a second way to reach both alongside the mouse.
    {
      id: 'home',
      name: 'Go to home screen',
      description: 'Same as pressing the circle/home button on Android. Middle-click in the mirror window does this too.',
      defaultBinding: 'F1',
    },
    {
      id: 'recent_apps',
      name: 'Recent apps',
      description: 'Same as pressing the square/overview button on Android - shows apps running in the background.',
      defaultBinding: 'F2',
    },
    {
      id: 'rotate_view',
      name: 'Rotate the mirror view',
      description: 'Puts the window back to the shape of the picture it is showing. Only needed by hand when the phone changed shape without the window following on its own.',
      defaultBinding: 'Ctrl+R',
    },
    {
      id: 'fill_screen',
      name: 'Fill the space',
      description: 'Makes the picture cover the whole tab or window, cutting the edges that do not fit, instead of showing the whole phone screen with black bars. Press it again to see all of it. The Fill button on a mirror tab does the same.',
      defaultBinding: 'Ctrl+F',
    },
    {
      id: 'game_mode',
      name: 'Game mode',
      description: 'While it is on, single keys act as shortcuts instead of being typed into the phone. This is what makes W A S D usable.',
      defaultBinding: 'F12',
    },
    {
      id: 'swipe_up',
      name: 'Swipe up',
      description: 'Scrolls up, the way the Up arrow or a scroll wheel would - so you see what is above.',
      defaultBinding: 'W',
    },
    {
      id: 'swipe_down',
      name: 'Swipe down',
      description: 'Scrolls down - so you see what is below.',
      defaultBinding: 'S',
    },
    {
      id: 'swipe_left',
      name: 'Swipe left',
      description: 'Moves left, the way the Left arrow would.',
      defaultBinding: 'A',
    },
    {
      id: 'swipe_right',
      name: 'Swipe right',
      description: 'Moves right, the way the Right arrow would.',
      defaultBinding: 'D',
    },
    // The other way to move the screen from a key: HOLD it, and it drags
    // continuously - like holding the left mouse button and moving - instead
    // of one quick flick per press. A game map taller than one swipe can reach
    // reads as jerky with W A S D as swipes, and smooth with them held to pan.
    //
    // Unbound by default (see the empty defaultBinding) because the obvious
    // keys are W A S D, and Swipe up/etc already own those out of the box.
    // Free them up here first - change Swipe up's key to something else - and
    // THEN this box will accept W; the same key cannot run both at once.
    {
      id: 'drag_up',
      name: 'Drag up (hold)',
      description: 'While held, drags up continuously - like holding the left mouse button and moving, not a quick flick. Needs a different key than Swipe up.',
      defaultBinding: '',
    },
    {
      id: 'drag_down',
      name: 'Drag down (hold)',
      description: 'While held, drags down continuously. Needs a different key than Swipe down.',
      defaultBinding: '',
    },
    {
      id: 'drag_left',
      name: 'Drag left (hold)',
      description: 'While held, drags left continuously. Needs a different key than Swipe left.',
      defaultBinding: '',
    },
    {
      id: 'drag_right',
      name: 'Drag right (hold)',
      description: 'While held, drags right continuously. Needs a different key than Swipe right.',
      defaultBinding: '',
    },
  ];

  // Game mode and its keys are switched off: testing found real bugs in them
  // while playing a game, and shipping without them beats shipping them
  // half-broken. The array above keeps every action defined; this just keeps them out of
  // Settings so nothing shows an "F12" row that quietly does nothing. The
  // matching gate is viewer-v070.html's GAME_MODE_ENABLED - undo both to
  // bring the cluster back.
  const GAME_MODE_ACTION_IDS = new Set([
    'game_mode', 'swipe_up', 'swipe_down', 'swipe_left', 'swipe_right',
    'drag_up', 'drag_down', 'drag_left', 'drag_right',
  ]);
  const VISIBLE_SHORTCUT_ACTIONS = SHORTCUT_ACTIONS.filter(action => !GAME_MODE_ACTION_IDS.has(action.id));

  let shortcutBindings = {};
  let capturingAction = null;

  /**
   * Turns a key press into the text a shortcut is stored as: "Ctrl+Shift+O",
   * "Alt+F5", "F9". Modifiers always in this order, so the same press always
   * writes the same string and a saved shortcut can be compared as plain text.
   *
   * THE MIRROR WINDOW HAS THE SAME FUNCTION (viewer-v070.html, describeKey).
   * They have to agree exactly: this one writes the file, that one matches
   * against it. A difference here is a shortcut that saves and never fires.
   */
  function describeKey(event) {
    const parts = [];
    if (event.ctrlKey) parts.push('Ctrl');
    if (event.altKey) parts.push('Alt');
    if (event.shiftKey) parts.push('Shift');
    const key = event.key;
    if (key === 'Control' || key === 'Alt' || key === 'Shift' || key === 'Meta') return null;
    parts.push(key.length === 1 ? key.toUpperCase() : key);
    return parts.join('+');
  }

  /**
   * Whether this key would otherwise be typed into the phone, and so only
   * works as a shortcut while game mode is on.
   *
   * Shift counts as bare: Shift+W is how a capital W is typed. Ctrl and Alt
   * type nothing, so they are always safe.
   *
   * THE MIRROR WINDOW MAKES THE SAME JUDGEMENT (viewer-v070.html,
   * needsGameMode) to decide whether to act on a key. This one only decides
   * what to warn about, so a difference here misleads rather than breaks -
   * but keep them in step anyway.
   */
  function needsGameMode(event) {
    return !event.ctrlKey && !event.altKey && [...event.key].length === 1;
  }

  /** The same judgement as above, read back off a saved shortcut. */
  function bindingNeedsGameMode(binding) {
    const parts = binding.split('+');
    const key = parts[parts.length - 1];
    return !parts.includes('Ctrl') && !parts.includes('Alt') && [...key].length === 1;
  }

  function showShortcutMessage(text) {
    const box = document.getElementById('shortcutMessage');
    box.textContent = text;
    box.style.display = text ? 'block' : 'none';
  }

  function renderShortcuts() {
    const list = document.getElementById('shortcutList');
    list.innerHTML = '';
    VISIBLE_SHORTCUT_ACTIONS.forEach(action => {
      const binding = shortcutBindings[action.id] || action.defaultBinding;
      const row = document.createElement('div');
      row.className = 'setting-row';

      const text = document.createElement('div');
      text.className = 'setting-text';
      const name = document.createElement('div');
      name.className = 'setting-name';
      name.textContent = action.name;
      const desc = document.createElement('div');
      desc.className = 'setting-desc';
      // Said on the row itself, not only when the key is chosen: someone
      // looking at "W" months later needs to know why it does nothing when
      // they try it, without having to remember the message shown on saving.
      desc.textContent = bindingNeedsGameMode(binding)
        ? `${action.description} Works while game mode is on.`
        : action.description;
      text.append(name, desc);

      const actions = document.createElement('div');
      actions.className = 'shortcut-actions';
      const box = document.createElement('button');
      box.className = 'secondary shortcut-box';
      // The drag_* actions ship with no default, so an unrouted binding shows
      // as blank without this - which reads as broken, not as "unset".
      box.textContent = capturingAction === action.id ? 'Press a key…' : (binding || 'Not set');
      if (capturingAction === action.id) box.classList.add('listening');
      box.addEventListener('click', () => {
        capturingAction = capturingAction === action.id ? null : action.id;
        showShortcutMessage(capturingAction
          ? 'Press the combination you want. Escape cancels.'
          : '');
        renderShortcuts();
      });
      actions.append(box);

      if (binding !== action.defaultBinding) {
        const reset = document.createElement('button');
        reset.className = 'ghost';
        reset.textContent = 'Reset';
        reset.addEventListener('click', async () => {
          delete shortcutBindings[action.id];
          await saveShortcuts();
          showShortcutMessage(`Back to ${action.defaultBinding}.`);
          renderShortcuts();
        });
        actions.append(reset);
      }

      row.append(text, actions);
      list.append(row);
    });
  }

  async function saveShortcuts() {
    // The COMPLETE map goes to disk, defaults included, so the file on its own
    // says what every action is bound to. The engine then never has to guess
    // whether a missing key means "default" or "the app is older than this
    // action".
    const complete = {};
    SHORTCUT_ACTIONS.forEach(action => {
      complete[action.id] = shortcutBindings[action.id] || action.defaultBinding;
    });
    const result = await callBackend('set_keybindings', { bindings: JSON.stringify(complete) });
    if (typeof result === 'string' && result.startsWith('Error:')) {
      showShortcutMessage(`Could not save the shortcut. ${result}`);
    }
  }

  // Capture runs on the whole window while a box is listening, because the
  // press has to be caught before the browser does anything with it.
  window.addEventListener('keydown', async event => {
    if (!capturingAction) return;
    event.preventDefault();
    if (event.key === 'Escape') {
      capturingAction = null;
      showShortcutMessage('');
      renderShortcuts();
      return;
    }
    const combination = describeKey(event);
    if (!combination) return; // A modifier on its own: keep waiting for the rest.

    // The mirror window passes these to the phone's text fields (viewer-v070.html,
    // phoneCtrlKeys and the paste listener). A shortcut on one would win there
    // and take copy or paste away, so they cannot be picked.
    const reserved = RESERVED_SHORTCUTS[combination];
    if (reserved) {
      showShortcutMessage(`${combination} is already used by "${reserved}".`);
      return;
    }
    const clash = SHORTCUT_ACTIONS.find(action =>
      action.id !== capturingAction
      && (shortcutBindings[action.id] || action.defaultBinding) === combination);
    if (clash) {
      showShortcutMessage(`${combination} is already used by "${clash.name}".`);
      return;
    }

    shortcutBindings[capturingAction] = combination;
    const gated = needsGameMode(event);
    capturingAction = null;
    await saveShortcuts();
    showShortcutMessage(gated
      ? `Saved as ${combination}. A single key only acts while game mode is on `
        + '- with it off, that key is typed into the phone as usual. '
        + 'Mirrors you open from now on will use it.'
      : `Saved as ${combination}. Mirrors you open from now on will use it.`);
    renderShortcuts();
  });

  (async () => {
    const saved = await callBackend('get_keybindings');
    try {
      const parsed = JSON.parse(saved);
      if (parsed && typeof parsed === 'object') shortcutBindings = parsed;
    } catch (_) {
      // A file we cannot read is the same as no file: the defaults below still
      // render, and the next save overwrites whatever is there.
    }
    renderShortcuts();
  })();

  // ---------- Device Manager ----------
  // `devices` mirrors the backend's JSON registry (list_devices/add_device/
  // update_device/remove_device). Connection status isn't stored - it's
  // derived live each refresh by matching each device's host_port against
  // the latest `adb devices -l` output (see refreshStatus below).
  let devices = [];
  let lastStatusText = '';

  // ---------- Search / filter / sort (2+ devices only, see renderDeviceManager) ----------
  let dmSearchQuery = '';
  let dmStatusFilter = 'all'; // 'all' | 'usb' | 'wifi' | 'disconnected'
  let dmSortMode = 'name-asc'; // 'name-asc' | 'name-desc' | 'last-seen' | 'added-desc' | 'added-asc'
  // The checked rows. Every bulk action (Send file, Restart, Remove) acts on
  // exactly these and nothing else: restarting twenty phones when three were
  // meant is the accident nobody forgives.
  // The checkboxes only exist with 2+ devices - see multiDevice() - and only
  // once Select is pressed. A column of ticks on every row, always on, for
  // something used now and then, is clutter on the screen you look at every
  // day.
  let dmSelectedIds = new Set();
  // Off until Select turns it on. While on, the rows carry checkboxes and the
  // row itself toggles them; Done in the bar puts it away.
  let dmSelectionMode = false;

  /** Search, filters and the Select button all start at two phones. */
  function multiDevice() {
    return devices.length > 1;
  }

  // ---------- Last seen, kept across restarts ----------
  // lastSeenConnected (below) is this session only and feeds the
  // "Reconnecting..." grace. Sorting by last seen needs it to survive a
  // restart, and it is nobody's business but this PC's, so it lives in the
  // webview's own storage, keyed by device id (ids are never reused - see
  // the 'added-desc' sort). Written at most every 30 s while phones stay
  // connected, and when the window closes.
  const LAST_SEEN_KEY = 'mirryl.lastSeen';
  const LAST_SEEN_SAVE_MS = 30000;
  const lastSeenStored = (() => {
    try {
      const parsed = JSON.parse(localStorage.getItem(LAST_SEEN_KEY) || '{}');
      return parsed && typeof parsed === 'object' ? parsed : {};
    } catch (e) {
      return {};
    }
  })();
  let lastSeenSavedAt = 0;

  function saveLastSeen() {
    lastSeenSavedAt = Date.now();
    try { localStorage.setItem(LAST_SEEN_KEY, JSON.stringify(lastSeenStored)); } catch (e) { /* no storage, no memory: sorting still works this session */ }
  }

  function noteSeen(id) {
    lastSeenStored[id] = Date.now();
    if (Date.now() - lastSeenSavedAt > LAST_SEEN_SAVE_MS) saveLastSeen();
  }

  function forgetSeen(id) {
    delete lastSeenStored[id];
    saveLastSeen();
  }

  window.addEventListener('beforeunload', saveLastSeen);

  /** "just now", "5 min ago", "3 h ago", "2 days ago". */
  function agoText(ms) {
    const minutes = Math.floor((Date.now() - ms) / 60000);
    if (minutes < 1) return 'just now';
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours} h ago`;
    const days = Math.floor(hours / 24);
    return `${days} day${days === 1 ? '' : 's'} ago`;
  }

  /** The link a connected row is on - the same answer its icon gives. */
  function connectedKind(d) {
    return mirrorKind.get(String(d.id)) || liveTransport(d).kind || 'wifi';
  }

  /** devices, filtered by dmSearchQuery/dmStatusFilter and sorted by dmSortMode. */
  function visibleDevices() {
    let list = devices.slice();
    const q = dmSearchQuery.trim().toLowerCase();
    if (q) {
      list = list.filter((d) =>
        d.alias.toLowerCase().includes(q)
        || (d.serial || '').toLowerCase().includes(q)
        || (d.host_port || '').toLowerCase().includes(q));
    }
    if (dmStatusFilter === 'usb' || dmStatusFilter === 'wifi') {
      list = list.filter((d) => deviceStatus(d) === 'connected' && connectedKind(d) === dmStatusFilter);
    } else if (dmStatusFilter === 'disconnected') {
      list = list.filter((d) => deviceStatus(d) !== 'connected');
    }
    // numeric:true makes this a NATURAL sort ("Device 2" before "Device 10") -
    // plain localeCompare would put "Device 10" before "Device 2" since '1' <
    // '2' as a character, which reads as broken with numbered aliases.
    if (dmSortMode === 'name-asc') {
      list.sort((a, b) => a.alias.localeCompare(b.alias, undefined, { sensitivity: 'base', numeric: true }));
    } else if (dmSortMode === 'name-desc') {
      list.sort((a, b) => b.alias.localeCompare(a.alias, undefined, { sensitivity: 'base', numeric: true }));
    } else if (dmSortMode === 'last-seen') {
      // Connected now beats any memory; never seen goes last; ties by name.
      const seenAt = (d) => (deviceStatus(d) === 'connected' ? Infinity : (lastSeenStored[d.id] || 0));
      list.sort((a, b) => (seenAt(b) - seenAt(a))
        || a.alias.localeCompare(b.alias, undefined, { sensitivity: 'base', numeric: true }));
    } else if (dmSortMode === 'added-desc') {
      // No separate "added at" timestamp is stored, by design: id is assigned
      // as (max existing id) + 1 on every add (see upsert_device in main.rs)
      // and is NEVER renumbered when a device is removed, so it doubles as
      // "add order". The gaps a delete leaves are invisible and don't affect
      // ordering. Renumbering would detach a row from the in-session state
      // keyed by its id (open mirrors, the bulk selection).
      list.sort((a, b) => b.id - a.id);
    } else {
      // 'added-asc'
      list.sort((a, b) => a.id - b.id);
    }
    return list;
  }
  const STATUS_META = {
    connected: { label: 'Connected', varName: '--success' },
    connecting: { label: 'Connecting...', varName: '--accent' },
    reconnecting: { label: 'Reconnecting...', varName: '--warn' },
    disconnected: { label: 'Disconnected', varName: '--neutral' },
  };
  /**
   * How long a single Connect attempt is allowed to own the row.
   *
   * Connecting is slow when the phone is unreachable - adb connect gives up
   * after ~3 s and the mDNS lookup can add 5 more - and a row that sat on
   * "Disconnected" the whole time would invite clicking again, each click
   * another engine process.
   *
   * The ceiling exists so a wedged attempt cannot lock the button forever.
   */
  const CONNECT_TIMEOUT_MS = 30000;
  const connectingUntil = new Map();
  // Device id -> { until, reason } while a failed attempt is being shown.
  const failedUntil = new Map();

  function isConnecting(d) {
    const until = connectingUntil.get(d.id);
    if (until === undefined) return false;
    if (Date.now() >= until) {
      connectingUntil.delete(d.id);
      return false;
    }
    return true;
  }
  // A phone that dropped off is not the same as a phone that was never there:
  // the watcher retries every 25 s, so for the first couple of minutes the
  // honest label is "Reconnecting...". Past that the phone almost always has
  // Wireless debugging switched off, which no amount of retrying from the PC
  // can fix - that is when "Disconnected" means "go look at the phone".
  const RECONNECT_GRACE_MS = 120000;
  const lastSeenConnected = new Map();
  const lastTransportKind = new Map();
  /**
   * Device ids (as strings) that already have a mirror window open.
   *
   * Opening the same phone twice is never worth anything: both windows show the
   * same screen, and each one is a whole engine - encoding on the phone,
   * decoding on the PC.
   *
   * Kept in step from two directions: added the moment we launch one (the poll
   * is up to 3 s behind, and the row must lock immediately), and rebuilt from
   * the backend on every poll, which is what notices a window the user closed.
   */
  const openMirrors = new Set();

  /**
   * Device id -> the link its OPEN MIRROR is on ('usb' or 'wifi').
   *
   * Not the same as the device's best available link, and that difference is
   * the whole point. Plugging a cable into a phone that is being mirrored over
   * Wi-Fi makes USB available, but the mirror stays on Wi-Fi until the user
   * says otherwise. While a mirror is open the row follows the mirror; the
   * engine reports it, so a switch is reflected too.
   */
  const mirrorKind = new Map();

  async function refreshOpenMirrors() {
    const result = await callBackend('list_open_mirrors');
    if (!Array.isArray(result)) return;
    openMirrors.clear();
    const seen = new Set();
    result.forEach((entry) => {
      const key = String(entry && entry.key !== undefined ? entry.key : entry);
      openMirrors.add(key);
      seen.add(key);
      // An empty kind means the engine has not reported yet; keep whatever we
      // recorded at launch rather than blanking the icon for a second.
      if (entry && entry.kind) mirrorKind.set(key, entry.kind);
    });
    [...mirrorKind.keys()].forEach((key) => { if (!seen.has(key)) mirrorKind.delete(key); });
  }

  async function loadDevices(reconcile = false) {
    const result = await callBackend('list_devices');
    devices = Array.isArray(result) ? result : [];
    renderDeviceManager();
    // When entering the Device Manager (not during the wizard), reconcile
    // adb's connections with our registry: this disconnects phantom wireless
    // devices (deleted ones, or connections made outside the app) so the LIVE
    // STATUS matches the managed list, and writes down the new address of a
    // registered phone found somewhere else. Never call with reconcile=true
    // from the pairing flow - a just-connected phone may not be saved yet.
    // Painted first, reconciled after: reconcile asks each unknown transport
    // who it is (up to 5 s each), and the list should not
    // sit empty while it does. It reloads the rows when it is done, because
    // it may have changed them.
    if (reconcile) {
      callBackend('reconcile_devices').then(() => loadDevices(false)).catch(() => {});
    }
  }

  // `adb devices -l` lines are "<address> <state> [key:value ...]".
  //
  // The address is NOT one whitespace-free token. mDNS renames a service when
  // the name is already taken, and the rename puts a space inside it:
  //
  //   adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp device product:m2quew ...
  //
  // Splitting on whitespace and reading field 1 as the state would throw the
  // whole line away, and report "Disconnected" for a phone that is connected
  // and working.
  //
  // So: find the STATE keyword and take everything before it as the address.
  // Non-greedy on purpose - `device:m2q` appears later on the same line, and
  // the first match is the real state. ("no permissions" has a space too.)
  const ADB_LINE = /^(.*?)\s+(device|offline|unauthorized|authorizing|connecting|bootloader|recovery|sideload|host|no permissions)(?:\s|$)/;

  function parseAdbLine(line) {
    const m = ADB_LINE.exec(line.trim());
    return m ? { address: m[1].trim(), state: m[2] } : null;
  }

  function connectedAddresses() {
    return lastStatusText
      .split('\n')
      .map(parseAdbLine)
      .filter((row) => row && row.state === 'device')
      .map((row) => row.address);
  }

  // A USB transport is the bare hardware serial. Everything else is wireless:
  // `ip:port`, or the mDNS name - which starts with `adb-`, carries `._tcp`,
  // and after a rename contains a space as well. Testing all four matters: a
  // name like `adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp` would
  // otherwise be offered as a phone "on the cable".
  function isUsbAddress(address) {
    return !address.includes(':')
      && !address.includes('._tcp')
      && !address.includes(' ')
      && !address.startsWith('adb-');
  }

  // Transport name -> current ip:port, from `adb mdns services`. Only fetched
  // when the cheap tests below have already failed, so the common case still
  // costs one `adb devices` and nothing else.
  let mdnsMap = new Map();

  // Bumped every time a row is forgotten. Anything that was already in flight
  // when that happened - a Connect that takes half a minute to answer - checks
  // its own generation before writing an address back, because repainting the
  // row is not the same as cancelling the work behind it.
  const forgetEpoch = new Map();
  // Serial behind each plain `ip:port` transport, as the phone itself reports
  // it (`getprop ro.serialno`), asked once per address while adb lists it.
  //
  // Why: a plain `ip:port` says nothing about who is behind it, and every
  // test in liveTransport below reads the transport NAME - which carries the
  // serial only on USB and on an mDNS transport. A phone the watcher brought
  // back at a new IP, or one added over USB that is now on Wi-Fi, would read
  // "Disconnected" while `adb devices` lists it. The watcher asks the phone
  // too. address -> { serial, retryAt }; `serial` is null while the phone has
  // not answered, retried after SERIAL_RETRY_MS.
  const serialByAddress = new Map();
  // Device ids already told this session that their Mirryl app is missing.
  const phoneAppMissingTold = new Set();
  const serialLookups = new Set();
  const SERIAL_RETRY_MS = 12000;

  function isPlainIpPort(address) {
    return /^\d[\d.]*:\d+$/.test(address);
  }

  function reportedSerial(address) {
    const known = serialByAddress.get(address);
    return known ? known.serial : null;
  }

  /**
   * Ask each newly listed `ip:port` transport for its serial, in the
   * background: the row turns green on the next 3 s poll rather than holding
   * this one up for a phone that may take a moment - or never answer.
   * Addresses adb no longer lists are forgotten, so a port reused by another
   * phone is asked again.
   */
  function learnTransportSerials() {
    const live = new Set(connectedAddresses());
    for (const address of Array.from(serialByAddress.keys())) {
      if (!live.has(address)) serialByAddress.delete(address);
    }
    for (const address of live) {
      if (!isPlainIpPort(address) || serialLookups.has(address)) continue;
      const known = serialByAddress.get(address);
      if (known && (known.serial || Date.now() < known.retryAt)) continue;
      serialLookups.add(address);
      callBackend('device_serial', { hostPort: address })
        .then((serial) => {
          // callBackend turns a failure into an 'Error: ...' string; a real
          // serial is one bare token, which is also what the backend enforces.
          const clean = typeof serial === 'string' && serial && !/\s/.test(serial) ? serial : null;
          serialByAddress.set(address, { serial: clean, retryAt: Date.now() + SERIAL_RETRY_MS });
        })
        .catch(() => {
          serialByAddress.set(address, { serial: null, retryAt: Date.now() + SERIAL_RETRY_MS });
        })
        .finally(() => { serialLookups.delete(address); });
    }
  }

  /**
   * The transport to actually use for a device, and which kind it is.
   *
   * The phone's Wireless-debugging port changes every time that setting is
   * re-enabled, so the saved ip:port goes stale as a matter of routine: match
   * on what identifies the phone - same IP on any port, or the serial -
   * preferring an exact hit.
   *
   * USB wins whenever both are up. It is the steadier link by some distance:
   * its address never changes (wireless debugging picks a new port every time
   * it restarts), it does not drop when the phone dozes, and it does not care
   * about the network at all.
   */
  function liveTransport(d) {
    // An address whose phone has already SAID who it is, and it is not this
    // row's phone, is off the table for every rule below - including the
    // address and same-IP ones. Without this, an IP the router hands to a
    // different phone (DHCP renewal, router reboot) would light this row up
    // as connected to someone else's phone, and syncDeviceAddresses would
    // write that address into the row. An address that has not answered yet
    // (null) is matched by the rules below, so a row without a serial loses
    // nothing.
    const foreign = (a) => !!d.serial && reportedSerial(a) !== null && reportedSerial(a) !== d.serial;
    const addresses = connectedAddresses().filter((a) => !foreign(a));
    if (d.serial) {
      const usb = addresses.find((a) => isUsbAddress(a) && a.includes(d.serial));
      if (usb) return { address: usb, kind: 'usb' };
    }
    if (addresses.includes(d.host_port)) return { address: d.host_port, kind: 'wifi' };
    const ip = (d.host_port || '').split(':')[0];
    const sameIp = ip && addresses.find((a) => a.startsWith(ip + ':'));
    if (sameIp) return { address: sameIp, kind: 'wifi' };
    const bySerial = d.serial && addresses.find((a) => a.includes(d.serial));
    if (bySerial) return { address: bySerial, kind: 'wifi' };
    // The serial the phone REPORTED over a plain `ip:port` transport (see
    // serialByAddress). This is what recognises a phone the watcher brought
    // back at a new IP, and one added over USB that turned up on Wi-Fi.
    if (d.serial) {
      const byReported = addresses.find((a) => reportedSerial(a) === d.serial);
      if (byReported) return { address: byReported, kind: 'wifi' };
    }
    // Last resort, and the one a REBOOT needs. The phone comes back under its
    // mDNS name only, with a brand-new port, so neither the saved ip:port nor
    // its IP appears in `adb devices` at all - and an entry saved without a
    // serial has nothing left to match on. Resolving the mDNS name to the
    // address it currently lives at is what closes that gap.
    if (ip && mdnsMap.size > 0) {
      const viaMdns = addresses.find((a) => {
        const resolved = mdnsMap.get(a);
        return resolved && resolved.split(':')[0] === ip;
      });
      if (viaMdns) return { address: viaMdns, kind: 'wifi' };
    }
    return { address: null, kind: null };
  }

  /**
   * Fill mdnsMap, but only when some device has nothing else to match on.
   *
   * `adb mdns services` is cheap (~46 ms) but it is still a second adb call on
   * a 3-second tick, and the overwhelmingly common case does not need it.
   */
  async function refreshMdnsIfNeeded() {
    const stranded = devices.some((d) => !findLiveAddress(d) && d.host_port);
    if (!stranded) { mdnsMap = new Map(); return; }
    const pairs = await callBackend('mdns_transports');
    if (Array.isArray(pairs)) mdnsMap = new Map(pairs);
  }

  /**
   * An mDNS transport name carries the phone's hardware serial:
   * `adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp`. When a device was saved
   * without one - manual pairing never captured it - this is where it can be
   * learned for free, no adb call needed.
   *
   * Without a serial a phone that reboots becomes unrecognisable.
   */
  async function learnMissingSerials() {
    for (const d of devices) {
      if (d.serial) continue;
      const address = findLiveAddress(d);
      const match = address && /^adb-([A-Za-z0-9]+)-/.exec(address);
      if (!match) continue;
      d.serial = match[1];
      await callBackend('update_device', { id: d.id, alias: null, hostPort: null, serial: match[1] });
    }
  }

  function findLiveAddress(d) {
    return liveTransport(d).address;
  }

  /**
   * The wireless addresses `adb disconnect` can actually drop for this row.
   *
   * With adb 37.0.0, `disconnect` matches a transport
   * by the serial adb LISTS it under, and appends `:5555` when no port is
   * given. So an mDNS transport can only be dropped by its name - and by its
   * name it cannot, because of the `:5555`. Resolving the name to the ip:port
   * behind it does NOT help either: that address is not what adb has the
   * transport registered as, and the command answers "no such device".
   *
   * Hence: only addresses that appear in `adb devices` AS `ip:port`. Anything
   * else is a link the app cannot end, and the dialog says so instead of
   * promising it.
   */
  function droppableAddresses(d) {
    const listed = connectedAddresses();
    const live = liveWirelessAddress(d);
    return [d.host_port, live]
      .filter((a) => a && isPlainIpPort(a) && listed.includes(a))
      // The saved address can have been recycled by DHCP: if the phone there
      // has said it is someone else, dropping it in this row's name would cut
      // a different phone. Same rule as liveWirelessAddress; an address that
      // has not answered yet (null) is kept.
      .filter((a) => !d.serial || reportedSerial(a) === null || reportedSerial(a) === d.serial)
      .filter((a, i, all) => all.indexOf(a) === i);
  }

  /**
   * Wireless transports of this phone that adb lists and NOBODY can drop:
   * the mDNS names adb opened by itself. `adb disconnect` cannot end one
   * (measured - see droppableAddresses), so when one of these is up, the
   * phone stays on Wi-Fi no matter what the app does, and the dialog has to
   * say so. adb listing the same phone twice - by name and by ip:port - is
   * the normal state, not an edge case.
   */
  function undroppableTransports(d) {
    if (!d.serial) return [];
    return connectedAddresses().filter(
      (a) => !isPlainIpPort(a) && !isUsbAddress(a)
        && (a.includes(d.serial) || reportedSerial(a) === d.serial)
    );
  }

  /**
   * The `ip:port` this phone is reachable at right now, or null.
   *
   * Not the same as `findLiveAddress`: that one returns the transport in use,
   * which for a phone adb found by itself is an mDNS NAME - and a name is not
   * something `adb disconnect` understands. So: the live transport when it is
   * already an ip:port, else the name resolved through `adb mdns services`,
   * else a second transport of the same phone (adb lists a phone twice often
   * enough). "Forget Wi-Fi address" needs this one: without it the dialog
   * would promise to drop the connection and drop nothing.
   */
  function liveWirelessAddress(d) {
    const live = findLiveAddress(d);
    // Same rule as syncDeviceAddresses: a row WITH a serial only owns an
    // ip:port once the phone there has said so. Otherwise a recycled IP (the
    // row's old address handed to someone else's phone by DHCP) could be
    // disconnected in this row's name.
    if (live && isPlainIpPort(live)) {
      if (d.serial && reportedSerial(live) !== d.serial) return null;
      return live;
    }
    if (live) {
      const resolved = mdnsMap.get(live);
      if (resolved && isPlainIpPort(resolved)) return resolved;
    }
    if (d.serial) {
      const twin = connectedAddresses().find((a) => isPlainIpPort(a) && reportedSerial(a) === d.serial);
      if (twin) return twin;
    }
    return null;
  }

  // ---------- Phones on the cable that Mirryl does not know yet ----------
  // A USB device needs NO pairing: plugging the cable in and accepting "Allow
  // USB debugging?" on the phone is the whole authorisation. So a cabled phone
  // can be added in one click, skipping the two-step wizard entirely - which is
  // also why a device added this way has an empty host_port (see the Device
  // struct in main.rs): it has no wireless address, and does not need one.
  //
  // Read out of the `adb devices -l` text the status poll already fetched, so
  // noticing this costs no extra adb call.
  function usbCandidates() {
    const knownSerials = new Set(devices.map((d) => d.serial).filter(Boolean));
    const liveAddresses = new Set(devices.map((d) => findLiveAddress(d)).filter(Boolean));
    return lastStatusText
      .split('\n')
      .map((line) => ({ line, row: parseAdbLine(line) }))
      .filter(({ row }) => row && row.state === 'device' && isUsbAddress(row.address))
      .map(({ line, row }) => {
        const model = (line.trim().split(/\s+/).find((p) => p.startsWith('model:')) || '').slice(6);
        return { serial: row.address, model: model ? model.replace(/_/g, ' ') : null };
      })
      // Skip anything already in the list. Serial is the reliable test; the
      // live-address one also covers an entry saved without a serial, which
      // would otherwise be offered as if it were new.
      .filter((c) => !knownSerials.has(c.serial) && !liveAddresses.has(c.serial));
  }

  /**
   * The offer lives where the user is already looking, not in a banner.
   *
   * With an empty list that is the empty state itself - it stops saying "no
   * devices yet" and says which phone is on the cable. With devices already
   * listed it is a row at the end of the list. Stacked as a third notice
   * above the list it would put two near-identical panels next to each other
   * and make good news look like another warning to dismiss.
   */
  function renderUsbNudge() {
    const title = document.getElementById('emptyTitle');
    const body = document.getElementById('emptyBody');
    const wizardBtn = document.getElementById('addDeviceBtnEmpty');
    const addBtn = document.getElementById('usbNudgeBtn');
    const candidate = usbCandidates()[0];

    // Only the empty state is driven here; the row case is built in
    // renderDeviceList, which owns the list's markup.
    if (devices.length > 0) return;

    if (!candidate) {
      title.textContent = 'No devices yet';
      body.textContent = 'Add your first Android device to connect and control it over your local network.';
      wizardBtn.style.display = '';
      addBtn.style.display = 'none';
      return;
    }
    addBtn.dataset.serial = candidate.serial;
    addBtn.dataset.model = candidate.model || '';
    title.textContent = `${candidate.model || candidate.serial} is connected by cable`;
    body.textContent = 'Add it and you can mirror it right away — over USB there is nothing to pair.';
    addBtn.textContent = 'Add device';
    wizardBtn.style.display = 'none';
    addBtn.style.display = '';
  }

  /** Shared by the empty-state button and the offer row at the end of the list. */
  async function addUsbDevice(serial, model, btn) {
    if (!serial) return;
    const original = btn ? btn.textContent : null;
    if (btn) { btn.disabled = true; btn.textContent = 'Adding...'; }
    // No host_port on purpose: this phone has no wireless address, and inventing
    // one would make the row show an address that does not work.
    const result = await callBackend('add_device', {
      alias: model || 'Device (USB)',
      hostPort: '',
      serial,
    });
    if (btn) { btn.disabled = false; btn.textContent = original; }
    // A string is the backend's refusal (devices.json could not be written):
    // shown as it is rather than a button that did nothing.
    if (typeof result === 'string') {
      showNotice(/devices\.json/i.test(result) ? 'Could not add it' : 'Not saved', result);
      return;
    }
    await loadDevices();
  }

  document.getElementById('usbNudgeBtn').addEventListener('click', (e) => {
    const btn = e.target;
    addUsbDevice(btn.dataset.serial, btn.dataset.model, btn);
  });

  /**
   * The line under the alias: where this phone actually is.
   *
   * The live wireless address when there is one, the saved address when
   * there is one, and the cable wording only for a row that really has
   * nothing else - "USB" under a Wi-Fi icon, after "Forget Wi-Fi address",
   * would be wrong.
   */
  function addressLine(d, kind) {
    if (kind === 'wifi') {
      const live = liveWirelessAddress(d);
      if (live) return live;
    }
    // On the cable the cable is the answer, forgotten address or not - that is
    // the state this whole feature exists to produce, and it should read like
    // any other phone on a cable.
    if (kind === 'usb') return d.serial ? 'USB · ' + d.serial : 'USB';
    // On Wi-Fi through a link we cannot name (adb's own mDNS transport): say
    // Wi-Fi.
    if (kind === 'wifi' && findLiveAddress(d)) return d.serial ? 'Wi-Fi · ' + d.serial : 'Wi-Fi';
    if (d.host_port) return d.host_port;
    if (d.cable_only) return d.serial ? 'Wi-Fi address forgotten · ' + d.serial : 'Wi-Fi address forgotten';
    return d.serial ? 'USB · ' + d.serial : 'USB';
  }

  function deviceStatus(d) {
    // Same reasoning as the label: the engine could not be mirroring a phone
    // that is not there.
    if (openMirrors.has(String(d.id))) {
      lastSeenConnected.set(d.id, Date.now());
      noteSeen(d.id);
      connectingUntil.delete(d.id);
      return 'connected';
    }
    if (findLiveAddress(d)) {
      lastSeenConnected.set(d.id, Date.now());
      noteSeen(d.id);
      connectingUntil.delete(d.id);
      return 'connected';
    }
    // An attempt in flight outranks "reconnecting"/"disconnected": the user
    // just asked for this, and saying so is what stops them asking again.
    if (isConnecting(d)) return 'connecting';
    // "Reconnecting..." is a promise that something is retrying. For a phone
    // that only exists on a cable that is nothing: the watcher only handles
    // wireless, and there is no address to dial. Unplugged is just gone.
    if (!d.host_port) return 'disconnected';
    const seen = lastSeenConnected.get(d.id);
    if (seen && Date.now() - seen < RECONNECT_GRACE_MS) return 'reconnecting';
    return 'disconnected';
  }

  // Keep the saved port in step with the phone's current one, so that clicking
  // a row mirrors right away instead of first failing on a stale port. Only
  // real ip:port addresses are persisted - never the mDNS transport name.
  async function syncDeviceAddresses() {
    for (const d of devices) {
      // "Forget Wi-Fi address" asked for exactly this: no address written
      // back into this row. Without the guard the next poll undoes it.
      if (d.cable_only) continue;
      const live = findLiveAddress(d);
      if (!live) continue;
      // The mDNS NAME is not an address and must never be saved as one - but
      // mDNS knows the address behind it, and THAT one is worth saving. Without
      // this the row keeps showing the port the phone had before it rebooted:
      // a dead address, displayed as if it were current.
      // adb can list the same phone twice - under its mDNS name (its own
      // auto-connect) and at the ip:port the watcher dialled. The name wins
      // the match above, but it is the ip:port, known by the serial the phone
      // reported there, that is worth writing down.
      const twin = d.serial && connectedAddresses().find((a) => isPlainIpPort(a) && reportedSerial(a) === d.serial);
      let resolved;
      if (isPlainIpPort(live)) {
        // A row WITH a serial waits for the phone at that address to confirm
        // it (one poll, normally) before the address is written down: a
        // same-IP match alone is how a neighbour's phone at a recycled IP
        // would get saved as ours. Rows without a serial match by IP alone.
        if (d.serial && reportedSerial(live) !== d.serial) continue;
        resolved = live;
      } else {
        // The twin is already vouched for by the serial it reported; the
        // mDNS name carries the serial itself, and its address comes from adb.
        resolved = twin || mdnsMap.get(live);
      }
      if (resolved && isPlainIpPort(resolved) && resolved !== d.host_port) {
        d.host_port = resolved;
        await callBackend('update_device', { id: d.id, alias: null, hostPort: resolved, serial: null });
      }
    }
  }

  // Click anywhere on a device's icon/name/status/IP to connect (if needed)
  // and open the mirror window for that specific device - this is the whole
  // point of seeing it listed here, not just a status readout.
  async function connectAndMirror(d) {
    // Already in a tab: go to it (or to its window) instead of opening a
    // second one. Checked before anything else so the row does not flash
    // "Connecting..." for a phone that is already up and mirroring.
    const existing = mirrorTabs.get(String(d.id));
    if (existing) {
      if (existing.state === 'window') {
        callBackend('mirror_command', { key: existing.key, command: 'window' });
      } else if (existing.state === 'closed') {
        reopenMirror(existing);
      } else {
        activateTab(existing.key);
      }
      return;
    }
    // The guard lives in a Map keyed by device id, NOT on the DOM element: the
    // 3-second status poll rebuilds the whole list, and a flag on the element
    // would go with it and let every further click start another engine.
    if (isConnecting(d)) return;
    // An engine with no tab: one whose tab was just closed and is still on its
    // way out, or one whose window outlived an earlier run of the app.
    if (openMirrors.has(String(d.id))) {
      const focused = await callBackend('focus_mirror', { key: String(d.id) });
      if (focused === 'focused') return;
      if (focused === 'not-found') {
        showNotice(
          'Still closing',
          `The last mirror of "${d.alias}" is still closing. Try again in a moment.`
        );
        return;
      }
      // 'gone': the window was closed since the last poll, so this click is a
      // request to open it again. Fall through.
      openMirrors.delete(String(d.id));
    }
    // A device with no wireless address lives on the cable. If adb cannot see
    // it, there is nothing to connect TO - `adb connect` needs an ip:port - so
    // say what is actually wrong instead of launching the engine at nothing.
    if (!d.host_port && !findLiveAddress(d)) {
      showNotice(
        'Cable not connected',
        `"${d.alias}" was added over USB and Mirryl cannot see it right now. Plug the cable back in, then try again.`
      );
      return;
    }
    connectingUntil.set(d.id, Date.now() + CONNECT_TIMEOUT_MS);
    // The tab opens at once and says it is connecting: the connect below can
    // take half a minute when the phone is slow to answer.
    const tab = openMirrorTab(d);
    renderDeviceList();
    // What "forgotten" means for a connect that is already under way: the
    // answer can take half a minute, and "Forget Wi-Fi address" pressed in the
    // meantime must win. Read before the wait, checked after it.
    const epochAtStart = forgetEpoch.get(d.id) || 0;
    if (d.host_port && deviceStatus(d) !== 'connected') {
      const res = await callBackend('connect_and_grant', { hostPort: d.host_port, serial: d.serial || null });
      if ((forgetEpoch.get(d.id) || 0) !== epochAtStart) {
        connectingUntil.delete(d.id);
        if (mirrorTabs.get(tab.key) === tab) removeMirrorTab(tab);
        renderDeviceList();
        return;
      }
      // The saved port goes stale when Wireless debugging is toggled or the
      // phone reboots; the backend re-resolves the current one. Persist it so
      // the row and future connects use the up-to-date port.
      if (res && typeof res === 'object' && res.host_port && res.host_port !== d.host_port) {
        await callBackend('update_device', { id: d.id, alias: null, hostPort: res.host_port, serial: null });
        d.host_port = res.host_port;
      }
      if (res && typeof res === 'object' && res.notice) {
        // Not awaited: the mirror should open regardless; the dialog queue
        // shows it when the user gets to it.
        showNotice('About this phone', res.notice);
      }
      // A saved phone whose Mirryl app is gone (uninstalled on the phone):
      // the mirror still works, but the phone will not come back by itself.
      // Said once per phone per session, not on every connect. Not awaited,
      // like the notice above.
      if (res && typeof res === 'object' && res.phone_app === 'missing' && !phoneAppMissingTold.has(d.id)) {
        phoneAppMissingTold.add(d.id);
        showNotice(
          'About this phone',
          "The Mirryl app isn't installed on this phone, so it won't turn Wireless debugging back on "
            + 'by itself after a reboot. To install it, open this phone\'s page (the ⋮ on its row) '
            + 'and press "Reinstall the Mirryl app".'
        );
      }
    }
    // Mirror whatever address adb actually has a live transport for, not the
    // saved one. After Wireless debugging restarts, the phone frequently comes
    // back only under its mDNS name (`adb-<serial>-...._adb-tls-connect._tcp`)
    // and the saved ip:port is dead - `adb -s` accepts either, but only the
    // live one exists, so using the saved one here would fail on a row that
    // correctly reads "Connected".
    const target = findLiveAddress(d) || d.host_port;
    // deviceKey is what the one-window-per-device rule is keyed on: the row's
    // identity, not the address, which differs between USB and Wi-Fi and gets a
    // new port every time Wireless debugging restarts.
    // wifiAddress is the saved ip:port, sent even when mirroring over USB: it is
    // what lets the mirror window offer "switch to Wi-Fi" if the cable is pulled.
    const mirrorResult = await callBackend('start_mirror', {
      hostPort: target,
      deviceKey: String(d.id),
      wifiAddress: d.host_port || null,
      alias: d.alias || null,
    });
    // start_mirror returns either a success line or the reason it could not
    // start: engine missing, no connected device, or a spawn error - shown,
    // or every one of those failures would look like a click that did nothing.
    // The backend has the last word on the one-window rule: it is the only side
    // that knows whether the engine is still running. Reaching this means our
    // Set was behind, so catch up rather than treating it as a failure - it is
    // not one, and an error dialog here would be a lie.
    if (typeof mirrorResult === 'string' && mirrorResult.startsWith('Already open')) {
      openMirrors.add(String(d.id));
      connectingUntil.delete(d.id);
      if (mirrorTabs.get(tab.key) === tab) removeMirrorTab(tab);
      renderDeviceList();
      return;
    }
    // start_mirror answers "Opening the mirror..." when the engine started.
    const mirrorStarted = typeof mirrorResult === 'string'
      && mirrorResult.startsWith('Opening');
    if (mirrorStarted) {
      // Locked from this instant, not from the next poll: the whole point is
      // that the second click - which comes long before 3 s are up - finds it.
      openMirrors.add(String(d.id));
      // Record the link it opened on straight away, so the row shows the right
      // icon in the second or two before the engine gets to report it.
      const launchedKind = liveTransport(d).kind;
      if (launchedKind) mirrorKind.set(String(d.id), launchedKind);
      // Deliberately does NOT clear the connecting state here. start_mirror
      // only LAUNCHES the engine - it is a detached process, so it returns in
      // milliseconds, long before the mirror is actually up. Clearing on this
      // reply would make "Connecting..." flash by and hand the row straight
      // back to the user.
      // It clears on its own: deviceStatus drops it the moment the device shows
      // up as connected, and the 30 s ceiling covers an attempt that never does.
      renderDeviceList();
    } else {
      // A failure is final, so release immediately - there is nothing left to
      // wait for and the user should be able to retry at once.
      connectingUntil.delete(d.id);
      // The failure reason is shown on the row itself and kept for 5 s. Written
      // straight to the live element rather than through renderDeviceList so
      // the next 3-second poll does not wipe it before it can be read; the poll
      // rebuilds afterwards and clears it on its own.
      failedUntil.set(d.id, { until: Date.now() + 5000, reason: String(mirrorResult ?? '') });
      renderDeviceList();
      setTimeout(() => {
        failedUntil.delete(d.id);
        renderDeviceList();
      }, 5000);
      // Said in the tab that was opened for it, not left in a tooltip nobody
      // hovers. If that tab was closed meanwhile, in a dialog.
      if (mirrorTabs.get(tab.key) === tab) {
        markMirrorClosed(tab, String(mirrorResult ?? 'Unknown error.'));
      } else {
        showNotice("Couldn't open the mirror", String(mirrorResult ?? 'Unknown error.'));
      }
    }
  }

  // Only one device's "more actions" menu is open at a time, tracked here so
  // it survives a renderDeviceList() rebuild (e.g. from the 3s status poll).
  let openMenuDeviceId = null;

  /**
   * The little tile on the left of each row says HOW the phone is attached and
   * how well, in one glance:
   *   - USB: the USB trident. No signal-strength idea applies to a cable, so
   *     there is one drawing; if it is there at all, the link is solid.
   *   - Wi-Fi: the same three arcs at three fill levels, so the states differ
   *     by shape and not only by colour - a red and a grey dot look alike to
   *     anyone who cannot separate those two hues.
   *       connected    -> all three arcs
   *       reconnecting -> inner arc only, the outer ones dimmed
   *       disconnected -> arcs dimmed with a slash through them
   */
  function transportIcon(kind, status, color) {
    if (kind === 'usb') {
      return `<svg width="20" height="20" viewBox="0 0 24 24" fill="none"
        stroke="${color}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="20.5" r="1.6" fill="${color}"/>
        <path d="M12 19V5"/><path d="M12 5l-2.5 3.5h5L12 5z" fill="${color}"/>
        <path d="M12 12.5l3.8-2.6V7.6"/><circle cx="15.8" cy="6.6" r="1.3" fill="${color}"/>
        <path d="M12 15.4l-3.8-2.6v-1.6"/><rect x="6.9" y="9.5" width="2.6" height="2.2" fill="${color}"/>
      </svg>`;
    }
    const dim = 'var(--neutral)';
    const outer = status === 'connected' ? color : dim;
    const middle = status === 'connected' ? color : dim;
    const inner = status === 'disconnected' ? dim : color;
    return `<svg width="20" height="20" viewBox="0 0 24 24" fill="none"
      stroke-width="1.9" stroke-linecap="round">
      <path d="M3.5 9.5a13 13 0 0 1 17 0" stroke="${outer}" opacity="${outer === dim ? 0.45 : 1}"/>
      <path d="M6.5 13a8.5 8.5 0 0 1 11 0" stroke="${middle}" opacity="${middle === dim ? 0.45 : 1}"/>
      <path d="M9.5 16.5a4 4 0 0 1 5 0" stroke="${inner}"/>
      <circle cx="12" cy="19.5" r="1.3" fill="${inner}"/>
      ${status === 'disconnected'
        ? `<path d="M4 4l16 16" stroke="${color}" stroke-width="1.9"/>`
        : ''}
    </svg>`;
  }

  function renderDeviceList() {
    const list = document.getElementById('dmDeviceList');
    list.innerHTML = '';
    const shown = visibleDevices();
    shown.forEach((d, idx) => {
      const status = deviceStatus(d);
      const meta = STATUS_META[status];
      // While a mirror is open the row follows THE MIRROR's link, not the
      // phone's best available one - see mirrorKind. Otherwise: remembered so a
      // device that just dropped still shows the icon of the link it was on,
      // instead of silently falling back to the Wi-Fi drawing.
      const linkKind = mirrorKind.get(String(d.id))
        || liveTransport(d).kind || lastTransportKind.get(d.id) || 'wifi';
      if (liveTransport(d).kind) lastTransportKind.set(d.id, linkKind);
      const kind = linkKind;
      const failed = failedUntil.get(d.id);
      // Saying "Mirroring" is what makes the one-window rule visible: without it
      // a click that (correctly) opens nothing new looks like a click that did
      // nothing at all.
      // An open mirror is DIRECT evidence the phone is there: our own engine is
      // streaming from it right now. That beats the adb table, which after a
      // reboot can fail to match a device it is mirroring - and a row reading
      // "Disconnected" next to a live picture is the app calling itself a liar.
      const mirroring = openMirrors.has(String(d.id)) || mirrorTabs.has(String(d.id));
      const label = failed ? 'Mirror failed' : mirroring ? 'Mirroring' : meta.label;
      const labelColor = failed ? 'var(--danger)' : `var(${meta.varName})`;
      const iconColor = `var(${meta.varName})`;
      const menuOpen = openMenuDeviceId === d.id;
      const selected = dmSelectedIds.has(d.id);
      // Only while sorting by it: otherwise it is one more line of text on
      // every row for something nobody asked to see.
      const seenAt = lastSeenStored[d.id];
      const seenNote = dmSortMode === 'last-seen' && status !== 'connected'
        ? ' · ' + (seenAt ? 'seen ' + agoText(seenAt) : 'not seen yet')
        : '';
      const row = document.createElement('div');
      row.className = 'device-row' + (dmSelectionMode ? ' selection-mode' : '');
      // Belt and braces with the JS guard: while an attempt is in flight the
      // clickable area stops taking pointer events at all. The button gets
      // `disabled` for free, but this whole region is also a Connect trigger
      // and had nothing stopping the click from reaching it. In selection
      // mode the row is a checkbox toggle instead, regardless of status.
      const iconTitle = kind === 'usb' ? 'Connected by USB' : 'Wi-Fi';
      row.innerHTML = `
        <div class="device-open"${status === 'connecting' && !dmSelectionMode ? ' style="pointer-events:none;"' : ''}
             title="${dmSelectionMode ? 'Click to select' : mirroring ? 'Already open - click to go to its tab' : 'Click to mirror this device'}">
          ${dmSelectionMode ? `<input type="checkbox" class="device-select-checkbox" aria-label="Select ${escapeHtml(d.alias)}" ${selected ? 'checked' : ''}>` : ''}
          <div class="device-icon" title="${iconTitle}">${transportIcon(kind, status, iconColor)}</div>
          <div class="device-info">
            <div class="device-alias-row">
              <div class="device-alias">${escapeHtml(d.alias)}</div>
              <div class="device-status">
                <div class="status-dot" style="background:${labelColor};"></div>
                <div class="status-label" style="color:${labelColor};"
                     title="${failed ? escapeHtml(failed.reason) : ''}">${label}</div>
              </div>
            </div>
            <div class="device-ip">${escapeHtml(addressLine(d, kind) + seenNote)}</div>
          </div>
        </div>
        <div class="device-actions">
          ${dmSelectionMode ? '' : status === 'connecting'
            ? '<button class="connect-btn" disabled>Connecting...</button>'
            // Connected means there is nothing left to connect - what remains
            // is opening the picture (the whole left region triggers it too).
            // "Show" rather than "Mirror" once it is open: clicking then goes
            // to its tab (or window), it does not open a second.
            : status === 'connected'
              ? `<button class="connect-btn">${mirroring ? 'Show' : 'Mirror'}</button>`
              // No "Connect" for a phone that only exists on a cable: there is
              // nothing to connect TO (adb connect needs an ip:port), so the
              // button could only ever say no. Plugging the cable back in is
              // the entire action, and the row goes green on its own.
              : d.host_port ? '<button class="connect-btn">Connect</button>' : ''}
          ${dmSelectionMode ? '' : `<div class="device-menu-wrap">
            <button class="device-menu-btn" aria-label="Open this phone's page" title="Open this phone's page: its apps, its info and everything that can be done to it">&#8942;</button>
            <div class="device-menu" style="display:${menuOpen ? 'block' : 'none'}; top:44px;">
              <button class="mirror-menu-btn">${mirroring ? 'Show' : 'Mirror'}</button>
              <button class="apps-btn">Apps…</button>
              <button class="phone-action send-file-btn">Send file</button>
              <button class="phone-action restart-btn">Restart device</button>
            </div>
          </div>`}
        </div>`;

      const openEl = row.querySelector('.device-open');
      openEl.addEventListener('click', (e) => {
        // "Edit alias" swaps the alias text for an <input> that lives INSIDE
        // this row, so clicking into the field to type would bubble up here
        // and start mirroring. Anything that is part of the editor is not a
        // request to open the device.
        if (e.target.closest('.device-alias-input')) return;
        // Selection mode: the whole row toggles the checkbox instead of
        // connecting/mirroring - the checkbox's own listener handles a click
        // that landed directly on it, this covers the rest of the row. Ticking
        // a phone still never mirrors it, which is the point.
        if (dmSelectionMode) {
          if (e.target.classList.contains('device-select-checkbox')) return;
          toggleSelection(d.id);
          return;
        }
        connectAndMirror(d);
      });

      row.querySelector('.device-select-checkbox')?.addEventListener('change', () => toggleSelection(d.id));

      list.appendChild(row);
      if (dmSelectionMode) return; // Nothing below exists in this mode's markup.

      row.querySelector('.connect-btn')?.addEventListener('click', () => connectAndMirror(d));

      const menuEl = row.querySelector('.device-menu');
      const closeMenu = () => { openMenuDeviceId = null; menuEl.style.display = 'none'; };

      // Up or down is decided by the room there is, measured now that the row
      // is in the list: the sidebar's scroll box cuts off what sticks out
      // upward, while downward overflow just makes the list scrollable, so up
      // is only for when down does not fit and up does.
      if (menuOpen) {
        const btnRect = row.querySelector('.device-menu-btn').getBoundingClientRect();
        const box = (row.closest('.sidebar-scroll') || document.documentElement).getBoundingClientRect();
        const need = menuEl.offsetHeight + 8;
        const below = Math.min(box.bottom, window.innerHeight) - btnRect.bottom;
        const above = btnRect.top - Math.max(box.top, 0);
        if (below < need && above >= need) {
          menuEl.style.top = '';
          menuEl.style.bottom = '44px';
        }
      }

      // The ⋮ opens the phone's own page; the right-click below keeps a short
      // menu of the quick actions, to use without leaving the screen in front.
      row.querySelector('.device-menu-btn').addEventListener('click', (e) => {
        e.stopPropagation();
        openMenuDeviceId = null;
        openDevicePage(d, 'apps');
      });

      row.querySelector('.mirror-menu-btn')?.addEventListener('click', () => {
        closeMenu();
        connectAndMirror(d);
      });

      // Same menu as the ⋮ button, just a second way to reach it - right-click
      // opens exactly what that button opens, not a competing menu with its own
      // action list to keep in sync.
      row.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        e.stopPropagation();
        openMenuDeviceId = d.id;
        renderDeviceList();
      });

      row.querySelector('.send-file-btn')?.addEventListener('click', async (e) => {
        const btn = e.target;
        const original = btn.textContent;
        const path = await callBackend('pick_file');
        if (!path) {
          closeMenu();
          return;
        }
        btn.disabled = true;
        btn.textContent = 'Sending...';
        const result = await callBackend('push_file', { hostPort: findLiveAddress(d) || d.host_port, localPath: path });
        const failed = /^Error:/.test(result);
        btn.textContent = failed ? 'Failed' : 'Sent!';
        setTimeout(() => {
          btn.textContent = original;
          btn.disabled = false;
          closeMenu();
        }, 2000);
        // "Sent!" on its own would leave the user searching the phone for the
        // file. The backend's message names the exact path it went to, so say
        // it; and on failure say why.
        showNotice(failed ? "Couldn't send the file" : 'File sent', result);
      });

      row.querySelector('.restart-btn')?.addEventListener('click', async () => {
        closeMenu();
        const confirmed = await showConfirm(
          'Restart this device?',
          `"${d.alias}" will restart. Anything unsaved on the phone is lost, and it drops off for about half a minute.`,
          'Restart'
        );
        if (!confirmed) return;
        await callBackend('device_reboot', { target: findLiveAddress(d) || d.host_port, alias: d.alias });
      });

      // The menu's "Apps…" line.
      row.querySelector('.apps-btn')?.addEventListener('click', () => {
        closeMenu();
        openDevicePage(d, 'apps');
      });
    });

    // Cabled phones that are not in the list, offered as the last row -
    // where a new device would appear anyway. Muted and dashed so it reads
    // as "available", not as something that is already set up. Not while
    // selecting: no checkbox, so no part in a bulk action. One offer at a
    // time.
    const candidates = dmSelectionMode ? [] : usbCandidates();
    candidates.slice(0, 1).forEach((candidate) => {
      const name = escapeHtml(candidate.model || candidate.serial);
      const icon = transportIcon('usb', 'disconnected', 'var(--textMuted)');
      const text = 'Connected by cable — not added yet';
      const button = '<button class="offer-add">Add device</button>';
      const onClick = (e) => addUsbDevice(candidate.serial, candidate.model, e.target);
      const offer = document.createElement('div');
      offer.className = 'device-row offer-row';
      offer.innerHTML = `
        <div class="device-open" style="cursor:default;">
          <div class="device-icon">${icon}</div>
          <div class="device-info">
            <div class="device-alias-row">
              <div class="device-alias">${name}</div>
            </div>
            <div class="device-ip">${text}</div>
          </div>
        </div>
        <div class="device-actions">${button}</div>`;
      offer.querySelector('button').addEventListener('click', onClick);
      list.appendChild(offer);
    });
    // The open phone page shows the same state as its row.
    try { renderDevicePageHead(); } catch (_) { /* first render, before the page code ran */ }
  }

  document.addEventListener('click', (e) => {
    if (openMenuDeviceId !== null && !e.target.closest('.device-menu-wrap')) {
      openMenuDeviceId = null;
      renderDeviceList();
    }
  });

  function toggleSelection(id) {
    if (dmSelectedIds.has(id)) dmSelectedIds.delete(id); else dmSelectedIds.add(id);
    renderDeviceList();
    updateSelectionBar();
  }

  /** The checked devices, in list order. */
  function selectedDevices() {
    return devices.filter((d) => dmSelectedIds.has(d.id));
  }

  function updateSelectionBar() {
    const n = dmSelectedIds.size;
    const on = multiDevice() && dmSelectionMode;
    document.getElementById('dmSelectionBar').style.display = on ? 'flex' : 'none';
    // One or the other, never both: Select opens the bar, the bar's Done
    // closes it and brings Select back.
    document.getElementById('dmSelectModeBtn').style.display = on ? 'none' : '';
    document.getElementById('dmSelectionCount').textContent = `${n} selected`;
    // Nothing ticked, nothing to act on - better a dead button than a
    // "Restart 0 devices?" dialog.
    for (const id of ['dmBulkSendFileBtn', 'dmBulkRestartBtn', 'dmSelectionRemoveBtn', 'dmBulkInstallBtn', 'dmBulkProfileBtn', 'dmBulkExportBtn']) {
      document.getElementById(id).disabled = n === 0;
    }
    // The select-all box speaks for the rows on screen: ticked when all of
    // them are, half-ticked when some are.
    const shown = visibleDevices();
    const shownSelected = shown.filter((d) => dmSelectedIds.has(d.id)).length;
    const all = document.getElementById('dmSelectAll');
    all.checked = shown.length > 0 && shownSelected === shown.length;
    all.indeterminate = shownSelected > 0 && shownSelected < shown.length;
  }

  document.getElementById('dmSelectModeBtn').addEventListener('click', () => {
    dmSelectionMode = true;
    dmSelectedIds = new Set();
    renderDeviceList();
    updateSelectionBar();
  });

  document.getElementById('dmSelectAll').addEventListener('change', (e) => {
    const shown = visibleDevices();
    if (e.target.checked) shown.forEach((d) => dmSelectedIds.add(d.id));
    else shown.forEach((d) => dmSelectedIds.delete(d.id));
    renderDeviceList();
    updateSelectionBar();
  });

  document.getElementById('dmSearchInput').addEventListener('input', (e) => {
    dmSearchQuery = e.target.value;
    renderDeviceList();
    updateSelectionBar();
  });
  document.getElementById('dmStatusFilter').addEventListener('change', (e) => {
    dmStatusFilter = e.target.value;
    renderDeviceList();
    updateSelectionBar();
  });
  document.getElementById('dmSortSelect').addEventListener('change', (e) => {
    dmSortMode = e.target.value;
    renderDeviceList();
  });

  // Bulk actions act on the CHECKED devices - including any a filter is
  // hiding right now. The filter is for finding phones; the tick is what
  // decides who gets the action, and the confirmation names every one.
  document.getElementById('dmBulkSendFileBtn').addEventListener('click', async (e) => {
    const targets = selectedDevices();
    if (targets.length === 0) return;
    const btn = e.currentTarget;
    const path = await callBackend('pick_file');
    if (!path) return;
    btn.disabled = true;
    const lines = [];
    let sent = 0;
    for (const d of targets) {
      const hostPort = findLiveAddress(d) || d.host_port;
      if (!hostPort) { lines.push(`${d.alias}: not connected, skipped`); continue; }
      const result = await callBackend('push_file', { hostPort, localPath: path });
      if (/^Error:/.test(result)) lines.push(`${d.alias}: ${result}`);
      else { sent++; lines.push(`${d.alias}: sent`); }
    }
    btn.disabled = false;
    // One summary for the lot, so a phone that failed does not hide among
    // the ones that worked.
    showNotice(
      sent === targets.length ? 'File sent' : `File sent to ${sent} of ${targets.length}`,
      lines.join('\n'));
  });

  document.getElementById('dmBulkRestartBtn').addEventListener('click', async () => {
    const targets = selectedDevices();
    if (targets.length === 0) return;
    const reachable = targets.filter((d) => findLiveAddress(d) || d.host_port);
    const skipped = targets.filter((d) => !reachable.includes(d));
    if (reachable.length === 0) {
      showNotice('Nothing to restart', 'None of the selected devices is connected.');
      return;
    }
    const confirmed = await showConfirm(
      `Restart ${reachable.length} device${reachable.length === 1 ? '' : 's'}?`,
      `${reachable.map((d) => d.alias).join(', ')} will restart. Anything unsaved on each phone is lost, and each one drops off for about half a minute.`
        + (skipped.length ? ` ${skipped.map((d) => d.alias).join(', ')} ${skipped.length === 1 ? 'is' : 'are'} not connected and will be left alone.` : ''),
      'Restart'
    );
    if (!confirmed) return;
    for (const d of reachable) {
      await callBackend('device_reboot', { target: findLiveAddress(d) || d.host_port, alias: d.alias });
    }
  });

  document.getElementById('dmSelectionCancelBtn').addEventListener('click', () => {
    dmSelectionMode = false;
    dmSelectedIds = new Set();
    renderDeviceList();
    updateSelectionBar();
  });

  document.getElementById('dmSelectionRemoveBtn').addEventListener('click', async () => {
    const ids = Array.from(dmSelectedIds);
    if (ids.length === 0) return;
    const names = devices.filter((d) => ids.includes(d.id)).map((d) => d.alias).join(', ');
    const confirmed = await showConfirm(
      `Remove ${ids.length} device${ids.length === 1 ? '' : 's'}?`,
      `${names} will be removed from Mirryl. You can add them again later by pairing each once more.`,
      'Remove',
      'Also delete their saved files'
    );
    if (!confirmed) return;
    const deleteFolders = document.getElementById('confirmCheckbox').checked;
    for (const id of ids) {
      if (deleteFolders) {
        const alias = devices.find((d) => d.id === id)?.alias;
        if (alias) await callBackend('delete_tools_folder', { alias });
      }
      await callBackend('remove_device', { id });
      forgetSeen(id);
    }
    dmSelectionMode = false;
    dmSelectedIds = new Set();
    await loadDevices();
  });

  function startEditAlias(row, d) {
    const aliasEl = row.querySelector('.device-alias');
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'device-alias-input';
    input.value = d.alias;
    aliasEl.replaceWith(input);
    input.focus();
    input.select();

    let done = false;
    const save = async () => {
      if (done) return;
      done = true;
      const newAlias = input.value.trim();
      if (newAlias && newAlias !== d.alias) {
        await callBackend('update_device', { id: d.id, alias: newAlias, hostPort: null, serial: null });
      }
      await loadDevices();
    };
    input.addEventListener('blur', save);
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') input.blur();
      if (e.key === 'Escape') { done = true; renderDeviceList(); }
    });
  }

  function escapeHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }

  function renderDeviceManager() {
    const isFilled = devices.length > 0;
    document.getElementById('dmEmptyState').style.display = isFilled ? 'none' : '';
    document.getElementById('dmDeviceList').style.display = isFilled ? '' : 'none';
    // Search/filter/sort and bulk actions only earn their place with 2+
    // devices - with one, there is nothing to search for or bulk-act on.
    document.getElementById('dmToolbar').style.display = devices.length > 1 ? 'flex' : 'none';
    // A tick on a phone that is gone, or with one phone left, means nothing.
    if (!multiDevice()) { dmSelectionMode = false; dmSelectedIds = new Set(); }
    else dmSelectedIds = new Set([...dmSelectedIds].filter((id) => devices.some((d) => d.id === id)));
    updateSelectionBar();
    renderUsbNudge();
    if (isFilled) renderDeviceList();
    renderStatusBar();
  }

  // ---------- Windows setup notice (backed by system_health / repair_system) ----------
  // Two pieces of Mirryl live OUTSIDE the app and can be removed without the
  // app knowing: the Windows Firewall rules (keyed by exe path, so a new
  // version at a new folder simply has none) and the auto-reconnect scheduled
  // task (which also breaks quietly when the folder it points at is deleted).
  //
  // Checking costs nothing and needs no rights. Fixing needs administrator, so
  // it happens only when the user presses Repair - that is why the app does
  // not demand administrator just to open.
  let systemHealth = null;
  // Session-only on purpose: "not now" should not silently disable a warning
  // about something still broken the next time the app opens.
  let systemNoticeDismissed = false;
  let systemNoticeMessage = null;
  // What the repair actually did, kept on screen after it succeeds: after a
  // minute of "Repairing...", silence cannot tell "cleaned up 137 rules"
  // apart from "there was nothing to do".
  let systemNoticeDone = null;

  async function refreshSystemHealth() {
    const h = await callBackend('system_health');
    if (h && typeof h === 'object') systemHealth = h;
    renderSystemNotice();
  }

  function systemNoticeProblems(h) {
    const problems = [];
    if (h.needs_firewall_fix) {
      problems.push(h.firewall === 'blocked'
        ? 'a firewall rule is blocking Mirryl'
        : "this version's firewall rules are missing");
    }
    if (h.needs_watcher_fix) {
      // One line per cause, each saying what Repair will do about it: the
      // three stale-* causes share a fix but not a cause, and "points at a
      // deleted folder" for all of them would send the user looking for a
      // folder that may not exist. Repair fixes all three the same way: it
      // re-registers the task with this copy's watcher.ps1 and RunLevel
      // Limited (watcher_repair_script in main.rs). The backend lists EVERY
      // cause it found, comma-separated ("disabled,stale-runlevel"), so a task
      // with two things wrong gets two lines.
      const texts = {
        'stale-deleted': 'the auto-reconnect watcher points at a deleted folder (Repair points it at this copy)',
        'stale-other-install': 'the auto-reconnect watcher points at another copy of Mirryl, not this one (Repair points it at this copy)',
        'stale-runlevel': 'the auto-reconnect watcher runs with administrator rights it does not need (Repair re-registers it without them)',
        'disabled': 'the auto-reconnect watcher is disabled (Repair re-registers it)',
        // A copy without watcher.ps1 beside Mirryl.exe: Repair would only
        // fail with "watcher.ps1 was not found", so do not promise one.
        'no-script': 'this copy has no watcher.ps1 next to Mirryl.exe, so the auto-reconnect watcher cannot be set up from here - reinstall Mirryl from the installer (Repair cannot fix this)',
      };
      const causes = String(h.watcher || '').split(',').map((c) => c.trim()).filter(Boolean);
      let known = 0;
      for (const cause of causes) {
        if (texts[cause]) { problems.push(texts[cause]); known++; }
      }
      if (known === 0) {
        problems.push('the auto-reconnect watcher is gone');
      }
    }
    return problems;
  }

  function renderSystemNotice() {
    const notice = document.getElementById('systemNotice');
    const text = document.getElementById('systemNoticeText');
    const repairBtn = document.getElementById('systemNoticeBtn');
    const dismissBtn = document.getElementById('systemNoticeDismissBtn');
    const h = systemHealth;
    const broken = h && (h.needs_watcher_fix || h.needs_firewall_fix);

    // The report of a finished repair outranks everything below: the machine
    // is healthy again by then, so `broken` is false and the hide branch would
    // take the report with it. It stays up
    // until the user dismisses it - they waited for this, they get to read it.
    if (systemNoticeDone) {
      notice.style.display = '';
      notice.classList.add('is-info');
      repairBtn.style.display = 'none';
      dismissBtn.textContent = 'Got it';
      text.innerHTML = '<strong>Windows setup repaired.</strong> ' + systemNoticeDone;
      return;
    }

    notice.classList.remove('is-info');
    repairBtn.style.display = '';
    dismissBtn.textContent = 'Not now';

    if (systemNoticeDismissed || !broken) {
      notice.style.display = 'none';
      return;
    }
    notice.style.display = '';
    if (systemNoticeMessage) {
      text.textContent = systemNoticeMessage;
      return;
    }
    const problems = systemNoticeProblems(h);
    const list = problems.length === 2 ? problems[0] + ', and ' + problems[1] : problems[0];
    text.innerHTML = '<strong>Windows setup needs a repair.</strong> '
      + `Right now ${list}. Repair asks for administrator once.`;
  }

  document.getElementById('systemNoticeDismissBtn').addEventListener('click', () => {
    // One button, two jobs, and the order matters: while a repair report is up
    // this is its "Got it", and clearing it must NOT also set the dismissed
    // flag - that flag suppresses warnings about things still broken, and a
    // successful repair is the wrong moment to arm it.
    if (systemNoticeDone) {
      systemNoticeDone = null;
    } else {
      systemNoticeDismissed = true;
    }
    renderSystemNotice();
  });

  document.getElementById('systemNoticeBtn').addEventListener('click', async () => {
    if (!systemHealth) return;
    const btn = document.getElementById('systemNoticeBtn');
    const original = btn.textContent;
    btn.disabled = true;
    btn.textContent = 'Repairing...';
    const result = await callBackend('repair_system', {
      watcher: !!systemHealth.needs_watcher_fix,
      firewall: !!systemHealth.needs_firewall_fix,
    });
    btn.disabled = false;
    btn.textContent = original;

    const text = String(result || '');
    if (/^done\./i.test(text.trim())) {
      // Trust the machine, not the return value: re-run the same read-only
      // check and let it decide whether anything is still broken.
      systemNoticeMessage = null;
      // ...but keep what the backend says it DID. That sentence is the only
      // account of a minute of waiting (see systemNoticeDone). The "Done." prefix is protocol
      // between the two layers, not something to show anybody.
      systemNoticeDone = text.trim().replace(/^done\.\s*/i, '')
        || 'Everything is in order again.';
      await refreshSystemHealth();
      return;
    }
    // Declined UAC ("Skipped.") or a real error: keep the notice up, say what
    // happened, and leave Repair there to try again.
    systemNoticeMessage = text.replace(/^Skipped\.\s*/i, '');
    renderSystemNotice();
  });

  // Set while re-pairing an existing device, so Finish updates its
  // host_port instead of adding a duplicate row. Cleared on fresh "+ Add
  // device" and on leaving the wizard via Home.
  let rePairingDeviceId = null;
  // The phone serial discovered during the last successful QR pairing (from the
  // `_adb-tls-connect` mDNS name). Passed to add_device so the backend can
  // dedupe by the stable hardware identity instead of the changing ip:port.
  let lastPairedSerial = null;

  function openWizard() {
    rePairingDeviceId = null;
    // Also stale otherwise: a QR pairing earlier in the session leaves this
    // set, and a manual pairing right after (which never learns its own
    // serial mid-flow) would silently reuse the PREVIOUS phone's serial
    // instead of resolving the new one - merging two different phones into
    // one row.
    lastPairedSerial = null;
    // Clear identity fields so the wizard starts clean - otherwise a stale
    // ip:port from a previous device would make the (device-scoped) LIVE
    // STATUS show the wrong phone until the new one connects. Re-pair sets
    // pairHostPort again right after calling this.
    document.getElementById('connectHostPort').value = '';
    document.getElementById('pairHostPort').value = '';
    document.getElementById('pairCode').value = '';
    // And the phone-app offer of the previous phone, if setup was left there.
    document.getElementById('phoneAppBlock').style.display = 'none';
    showView('view-wizard');
    goToStep(1);
  }
  document.getElementById('addDeviceBtnEmpty').addEventListener('click', openWizard);

  // ---------- Generic confirm dialog (replaces native confirm(), which shows
  // a "tauri.localhost says" browser-chrome label we don't want) ----------
  let pendingConfirmResolve = null;
  // One dialog at a time. There is a single overlay and a single pending
  // resolver, and a second showConfirm/showNotice would overwrite both and
  // leave the first promise unresolved. So dialogs queue: each waits for the
  // previous one to be answered, and the DOM is only touched when its turn
  // comes.
  let dialogQueue = Promise.resolve();
  // checkboxLabel is optional - when given, an unchecked checkbox appears
  // between the body and the buttons. Its state after confirming is read
  // separately (getElementById('confirmCheckbox').checked): the resolved
  // value itself stays a plain true/false, so callers can stay
  // `if (!confirmed) return`.
  function showConfirm(title, body, okLabel, checkboxLabel, hideCancel) {
    const show = () => new Promise(resolve => {
      document.getElementById('confirmTitle').textContent = title;
      document.getElementById('confirmBody').textContent = body;
      document.getElementById('confirmOkBtn').textContent = okLabel || 'OK';
      document.getElementById('confirmCancelBtn').style.display = hideCancel ? 'none' : '';
      const checkboxRow = document.getElementById('confirmCheckboxRow');
      const checkbox = document.getElementById('confirmCheckbox');
      checkboxRow.style.display = checkboxLabel ? 'flex' : 'none';
      document.getElementById('confirmCheckboxLabel').textContent = checkboxLabel || '';
      checkbox.checked = false;
      document.getElementById('confirmOverlay').classList.add('active');
      pendingConfirmResolve = resolve;
    });
    const turn = dialogQueue.then(show, show);
    // The queue itself never rejects, so one dismissed dialog cannot block
    // the next; callers still get their own promise.
    dialogQueue = turn.catch(() => {});
    return turn;
  }
  /**
   * A one-button notice in Mirryl's own styling.
   *
   * Reuses the confirm dialog with Cancel hidden, so failures are explained
   * inside the app instead of by a native message box from the engine - which
   * would look like the whole program had crashed rather than like a phone
   * that simply is not plugged in.
   */
  function showNotice(title, body) {
    return showConfirm(title, body, 'OK', undefined, true);
  }

  function resolveConfirm(result) {
    document.getElementById('confirmOverlay').classList.remove('active');
    if (pendingConfirmResolve) {
      pendingConfirmResolve(result);
      pendingConfirmResolve = null;
    }
  }
  // Notices the backend raises on its own (an unreadable pc_identity.json,
  // from the mirror path a cable phone takes).
  // Queued like every other dialog.
  if (window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen) {
    window.__TAURI__.event.listen('mirryl-notice', (event) => {
      const text = event && event.payload ? String(event.payload) : '';
      if (text) showNotice('About this phone', text);
    });
  }
  document.getElementById('confirmOkBtn').addEventListener('click', () => resolveConfirm(true));
  document.getElementById('confirmCancelBtn').addEventListener('click', () => resolveConfirm(false));

  // ---------- Wizard navigation ----------
  // Two steps: installing the auto-reconnect watcher needs no input from the
  // user, so it is part of Step 2's Finish action rather than a step of its own.
  const TOTAL_STEPS = 2;
  let currentStep = 1;

  const STEP_TITLES = {
    1: ['Pair with the phone', 'Whichever is faster for you - both work the same.'],
    2: ['Connect & grant permission', "Mirryl will try to find and connect to the phone automatically. Finishing also sets up auto-reconnect after every reboot."],
  };

  // ---------- QR pairing (Step 1's "Scan QR code" panel) ----------
  let qrPollTimer = null;

  function stopQrPairingPolling() {
    if (qrPollTimer) {
      clearTimeout(qrPollTimer);
      qrPollTimer = null;
    }
  }

  async function startQrPairingFlow() {
    const placeholder = document.getElementById('qrPlaceholder');
    const img = document.getElementById('qrImage');
    const statusEl = document.getElementById('qrStatus');
    stopQrPairingPolling();
    lastPairedSerial = null; // fresh attempt; set again only on this pairing's success
    img.style.display = 'none';
    placeholder.style.display = '';
    placeholder.textContent = 'Generating...';
    statusEl.textContent = '';

    const result = await callBackend('start_qr_pairing');
    if (typeof result !== 'string' || !result.startsWith('data:image')) {
      placeholder.textContent = "Couldn't generate the QR code.";
      statusEl.textContent = typeof result === 'string' ? result : '';
      return;
    }
    img.src = result;
    img.style.display = '';
    placeholder.style.display = 'none';
    statusEl.textContent = 'Waiting for the phone to scan...';
    pollQrPairingStatus();
  }

  function pollQrPairingStatus() {
    qrPollTimer = setTimeout(async () => {
      const result = await callBackend('get_qr_pairing_status');
      const statusEl = document.getElementById('qrStatus');
      if (!result || typeof result !== 'object') {
        pollQrPairingStatus();
        return;
      }
      if (result.status === 'success') {
        // Pairing done. Fully automatic from here: jump to Step 2, connect
        // to the address the backend discovered via mDNS, and - if that
        // works - run the same Finish logic (watcher + save device + back to
        // the Device Manager). Scan the QR and everything else just happens.
        statusEl.textContent = 'Paired successfully!';
        const discovered = typeof result.connect === 'string' ? result.connect : '';
        lastPairedSerial = typeof result.serial === 'string' ? result.serial : null;
        goToStep(2);
        if (discovered) {
          document.getElementById('connectHostPort').value = discovered;
          document.getElementById('connectOutput').textContent =
            'Paired! Connecting automatically to ' + discovered + '...';
          (async () => {
            const connected = await runConnectFlow();
            if (connected) {
              const out = document.getElementById('connectOutput');
              out.textContent += '\n\nFinishing setup automatically...';
              await finishSetup();
            }
          })();
        } else {
          document.getElementById('connectOutput').textContent =
            "Paired! Couldn't auto-detect the connect address - enter the IP:Port from the phone's Wireless debugging screen, then click Connect.";
        }
      } else if (result.status === 'error') {
        statusEl.textContent = 'Error: ' + result.message;
      } else {
        statusEl.textContent = 'Waiting for the phone to scan...';
        pollQrPairingStatus();
      }
    }, 1500);
  }

  function goToStep(step) {
    currentStep = step;
    document.querySelectorAll('.wiz-step').forEach(el => {
      el.style.display = Number(el.dataset.step) === step ? '' : 'none';
    });
    document.getElementById('wizStepTitle').textContent = STEP_TITLES[step][0];
    document.getElementById('wizStepSubtitle').textContent = STEP_TITLES[step][1];
    document.getElementById('counter').textContent = `Step ${step} of ${TOTAL_STEPS}`;
    document.getElementById('backBtn').disabled = step === 1;
    document.getElementById('nextBtn').textContent = step === TOTAL_STEPS ? 'Finish' : 'Next';

    for (let i = 1; i <= TOTAL_STEPS; i++) {
      document.getElementById('wizTab' + i).classList.toggle('active', i === step);
    }
    const track = document.getElementById('progressTrack');
    track.innerHTML = '';
    for (let i = 1; i <= TOTAL_STEPS; i++) {
      const seg = document.createElement('div');
      seg.className = 'seg' + (i < step ? ' done' : i === step ? ' current' : '');
      track.appendChild(seg);
    }

    // Only actually start the listener/mDNS/QR generation when the wizard is
    // the view the user is looking at - goToStep(1) is also called just to
    // reset internal wizard state while navigating *away* to another view
    // (Home, Finish), where kicking off a background pairing session would
    // be pointless and wasteful.
    if (step === 1 && document.getElementById('view-wizard').classList.contains('active')) {
      startQrPairingFlow();
    } else {
      stopQrPairingPolling();
      callBackend('cancel_qr_pairing');
    }
  }

  // ---------- What to do when a pair/connect fails ----------
  // Never elevate by ourselves: `firewall_fix` would throw a UAC dialog at the
  // user with nothing on screen asking for it, in the middle of something else.
  // So: restart ADB (costs nothing, needs no
  // rights), then CHECK the firewall read-only. Rules present means the
  // firewall is not the problem and there is nothing to offer; only when they
  // are genuinely missing or blocked does the user get told, and the fix is a
  // button they press.
  const FIREWALL_SUSPECT_HINT =
    "Mirryl's Windows Firewall rules are missing or blocked, which can cause exactly this. "
    + 'Press "Fix connection issues" below to rebuild them (asks for administrator once).';

  async function retryFixups() {
    await callBackend('restart_adb');
    const health = await callBackend('system_health');
    return !!(health && typeof health === 'object' && health.needs_firewall_fix);
  }

  function isInstallSuccess(text) {
    return /^done\./i.test(text.trim());
  }

  /**
   * The user saw the UAC prompt and said no.
   *
   * That must not cost them the device they just spent two steps pairing: the
   * watcher is a background convenience, not part of being paired. Setup goes
   * on, and the notice on the device list offers to install it later.
   */
  function isPermissionDeclined(text) {
    return /^skipped\./i.test(String(text).trim());
  }

  // Finish: install the auto-reconnect watcher, save the device, then return
  // to the Device Manager. Shared by the Finish button and the fully
  // automatic QR flow (which runs it right after auto-connecting).
  async function finishSetup() {
    const btn = document.getElementById('nextBtn');
    const original = btn.textContent;
    btn.disabled = true;
    btn.textContent = 'Finishing setup...';
    const result = await callBackend('install_watcher');
    btn.disabled = false;
    btn.textContent = original;

    if (!isInstallSuccess(result) && !isPermissionDeclined(result)) {
      document.getElementById('connectOutput').textContent = 'Could not finish setup: ' + result;
      return false;
    }

    const hostPort = document.getElementById('connectHostPort').value.trim();
    if (hostPort) {
      // QR pairing already knows the serial; the manual IP:Port + code path
      // never does. Ask the live connection directly in that case, so a
      // phone added by USB and then paired manually merges into its existing
      // row instead of creating a duplicate for the same physical device.
      const serial = lastPairedSerial || await callBackend('device_serial', { hostPort });
      if (rePairingDeviceId !== null) {
        await callBackend('update_device', { id: rePairingDeviceId, alias: null, hostPort, serial });
      } else {
        const ip = hostPort.split(':')[0];
        // Pass the serial (from QR, or just resolved above) so the backend
        // dedupes by the phone's stable hardware id, not the changing ip:port.
        const added = await callBackend('add_device', { alias: `Device (${ip})`, hostPort, serial });
        // A string is the backend saying devices.json could not be written:
        // the phone is paired, but the row is not saved.
        if (typeof added === 'string') showNotice('Paired, but not saved', added);
      }
    }
    rePairingDeviceId = null;

    await loadDevices();
    // Not awaited: it shells out to PowerShell, and the device list should
    // appear now. If the watcher install was declined a moment ago, this is
    // what puts the offer to install it back on screen.
    refreshSystemHealth();
    showView('view-home');
    return true;
  }

  document.getElementById('wizTab1').addEventListener('click', () => goToStep(1));
  document.getElementById('wizTab2').addEventListener('click', () => goToStep(2));

  document.getElementById('backBtn').addEventListener('click', () => {
    if (currentStep > 1) goToStep(currentStep - 1);
  });
  document.getElementById('nextBtn').addEventListener('click', async () => {
    if (currentStep < TOTAL_STEPS) {
      goToStep(currentStep + 1);
      return;
    }
    await finishSetup();
  });

  document.getElementById('wizHomeBtn').addEventListener('click', async () => {
    const confirmed = await showConfirm(
      'Leave setup?',
      "Your progress will be lost and you'll need to start pairing over from Step 1.",
      'Leave'
    );
    if (!confirmed) return;
    rePairingDeviceId = null;
    showView('view-home');
    goToStep(1);
    // Reconcile: an abandoned wizard may have left a connected-but-unsaved
    // phone; clear it so it doesn't linger as a phantom in the LIVE STATUS.
    await loadDevices(true);
  });

  // ---------- Actions (Step 1: Pair) ----------
  // If the first pair attempt fails, automatically try the same fixes the
  // "Fix connection issues" / "Restart ADB" buttons below would apply, then
  // retry once before giving up and leaving it to the user.
  function isPairSuccess(text) {
    return /successfully paired/i.test(text);
  }

  document.getElementById('pairBtn').addEventListener('click', async () => {
    const out = document.getElementById('pairOutput');
    const troubleBlock = document.getElementById('pairTroubleBlock');
    const hostPort = document.getElementById('pairHostPort').value.trim();
    const code = document.getElementById('pairCode').value.trim();
    if (!hostPort || !code) {
      out.textContent = 'Enter both the pairing IP:Port and the 6-digit code.';
      return;
    }

    troubleBlock.style.display = 'none';
    out.textContent = 'Pairing...';
    let result = await callBackend('pair', { hostPort, code });

    if (!isPairSuccess(result)) {
      out.textContent = result + '\n\nConnection issue detected - restarting ADB and retrying...';
      const firewallSuspect = await retryFixups();
      if (firewallSuspect) out.textContent += '\n' + FIREWALL_SUSPECT_HINT;
      out.textContent += '\nRetrying pairing...';
      result = await callBackend('pair', { hostPort, code });

      if (!isPairSuccess(result)) {
        out.textContent = result + "\n\nAutomatic fixes didn't resolve it. Make sure the phone is awake, on the same Wi-Fi network, and that the IP:Port/code are current (they change every time you reopen Wireless debugging pairing on the phone).";
        troubleBlock.style.display = '';
        return;
      }
    }

    out.textContent = result;
  });

  // ---------- Actions (Step 2: Connect & grant) ----------
  // Same auto-fix-then-retry pattern as Step 1's pair: if the first connect
  // attempt fails, restart ADB, check the firewall, and retry once before
  // surfacing the manual troubleshooting buttons.
  function isConnectSuccess(text) {
    return /connected to/i.test(text) && !/failed|cannot|unable/i.test(text);
  }
  function renderConnectResult(result) {
    if (typeof result === 'string') return result;
    let text = 'Connect: ' + result.connect + '\nGrant permission: ' + result.grant;
    // The identity notice, when there is one (matriculation_notice in main.rs).
    if (result.notice) text += '\n\n' + result.notice;
    return text;
  }

  // Shared by the Connect button and the fully automatic QR flow.
  // Returns true when the device ended up connected.
  async function runConnectFlow() {
    const out = document.getElementById('connectOutput');
    const troubleBlock = document.getElementById('connectTroubleBlock');
    const hostPort = document.getElementById('connectHostPort').value.trim();
    if (!hostPort) {
      out.textContent = "Enter the IP:Port shown on the phone's Wireless debugging screen.";
      return false;
    }

    troubleBlock.style.display = 'none';
    hidePhoneAppOffer();
    out.textContent = 'Working...';
    const serial = lastPairedSerial;
    let result = await callBackend('connect_and_grant', { hostPort, serial });

    if (typeof result !== 'string' && !isConnectSuccess(result.connect)) {
      out.textContent = renderConnectResult(result) + '\n\nConnection issue detected - restarting ADB and retrying...';
      const firewallSuspect = await retryFixups();
      if (firewallSuspect) out.textContent += '\n' + FIREWALL_SUSPECT_HINT;
      out.textContent += '\nRetrying connection...';
      result = await callBackend('connect_and_grant', { hostPort, serial });

      if (typeof result !== 'string' && !isConnectSuccess(result.connect)) {
        out.textContent = renderConnectResult(result) + "\n\nAutomatic fixes didn't resolve it. Make sure the phone is awake, on the same Wi-Fi network, and that the IP:Port is current.";
        troubleBlock.style.display = '';
        return false;
      }
    }

    // If the backend re-resolved a fresh port, reflect it in the field so
    // finishSetup saves the current port (not the stale one the user typed).
    if (typeof result === 'object' && result.host_port && result.host_port !== hostPort) {
      document.getElementById('connectHostPort').value = result.host_port;
    }

    out.textContent = renderConnectResult(result);
    const connected = typeof result !== 'string' && isConnectSuccess(result.connect);
    // A phone without the Mirryl app, or with an older one, stops here with
    // the offer instead of finishing on its own (the QR flow finishes by
    // itself when this returns true). Missing means nothing was granted yet:
    // the button installs the app and connects again.
    if (connected && (result.phone_app === 'missing' || result.phone_app === 'outdated')) {
      showPhoneAppOffer(result.phone_app);
      return false;
    }
    return connected;
  }

  document.getElementById('connectBtn').addEventListener('click', runConnectFlow);

  // ---------- The phone app (Step 2) ----------
  // The Mirryl phone app is not in any store: this PC app carries it and
  // installs it with this button, only when the user presses
  // it. Never shown for a phone that already has the app at this version.
  const PHONE_APP_OFFER = {
    missing: {
      hint: "This phone doesn't have the Mirryl app yet. It keeps Wireless debugging on, so the phone "
        + 'reconnects to this PC by itself after a reboot. Mirryl carries it and can install it now '
        + 'over this connection - nothing is downloaded.',
      button: 'Install the Mirryl app on the phone',
    },
    outdated: {
      hint: 'This phone has an older version of the Mirryl app. Mirryl can update it now over this '
        + 'connection; its settings on the phone stay. Or press Finish to keep the version it has.',
      button: 'Update the Mirryl app on the phone',
    },
  };

  function showPhoneAppOffer(state) {
    const offer = PHONE_APP_OFFER[state];
    if (!offer) return;
    document.getElementById('phoneAppHint').textContent = offer.hint;
    document.getElementById('phoneAppBtn').textContent = offer.button;
    document.getElementById('phoneAppOutput').textContent = '';
    document.getElementById('phoneAppBlock').style.display = '';
  }

  function hidePhoneAppOffer() {
    document.getElementById('phoneAppBlock').style.display = 'none';
  }

  document.getElementById('phoneAppBtn').addEventListener('click', async () => {
    const btn = document.getElementById('phoneAppBtn');
    const out = document.getElementById('phoneAppOutput');
    const hostPort = document.getElementById('connectHostPort').value.trim();
    const original = btn.textContent;
    btn.disabled = true;
    btn.textContent = 'Installing...';
    out.textContent = 'Installing. If the phone asks for confirmation, accept it on its screen.';
    const res = await callBackend('install_phone_app', { hostPort });
    btn.disabled = false;
    btn.textContent = original;
    if (!res || typeof res !== 'object' || !res.ok) {
      out.textContent = typeof res === 'string' ? res : ((res && res.message) || 'The Mirryl app could not be installed on the phone.');
      return;
    }
    // Installed: connect again, which now grants the permission, and finish
    // the way the QR flow does.
    const connected = await runConnectFlow();
    const main = document.getElementById('connectOutput');
    main.textContent = res.message + '\n\n' + main.textContent;
    if (connected) {
      main.textContent += '\n\nFinishing setup automatically...';
      await finishSetup();
    }
  });

  document.getElementById('discoverBtn').addEventListener('click', async () => {
    const out = document.getElementById('connectOutput');
    out.textContent = 'Searching for the phone via mDNS...';
    const result = await callBackend('discover_port');
    out.textContent = result;
    const match = /\d{1,3}(?:\.\d{1,3}){3}:\d+/.exec(result);
    if (match) {
      document.getElementById('connectHostPort').value = match[0];
    }
  });

  // ---------- Troubleshooting (shared across steps 1 & 2) ----------
  const runFirewallFix = async (outputElId) => {
    const out = document.getElementById(outputElId);
    out.textContent = 'Working...';
    out.textContent = await callBackend('firewall_fix');
  };
  document.getElementById('firewallBtn1').addEventListener('click', () => runFirewallFix('firewallOutput1'));
  document.getElementById('firewallBtn2').addEventListener('click', () => runFirewallFix('firewallOutput2'));

  const runRestartAdb = async (btnId) => {
    const btn = document.getElementById(btnId);
    const original = btn.textContent;
    btn.textContent = 'Restarting...';
    btn.disabled = true;
    await callBackend('restart_adb');
    btn.textContent = original;
    btn.disabled = false;
  };
  document.getElementById('restartAdbBtn1').addEventListener('click', () => runRestartAdb('restartAdbBtn1'));
  document.getElementById('restartAdbBtn2').addEventListener('click', () => runRestartAdb('restartAdbBtn2'));

  // ---------- Live status (adb devices) ----------
  // The wizard's LIVE STATUS panel should show ONLY the device currently being
  // configured, not every connected device (the Device Manager already lists
  // all of them). We keep the FULL `adb devices` text in lastStatusText for the
  // Device Manager's per-device status detection, but filter the wizard panel
  // to the device whose ip:port we're pairing/connecting to.
  function filterStatusForWizardDevice(fullText, targetHostPort) {
    const ip = (targetHostPort.split(':')[0] || '').trim();
    if (!ip) return 'Waiting for the device to connect...';
    const matches = fullText.split('\n').filter((l) => l.trim().startsWith(ip + ':'));
    if (matches.length === 0) return 'Waiting for the device to connect...';
    return 'List of devices attached\n' + matches.join('\n');
  }

  const refreshStatus = async () => {
    const result = await callBackend('get_status');
    lastStatusText = result || '';
    // In every view, not only the Device Manager: the answer takes a poll to
    // arrive, and the list should be right the moment the user opens it.
    learnTransportSerials();
    const statusEl = document.getElementById('statusOutput');
    if (document.getElementById('view-wizard').classList.contains('active')) {
      const target = document.getElementById('connectHostPort').value.trim();
      statusEl.textContent = filterStatusForWizardDevice(lastStatusText, target);
    } else {
      statusEl.textContent = result || '(no devices found yet)';
    }
    const list = document.getElementById('dmDeviceList');
    const isEditingAlias = list.querySelector('.device-alias-input');
    // Outside the `devices.length > 0` guard below on purpose: a cabled phone
    // with an EMPTY device list is the main case this exists for - someone who
    // has never paired anything, which is exactly who the offer is aimed at.
    renderUsbNudge();
    if (devices.length > 0 && !isEditingAlias) {
      // Cheap: it reads a map the app keeps in memory. Nothing here touches adb
      // or the phone, so the one-window rule costs the phone nothing.
      await refreshOpenMirrors();
      // Before syncing addresses: a device that only matches through mDNS has
      // to be resolvable first, or the sync below sees nothing to sync.
      await refreshMdnsIfNeeded();
      await learnMissingSerials();
      await syncDeviceAddresses();
      renderDeviceList();
    }
    setTimeout(refreshStatus, 3000);
  };
  refreshStatus();

  // ---------- Device tools (support toolkit) ----------
  // Everything here targets ONE device, the row it was opened from. The target
  // for adb is the LIVE transport, not the saved address - same rule as
  // mirroring, and for the same reason: the saved ip:port goes stale as a
  // matter of routine.
  let toolsDevice = null;

  function toolsTarget() {
    return toolsDevice ? (findLiveAddress(toolsDevice) || toolsDevice.host_port) : '';
  }

  function openDeviceTools(d) {
    openDevicePage(d, 'info');
  }

  async function refreshDeviceInfo() {
    const fields = {
      infoBattery: 'battery', infoStorage: 'storage', infoAndroid: 'android',
      infoModel: 'model', infoSerial: 'serial', infoUptime: 'uptime', infoIp: 'ip',
    };
    Object.keys(fields).forEach((id) => { document.getElementById(id).textContent = '…'; });
    const info = await callBackend('device_info', { target: toolsTarget() });
    const grid = document.getElementById('toolsInfo');
    if (!info || typeof info !== 'object') {
      grid.classList.add('is-unreachable');
      Object.keys(fields).forEach((id) => { document.getElementById(id).textContent = '—'; });
      return;
    }
    // Reachable is its own answer: seven dashes with no explanation looks like
    // the app broke, when what happened is the phone went away.
    grid.classList.toggle('is-unreachable', !info.reachable);
    Object.entries(fields).forEach(([id, key]) => {
      document.getElementById(id).textContent = info[key] || '—';
    });
  }

  document.getElementById('toolsRefreshBtn').addEventListener('click', refreshDeviceInfo);

  // ---------- The page of one device ----------
  // What the ⋮ of a row opens: on the left, the phone's name and state, its
  // two screens (Apps, Info) and every action that is about this one phone;
  // on the right, the screen chosen. The right-click on a row keeps a short
  // menu of the quick actions.
  let devicePanel = 'apps';
  let editingPageAlias = false;

  /** The page's phone as the list has it now (alias, addresses, state). */
  function pageDevice() {
    if (!appsDevice) return null;
    return devices.find((x) => x.id === appsDevice.id) || null;
  }

  function openDevicePage(d, panel) {
    const changed = !appsDevice || appsDevice.id !== d.id;
    toolsDevice = d;
    appsDevice = d;
    if (changed) {
      appsList = [];
      appsLock = { supported: false, pin: false };
      appsSelected.clear();
      appsSelectMode = false;
      document.getElementById('appsSearch').value = '';
      document.getElementById('appsGrid').innerHTML = '';
      setAppsStatus('');
    }
    editingPageAlias = false;
    showView('view-device');
    renderDevicePageHead();
    showDevicePanel(panel || 'apps', true);
  }

  function showDevicePanel(panel, reload) {
    devicePanel = panel;
    hideAppMenu();
    document.getElementById('devPanelApps').classList.toggle('active', panel === 'apps');
    document.getElementById('devPanelInfo').classList.toggle('active', panel === 'info');
    document.querySelectorAll('.device-nav-item[data-panel]').forEach((b) => {
      b.classList.toggle('active', b.dataset.panel === panel);
    });
    if (panel === 'apps') {
      renderApps();
      if (reload || !appsList.length) loadApps();
    } else {
      refreshDeviceInfo();
    }
  }

  function renderDevicePageHead() {
    const d = pageDevice();
    if (!d) return;
    if (!editingPageAlias) document.getElementById('devAlias').textContent = d.alias;
    document.getElementById('appsTitle').textContent = d.alias;
    document.getElementById('toolsTitle').textContent = d.alias;
    document.getElementById('appsDropName').textContent = d.alias;
    const status = deviceStatus(d);
    const mirroring = openMirrors.has(String(d.id)) || mirrorTabs.has(String(d.id));
    const state = document.getElementById('devState');
    state.textContent = mirroring ? 'Mirroring' : STATUS_META[status].label;
    state.style.color = `var(${STATUS_META[status].varName})`;
    document.getElementById('devMirrorBtn').textContent = mirroring ? 'Show' : 'Mirror';
    document.getElementById('devForgetBtn').style.display =
      (d.host_port || liveTransport(d).kind === 'wifi') ? '' : 'none';
  }

  async function sendFileFromPage(d) {
    const path = await callBackend('pick_file');
    if (!path) return;
    const result = await callBackend('push_file', { hostPort: findLiveAddress(d) || d.host_port, localPath: path });
    showNotice(/^Error:/.test(result) ? "Couldn't send the file" : 'File sent', result);
  }

  async function restartFromPage(d) {
    const confirmed = await showConfirm(
      'Restart this device?',
      `"${d.alias}" will restart. Anything unsaved on the phone is lost, and it drops off for about half a minute.`,
      'Restart'
    );
    if (!confirmed) return;
    await callBackend('device_reboot', { target: findLiveAddress(d) || d.host_port, alias: d.alias });
  }

  /** Puts the Mirryl app back on the phone, for when it was uninstalled there. */
  async function reinstallPhoneApp(d, button) {
    const confirmed = await showConfirm(
      'Install the Mirryl app on the phone again?',
      `Mirryl installs its own app on "${d.alias}" again, over the connection it has now, and gives it back its permission to turn Wireless debugging on by itself. If the phone asks for confirmation, accept it on its screen.`,
      'Install'
    );
    if (!confirmed) return;
    const original = button.textContent;
    button.disabled = true;
    button.textContent = 'Installing…';
    const result = await callBackend('reinstall_phone_app', { target: findLiveAddress(d) || d.host_port, alias: d.alias });
    button.disabled = false;
    button.textContent = original;
    if (result && typeof result === 'object') {
      showNotice(result.ok ? 'The Mirryl app is back' : 'The Mirryl app was not installed', result.message);
      // A newer phone app can do more (the PIN): what the page knew about the
      // old one no longer holds.
      if (result.ok && appsDevice && appsDevice.id === d.id) loadApps();
    } else {
      showNotice('The Mirryl app was not installed', String(result || 'The phone did not answer.'));
    }
  }

  function editAliasOnPage(d) {
    if (editingPageAlias) return;
    editingPageAlias = true;
    const aliasEl = document.getElementById('devAlias');
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'device-alias-input';
    input.value = d.alias;
    aliasEl.textContent = '';
    aliasEl.appendChild(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (save) => {
      if (done) return;
      done = true;
      const newAlias = input.value.trim();
      if (save && newAlias && newAlias !== d.alias) {
        await callBackend('update_device', { id: d.id, alias: newAlias, hostPort: null, serial: null });
      }
      editingPageAlias = false;
      await loadDevices();
      renderDevicePageHead();
    };
    input.addEventListener('blur', () => finish(true));
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') input.blur();
      if (e.key === 'Escape') finish(false);
    });
  }

  function repairDevice(d) {
    openWizard();
    rePairingDeviceId = d.id;
    document.getElementById('pairHostPort').value = d.host_port;
  }

  // "Forget Wi-Fi address": for a phone that lives on the cable and whose
  // saved ip:port is only noise - the row shows an address that is never
  // dialled on purpose, and the watcher keeps looking for it on the network
  // every cycle.
  async function forgetWifiAddress(d) {
    const willDrop = droppableAddresses(d);
    const live = willDrop[willDrop.length - 1] || null;
    const stuck = undroppableTransports(d);
    const mirroring = openMirrors.has(String(d.id));
    const dropping = willDrop.length > 0;
    // What happens to an open mirror depends on the link THE MIRROR is
    // on, not on the row's best link: mirrorKind is why a window opened
    // over Wi-Fi stays on Wi-Fi after the cable goes in. An unknown kind
    // is unknown - promising a window will close is worse than silence.
    const mirrorOnWifi = mirroring && mirrorKind.get(String(d.id)) === 'wifi';
    const confirmed = await showConfirm(
      'Forget this Wi-Fi address?',
      // No escaping here: showConfirm sets textContent, so an escaped
      // string would show the entities themselves (same as every other
      // caller on this screen).
      `Mirryl will stop remembering ${d.host_port || live || 'the Wi-Fi address'} for "${d.alias}" and stop looking for it on the network.` +
        (dropping
          ? ' The connection at ' + willDrop.join(' and ') + ' is dropped' + (mirrorOnWifi && stuck.length === 0 ? ' and the mirror window closes.' : '.')
          : '') +
        // Said whenever a link adb opened by itself is up, dropping or
        // not: adb will not let anyone end one of those, so the phone
        // stays on Wi-Fi even when the ip:port above IS dropped.
        (stuck.length > 0
          ? ' It stays connected over the Wi-Fi link adb opened on its own; turning off wireless debugging on the phone ends that one.'
          : '') +
        (mirroring && (!dropping || stuck.length > 0)
          ? ' The mirror window stays open — until you close it, it can still offer to switch to Wi-Fi.'
          : '') +
        ' The cable keeps working, and pairing it again brings Wi-Fi back.',
      'Forget'
    );
    if (!confirmed) return;
    // Marked BEFORE the wait, not after: the whole point is to beat work
    // that is already in flight, and the backend call is itself a wait.
    const epochBefore = forgetEpoch.get(d.id) || 0;
    forgetEpoch.set(d.id, epochBefore + 1);
    const result = await callBackend('forget_wifi_address', { id: d.id, live });
    if (typeof result === 'string') {
      // Nothing was forgotten, so nothing in flight should be cancelled.
      forgetEpoch.set(d.id, epochBefore);
      showNotice('Could not forget it', result);
      return;
    }
    connectingUntil.delete(d.id);
    lastSeenConnected.delete(d.id);
    failedUntil.delete(d.id);
    await loadDevices();
  }

  async function removeDeviceAsk(d) {
    const confirmed = await showConfirm(
      'Remove this device?',
      `"${d.alias}" will be removed from Mirryl. You can add it again later by pairing it once more.`,
      'Remove',
      'Also delete its saved files'
    );
    if (!confirmed) return;
    if (document.getElementById('confirmCheckbox').checked) {
      await callBackend('delete_tools_folder', { alias: d.alias });
    }
    await callBackend('remove_device', { id: d.id });
    forgetSeen(d.id);
    appsDevice = null;
    toolsDevice = null;
    showView('view-home');
    await loadDevices();
  }

  document.querySelectorAll('.device-nav-item').forEach((b) => {
    b.addEventListener('click', () => {
      if (b.dataset.panel) {
        showDevicePanel(b.dataset.panel, false);
        return;
      }
      const d = pageDevice();
      if (!d) return;
      const action = b.dataset.action;
      if (action === 'send') sendFileFromPage(d);
      else if (action === 'restart') restartFromPage(d);
      else if (action === 'reinstall') reinstallPhoneApp(d, b);
      else if (action === 'pin') changePin(d);
      else if (action === 'profile-save') saveProfileFrom(d);
      else if (action === 'profile-apply') applyProfileTo([d]);
      else if (action === 'export') exportInventory([d]);
      else if (action === 'alias') editAliasOnPage(d);
      else if (action === 'repair') repairDevice(d);
      else if (action === 'forget') forgetWifiAddress(d);
      else if (action === 'remove') removeDeviceAsk(d);
    });
  });
  document.getElementById('devMirrorBtn').addEventListener('click', () => {
    const d = pageDevice();
    if (d) connectAndMirror(d);
  });

  // ---------- Apps of one device ----------
  // Like Device tools: one phone, the row it was opened from, reached over its
  // live transport. The list is read from the phone when the screen opens and
  // on Refresh; each action updates the app it touched right away.
  //
  // What cannot be undone asks first here, and the backend refuses on its own
  // to touch the apps the phone needs (they come marked `protected`) or to
  // uninstall a system app (those are only turned off).
  let appsDevice = null;
  let appsList = [];
  let appsFilter = 'drawer';
  let appsGeneration = 0;
  let appsBusy = false;
  let appsMenuApp = null;
  // The PIN protection of the phone on screen: whether its Mirryl app has it,
  // and whether a PIN is set (from list_apps).
  let appsLock = { supported: false, pin: false };
  // Several apps at once: the packages ticked, whether a plain click ticks
  // (the Select button) and the last one clicked, for Shift+click ranges.
  const appsSelected = new Set();
  let appsSelectMode = false;
  let appsLastClicked = null;

  function appsTarget() {
    return appsDevice ? (findLiveAddress(appsDevice) || appsDevice.host_port) : '';
  }

  function appsViewActive() {
    return activeTabKey === 'home' && devicePanel === 'apps'
      && document.getElementById('view-device').classList.contains('active');
  }

  function setAppsStatus(text, kind) {
    const el = document.getElementById('appsStatus');
    appsBusy = kind === 'busy';
    el.classList.toggle('is-error', kind === 'error');
    el.innerHTML = appsBusy ? '<div class="status-dot-spin"></div><span></span>' : '<span></span>';
    el.querySelector('span').textContent = text || '';
  }

  function appsCountText() {
    const shown = document.querySelectorAll('#appsGrid .app-tile').length;
    const blocked = appsList.filter((a) => a.suspended || !a.enabled).length;
    return `${shown} shown of ${appsList.length} on the phone` + (blocked ? ` · ${blocked} blocked or off` : '');
  }

  async function loadApps() {
    if (!appsDevice) return;
    const generation = ++appsGeneration;
    const device = appsDevice;
    setAppsStatus(appsList.length
      ? 'Reading the apps again…'
      : `Reading the apps of ${device.alias}… this takes a few seconds.`, 'busy');
    const result = await callBackend('list_apps', { target: appsTarget() });
    // A slow answer for a phone that is no longer on screen is dropped.
    if (generation !== appsGeneration || appsDevice !== device) return;
    if (!result || typeof result !== 'object' || !Array.isArray(result.apps)) {
      setAppsStatus(typeof result === 'string' ? result.replace(/^Error:\s*/, '') : 'The phone did not answer.', 'error');
      return;
    }
    appsList = result.apps;
    appsLock = result.lock || { supported: false, pin: false };
    [...appsSelected].forEach((p) => { if (!appsList.some((a) => a.package === p)) appsSelected.delete(p); });
    renderApps();
    setAppsStatus(appsCountText());
  }

  function appMatchesFilter(a) {
    if (appsFilter === 'drawer') return !!a.activity;
    if (appsFilter === 'user') return !a.system;
    if (appsFilter === 'system') return a.system;
    if (appsFilter === 'blocked') return a.suspended || !a.enabled;
    return true;
  }

  function renderApps() {
    const grid = document.getElementById('appsGrid');
    const query = document.getElementById('appsSearch').value.trim().toLowerCase();
    const shown = appsList.filter((a) => appMatchesFilter(a)
      && (!query || a.label.toLowerCase().includes(query) || a.package.toLowerCase().includes(query)));
    grid.innerHTML = '';
    shown.forEach((a) => {
      const tile = document.createElement('button');
      tile.className = 'app-tile'
        + (a.enabled ? '' : ' is-off')
        + (a.suspended ? ' is-blocked' : '')
        + (appsMenuApp && appsMenuApp.package === a.package ? ' menu-open' : '')
        + (appsSelected.has(a.package) ? ' selected' : '');
      tile.title = a.package;
      const icon = /^[A-Za-z0-9+/=]+$/.test(a.icon || '')
        ? `<div class="app-icon"><img alt="" src="data:image/png;base64,${a.icon}"></div>`
        : `<div class="app-icon placeholder">${escapeHtml((a.label || '?').trim().charAt(0).toUpperCase() || '?')}</div>`;
      const chip = !a.enabled ? '<span class="app-chip">Off</span>'
        : a.suspended ? '<span class="app-chip">Blocked</span>'
        : a.locked ? '<span class="app-chip is-pin">PIN</span>'
        : a.protected && appsFilter !== 'drawer' ? '<span class="app-chip is-protected">Protected</span>' : '';
      tile.innerHTML = `${icon}<div class="app-name"></div>${chip}`;
      tile.querySelector('.app-name').textContent = a.label;
      tile.addEventListener('click', (e) => {
        if (e.shiftKey && appsLastClicked) {
          selectRange(shown, appsLastClicked, a.package);
        } else if (appsSelectMode || e.ctrlKey || e.metaKey || appsSelected.size) {
          toggleSelected(a.package);
        } else {
          openAppOnPhone(a);
        }
        appsLastClicked = a.package;
      });
      tile.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (appsSelected.has(a.package) && appsSelected.size > 1) showBulkMenu(e.clientX, e.clientY);
        else showAppMenu(a, e.clientX, e.clientY);
      });
      grid.appendChild(tile);
    });
    grid.classList.toggle('select-mode', appsSelectMode || appsSelected.size > 0);
    renderSelectionBar();
    if (!appsBusy && !document.getElementById('appsStatus').classList.contains('is-error') && appsList.length) {
      setAppsStatus(appsCountText());
    }
  }

  async function runAppAction(a, action) {
    const device = appsDevice;
    setAppsStatus(`Working on ${a.label}…`, 'busy');
    const result = await callBackend('app_action', {
      target: appsTarget(), alias: device.alias, package: a.package, label: a.label,
      action, activity: a.activity || null,
    });
    if (appsDevice !== device) return null;
    if (!result || typeof result !== 'object') {
      setAppsStatus(String(result || 'The phone did not answer.').replace(/^Error:\s*/, ''), 'error');
      return null;
    }
    setAppsStatus(result.message, result.ok ? '' : 'error');
    return result;
  }

  async function openAppOnPhone(a) {
    hideAppMenu();
    if (!a.activity) {
      showNotice('Nothing to open', `${a.label} has no icon on the phone to open: it works in the background.`);
      return;
    }
    if (!a.enabled) {
      showNotice(`${a.label} is turned off`, 'Turn it on first (right-click it), then open it.');
      return;
    }
    const device = appsDevice;
    const result = await runAppAction(a, 'open');
    // Opened on the phone; the screen shows it, like the app drawer would.
    if (result && result.ok) connectAndMirror(device);
  }

  /** What each line of an app's menu asks before it acts, or null to act at once. */
  function appActionConfirm(a, action) {
    if (action === 'clear_data') {
      return [`Delete the data of ${a.label}?`,
        `Everything ${a.label} keeps on "${appsDevice.alias}" is deleted: accounts signed in, settings and files of its own. It starts as if it had just been installed. This cannot be undone.`,
        'Delete its data'];
    }
    if (action === 'uninstall') {
      return [`Uninstall ${a.label}?`,
        `${a.label} and all its data are removed from "${appsDevice.alias}". This cannot be undone: "Save a copy of the APK" keeps the app, not its data.`,
        'Uninstall'];
    }
    if (action === 'disable') {
      return [`Turn off ${a.label}?`,
        `${a.label} disappears from "${appsDevice.alias}" and stops running, with its data kept. If the phone uses it for something, that stops working too. "Turn on" here brings it back.`,
        'Turn off'];
    }
    if (action === 'block') {
      const extra = {
        'com.android.vending': ' While it is blocked, nobody can install apps from it, and the apps on the phone are not updated either.',
        'com.android.settings': ' While it is blocked, the phone\'s settings do not open - choosing a new Wi-Fi network included - until it is unblocked here.',
      }[a.package] || (a.protected ? ' The phone uses this one for itself: parts of the phone may stop working while it is blocked.' : '');
      const fallback = a.system
        ? ' If Android does not allow it to be blocked, it is turned off instead (it disappears from the phone); Unblock brings it back either way.'
        : '';
      return [`Block ${a.label}?`,
        `${a.label} stays on "${appsDevice.alias}", greyed out, and does not open until it is unblocked from here. Its data is kept.${extra}${fallback}`,
        'Block'];
    }
    return null;
  }

  async function doAppAction(a, action) {
    hideAppMenu();
    if (action === 'open') {
      openAppOnPhone(a);
      return;
    }
    if (action === 'pin' || action === 'unpin') {
      protectWithPin(a, action === 'pin');
      return;
    }
    if (action === 'backup') {
      const device = appsDevice;
      setAppsStatus(`Copying ${a.label} from the phone…`, 'busy');
      const result = await callBackend('backup_apk', {
        target: appsTarget(), alias: device.alias, package: a.package, label: a.label,
        version: a.version || '', apks: a.apks || [],
      });
      if (appsDevice !== device) return;
      if (result && typeof result === 'object') setAppsStatus(result.message, result.ok ? '' : 'error');
      else setAppsStatus(String(result || '').replace(/^Error:\s*/, ''), 'error');
      return;
    }
    const ask = appActionConfirm(a, action);
    if (ask && !(await showConfirm(ask[0], ask[1], ask[2]))) return;
    const result = await runAppAction(a, action);
    if (!result || !result.ok) return;
    // Block may have turned the app off instead of blocking it, and Unblock
    // undoes either: the phone says which, so read the list again.
    if (action === 'block' || action === 'unblock') {
      const message = result.message;
      await loadApps();
      setAppsStatus(message);
      return;
    }
    // The app's new state, here and now; Refresh reads it all again.
    if (action === 'disable') a.enabled = false;
    else if (action === 'enable') a.enabled = true;
    else if (action === 'uninstall') appsList = appsList.filter((x) => x !== a);
    const message = result.message;
    renderApps();
    setAppsStatus(message);
  }

  /**
   * Asks for a new PIN twice, in Mirryl's own dialog. Resolves to the digits,
   * or null when cancelled.
   */
  function askNewPin(title, body) {
    return new Promise((resolve) => {
      const overlay = document.getElementById('pinOverlay');
      const one = document.getElementById('pinInput');
      const two = document.getElementById('pinInput2');
      const error = document.getElementById('pinError');
      document.getElementById('pinTitle').textContent = title;
      document.getElementById('pinBody').textContent = body;
      one.value = '';
      two.value = '';
      error.textContent = '';
      overlay.classList.add('active');
      setTimeout(() => one.focus(), 0);
      const close = (value) => {
        overlay.classList.remove('active');
        okBtn.removeEventListener('click', ok);
        cancelBtn.removeEventListener('click', cancel);
        one.removeEventListener('keydown', key);
        two.removeEventListener('keydown', key);
        resolve(value);
      };
      const ok = () => {
        if (!/^[0-9]{4,12}$/.test(one.value)) {
          error.textContent = 'The PIN must be 4 to 12 digits.';
          return;
        }
        if (one.value !== two.value) {
          error.textContent = 'The two PINs are not the same.';
          return;
        }
        close(one.value);
      };
      const cancel = () => close(null);
      const key = (e) => {
        error.textContent = '';
        if (e.key === 'Enter') ok();
        if (e.key === 'Escape') cancel();
      };
      const okBtn = document.getElementById('pinOkBtn');
      const cancelBtn = document.getElementById('pinCancelBtn');
      okBtn.addEventListener('click', ok);
      cancelBtn.addEventListener('click', cancel);
      one.addEventListener('keydown', key);
      two.addEventListener('keydown', key);
    });
  }

  const PIN_NEEDS_NEW_APP = 'The Mirryl app on this phone is older than the PIN protection. Press "Reinstall the Mirryl app" on the left, then try again.';
  const PIN_FORGOTTEN = 'If it is forgotten, Mirryl on this PC changes it or removes the protection without asking for it.';

  async function setPinFor(d, title) {
    const pin = await askNewPin(title, `The protected apps of "${d.alias}" open with this PIN. ${PIN_FORGOTTEN}`);
    if (!pin) return false;
    const result = await callBackend('set_lock_pin', { target: findLiveAddress(d) || d.host_port, alias: d.alias, pin });
    if (!result || typeof result !== 'object' || !result.ok) {
      showNotice('The PIN was not set', (result && result.message) || String(result || 'The phone did not answer.'));
      return false;
    }
    appsLock.pin = true;
    return true;
  }

  /** The page may still remember an older phone app: asks the phone again before saying so. */
  async function pinSupported() {
    if (appsLock.supported) return true;
    await loadApps();
    return appsLock.supported;
  }

  async function changePin(d) {
    if (!(await pinSupported())) {
      showNotice('PIN protection', PIN_NEEDS_NEW_APP);
      return;
    }
    if (await setPinFor(d, appsLock.pin ? 'Change the PIN' : 'Choose a PIN')) {
      showNotice('PIN set', `The protected apps of "${d.alias}" now open with the new PIN.`);
    }
  }

  async function protectWithPin(a, on) {
    const d = appsDevice;
    if (!(await pinSupported())) {
      showNotice('PIN protection', PIN_NEEDS_NEW_APP);
      return;
    }
    if (on && !appsLock.pin && !(await setPinFor(d, 'Choose a PIN for this phone'))) return;
    setAppsStatus(`Working on ${a.label}…`, 'busy');
    const result = await callBackend('app_pin', {
      target: appsTarget(), alias: d.alias, package: a.package, label: a.label, action: on ? 'protect' : 'unprotect',
    });
    if (appsDevice !== d) return;
    if (!result || typeof result !== 'object' || !result.ok) {
      setAppsStatus((result && result.message) || String(result || 'The phone did not answer.'), 'error');
      return;
    }
    const message = result.message;
    await loadApps();
    setAppsStatus(message);
  }

  // ---------- Several apps at once ----------
  function toggleSelected(pkg) {
    if (appsSelected.has(pkg)) appsSelected.delete(pkg);
    else appsSelected.add(pkg);
    renderApps();
  }

  function selectRange(shown, fromPkg, toPkg) {
    const a = shown.findIndex((x) => x.package === fromPkg);
    const b = shown.findIndex((x) => x.package === toPkg);
    if (a < 0 || b < 0) {
      toggleSelected(toPkg);
      return;
    }
    shown.slice(Math.min(a, b), Math.max(a, b) + 1).forEach((x) => appsSelected.add(x.package));
    renderApps();
  }

  function clearSelection() {
    appsSelected.clear();
    appsSelectMode = false;
    appsLastClicked = null;
    renderApps();
  }

  function selectedApps() {
    return appsList.filter((a) => appsSelected.has(a.package));
  }

  function renderSelectionBar() {
    const bar = document.getElementById('appsSelBar');
    const on = appsSelectMode || appsSelected.size > 0;
    bar.style.display = on ? 'flex' : 'none';
    document.getElementById('appsSelCount').textContent = appsSelected.size === 1
      ? '1 app selected'
      : `${appsSelected.size} apps selected`;
    bar.querySelectorAll('button[data-bulk]').forEach((b) => { b.disabled = appsSelected.size === 0; });
    document.getElementById('appsSelectBtn').classList.toggle('active', appsSelectMode);
  }

  function showBulkMenu(x, y) {
    hideAppMenu();
    const n = appsSelected.size;
    const menu = document.getElementById('appsMenu');
    appsMenuApp = { package: '' };
    menu.innerHTML = `<div class="apps-menu-head"><div class="apps-menu-name">${n} apps selected</div></div>`
      + [['block', `Block ${n} apps…`], ['unblock', `Unblock ${n} apps`], ['pin', `Protect ${n} apps with PIN…`], ['unpin', `Remove the PIN from ${n} apps`]]
        .map(([bulk, text]) => `<button data-bulk="${bulk}">${escapeHtml(text)}</button>`).join('')
      + '<button class="group-start" data-bulk="clear">Clear the selection</button>';
    menu.querySelectorAll('button[data-bulk]').forEach((b) => {
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        hideAppMenu();
        if (b.dataset.bulk === 'clear') clearSelection();
        else runBulk(b.dataset.bulk);
      });
    });
    menu.style.display = 'block';
    const w = menu.offsetWidth;
    const h = menu.offsetHeight;
    menu.style.left = Math.max(8, Math.min(x, window.innerWidth - w - 8)) + 'px';
    menu.style.top = Math.max(8, y + h > window.innerHeight - 8 ? y - h : y) + 'px';
  }

  /** One action on every selected app, one after the other, with one question first. */
  async function runBulk(kind) {
    const d = appsDevice;
    const apps = selectedApps();
    if (!d || !apps.length) return;
    const n = apps.length;
    const names = apps.slice(0, 12).map((a) => a.label).join(', ') + (n > 12 ? `, and ${n - 12} more` : '');
    if (kind === 'block') {
      const fallback = apps.some((a) => a.system)
        ? ' Apps that came with the phone and that Android does not allow to block are turned off instead; Unblock brings them back.'
        : '';
      const ok = await showConfirm(`Block ${n} apps?`,
        `${names} stay on "${d.alias}", greyed out, and do not open until they are unblocked from here. Their data is kept.${fallback}`, 'Block');
      if (!ok) return;
    }
    if (kind === 'pin' || kind === 'unpin') {
      if (!(await pinSupported())) {
        showNotice('PIN protection', PIN_NEEDS_NEW_APP);
        return;
      }
      if (kind === 'pin' && !appsLock.pin && !(await setPinFor(d, 'Choose a PIN for this phone'))) return;
    }
    const failures = [];
    let done = 0;
    for (const a of apps) {
      if (appsDevice !== d) return;
      setAppsStatus(`Working on ${a.label} (${done + 1} of ${n})…`, 'busy');
      let result;
      if (kind === 'pin' || kind === 'unpin') {
        result = await callBackend('app_pin', {
          target: appsTarget(), alias: d.alias, package: a.package, label: a.label, action: kind === 'pin' ? 'protect' : 'unprotect',
        });
      } else {
        result = await callBackend('app_action', {
          target: appsTarget(), alias: d.alias, package: a.package, label: a.label, action: kind, activity: a.activity || null,
        });
      }
      if (!result || typeof result !== 'object' || !result.ok) {
        failures.push(`${a.label}: ${(result && result.message) || String(result || 'the phone did not answer.')}`);
      }
      done += 1;
    }
    if (appsDevice !== d) return;
    appsSelected.clear();
    appsSelectMode = false;
    await loadApps();
    const verb = { block: 'blocked', unblock: 'unblocked', pin: 'protected with the PIN', unpin: 'no longer protected with the PIN' }[kind];
    const okCount = n - failures.length;
    setAppsStatus(`${okCount} of ${n} apps ${verb}.`, failures.length ? 'error' : '');
    if (failures.length) showNotice(`${failures.length} of ${n} could not be done`, failures.join('\n\n'));
  }

  document.getElementById('appsSelectBtn').addEventListener('click', () => {
    if (appsSelectMode || appsSelected.size) {
      clearSelection();
    } else {
      appsSelectMode = true;
      renderApps();
    }
  });
  document.getElementById('appsSelDone').addEventListener('click', clearSelection);
  document.getElementById('appsSelAll').addEventListener('click', () => {
    document.querySelectorAll('#appsGrid .app-tile').forEach((t) => appsSelected.add(t.title));
    renderApps();
  });
  document.querySelectorAll('#appsSelBar button[data-bulk]').forEach((b) => {
    b.addEventListener('click', () => runBulk(b.dataset.bulk));
  });

  // ---------- Several phones: inventory, profiles, install, the watcher's alerts ----------
  function csvCell(value) {
    const text = String(value == null ? '' : value);
    return /[",\n;]/.test(text) ? '"' + text.replace(/"/g, '""') + '"' : text;
  }

  function stampForFile() {
    const n = new Date();
    const two = (x) => String(x).padStart(2, '0');
    return `${n.getFullYear()}-${two(n.getMonth() + 1)}-${two(n.getDate())}-${two(n.getHours())}${two(n.getMinutes())}`;
  }

  function safeForFile(text) {
    return String(text).replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40) || 'phone';
  }

  /** Reads the apps of each phone and saves them all in one CSV (Documents\Mirryl\Inventory). */
  async function exportInventory(phones) {
    const connected = phones.filter((d) => deviceStatus(d) === 'connected');
    if (!connected.length) {
      showNotice('Export apps', 'None of these phones is connected.');
      return;
    }
    const rows = [['Phone', 'App', 'Package', 'Version', 'Came with the phone', 'State', 'PIN', 'Installed', 'Updated'].map(csvCell).join(',')];
    const problems = phones.filter((d) => deviceStatus(d) !== 'connected').map((d) => `${d.alias}: not connected, skipped`);
    const date = (ms) => (ms ? new Date(ms).toISOString().slice(0, 10) : '');
    for (const d of connected) {
      setAppsStatusIfShown(`Reading the apps of ${d.alias}…`, 'busy');
      const result = await callBackend('list_apps', { target: findLiveAddress(d) || d.host_port });
      if (!result || typeof result !== 'object' || !Array.isArray(result.apps)) {
        problems.push(`${d.alias}: ${String(result || 'did not answer').replace(/^Error:\s*/, '').split('\n')[0]}`);
        continue;
      }
      for (const a of result.apps) {
        const state = a.suspended ? 'Blocked' : (a.enabled ? 'Active' : 'Off');
        rows.push([d.alias, a.label, a.package, a.version, a.system ? 'yes' : 'no', state, a.locked ? 'yes' : 'no', date(a.installed), date(a.updated)].map(csvCell).join(','));
      }
    }
    setAppsStatusIfShown('');
    const name = connected.length === 1 ? `mirryl-apps-${safeForFile(connected[0].alias)}-${stampForFile()}.csv` : `mirryl-apps-${connected.length}-phones-${stampForFile()}.csv`;
    const saved = await callBackend('save_inventory', { fileName: name, csv: rows.join('\r\n') + '\r\n' });
    const message = saved && typeof saved === 'object' ? saved.message : String(saved || '');
    showNotice(saved && saved.ok ? 'Apps exported' : 'The apps were not exported',
      [message].concat(problems).join('\n'));
  }

  function setAppsStatusIfShown(text, kind) {
    if (appsViewActive()) setAppsStatus(text, kind);
  }

  async function installOnPhones(phones) {
    const connected = phones.filter((d) => deviceStatus(d) === 'connected');
    if (!connected.length) {
      showNotice('Install APK', 'None of these phones is connected.');
      return;
    }
    const paths = await callBackend('pick_apks');
    if (!Array.isArray(paths) || !paths.length) return;
    const lines = phones.filter((d) => deviceStatus(d) !== 'connected').map((d) => `${d.alias}: not connected, skipped`);
    let good = 0;
    for (const d of connected) {
      const results = await callBackend('install_apks', { target: findLiveAddress(d) || d.host_port, alias: d.alias, paths });
      if (!Array.isArray(results)) {
        lines.push(`${d.alias}: ${String(results || 'did not answer')}`);
        continue;
      }
      const failedOnes = results.filter((r) => !r.ok);
      if (!failedOnes.length) good += 1;
      lines.push(failedOnes.length
        ? `${d.alias}: ${failedOnes.map((r) => r.message.split('\n')[0]).join(' / ')}`
        : `${d.alias}: installed`);
    }
    showNotice(good === connected.length ? 'Installed' : `Installed on ${good} of ${connected.length}`, lines.join('\n'));
  }

  /** Asks for a name in Mirryl's own dialog. Resolves to the text, or null. */
  function askName(title, body, value) {
    return new Promise((resolve) => {
      const overlay = document.getElementById('nameOverlay');
      const input = document.getElementById('nameInput');
      const error = document.getElementById('nameError');
      document.getElementById('nameTitle').textContent = title;
      document.getElementById('nameBody').textContent = body;
      input.value = value || '';
      error.textContent = '';
      overlay.classList.add('active');
      setTimeout(() => { input.focus(); input.select(); }, 0);
      const okBtn = document.getElementById('nameOkBtn');
      const cancelBtn = document.getElementById('nameCancelBtn');
      const close = (v) => {
        overlay.classList.remove('active');
        okBtn.removeEventListener('click', ok);
        cancelBtn.removeEventListener('click', cancel);
        input.removeEventListener('keydown', key);
        resolve(v);
      };
      const ok = () => {
        const v = input.value.trim();
        if (!v) { error.textContent = 'Type a name first.'; return; }
        close(v);
      };
      const cancel = () => close(null);
      const key = (e) => {
        error.textContent = '';
        if (e.key === 'Enter') ok();
        if (e.key === 'Escape') cancel();
      };
      okBtn.addEventListener('click', ok);
      cancelBtn.addEventListener('click', cancel);
      input.addEventListener('keydown', key);
    });
  }

  const TURNED_OFF_BY_BLOCK = ['com.android.settings', 'com.android.vending'];
  const INSTALLER_PACKAGES = ['com.android.packageinstaller', 'com.google.android.packageinstaller', 'com.samsung.android.packageinstaller'];

  /** Saves what this phone has blocked and behind the PIN as a profile. */
  async function saveProfileFrom(d) {
    if (!appsList.length) await loadApps();
    if (!appsList.length) {
      showNotice('Save as profile', 'The apps of this phone could not be read.');
      return;
    }
    // Blocked, or turned off by Block because Android would not block it
    // (Settings, the Play Store). Apps that came turned off from the factory
    // are not part of what the person chose.
    const block = appsList.filter((a) => a.suspended || (!a.enabled && TURNED_OFF_BY_BLOCK.includes(a.package)))
      .map((a) => ({ package: a.package, label: a.label }));
    const pin = appsList.filter((a) => a.locked && !INSTALLER_PACKAGES.includes(a.package))
      .map((a) => ({ package: a.package, label: a.label }));
    if (!block.length && !pin.length) {
      showNotice('Save as profile', `"${d.alias}" has no app blocked or protected with the PIN. Block or protect the apps the profile should have, then save it.`);
      return;
    }
    const name = await askName('Save as profile',
      `${block.length} blocked and ${pin.length} with the PIN, as "${d.alias}" has them now. Give the profile a name.`, '');
    if (!name) return;
    const profiles = await callBackend('get_profiles');
    const list = Array.isArray(profiles) ? profiles : [];
    const existing = list.findIndex((p) => p.name.toLowerCase() === name.toLowerCase());
    if (existing >= 0 && !(await showConfirm('Replace this profile?', `There is already a profile called "${list[existing].name}". It is replaced by this one.`, 'Replace'))) return;
    const profile = { name, block, pin };
    if (existing >= 0) list[existing] = profile; else list.push(profile);
    const saved = await callBackend('save_profiles', { profiles: list });
    if (typeof saved === 'string' && saved) showNotice('The profile was not saved', saved.replace(/^Error:\s*/, ''));
    else showNotice('Profile saved', `"${name}": ${block.length} apps blocked and ${pin.length} with the PIN. Apply it from a phone's page or to several phones from the list.`);
  }

  /** Lets the person pick a profile (and delete ones). Resolves to {profile, exact} or null. */
  function chooseProfile(title, body) {
    return new Promise(async (resolve) => {
      let profiles = await callBackend('get_profiles');
      profiles = Array.isArray(profiles) ? profiles : [];
      if (!profiles.length) {
        showNotice(title, 'There are no profiles yet. Open a phone, block or protect the apps the profile should have, and press "Save as profile…" on its page.');
        resolve(null);
        return;
      }
      const overlay = document.getElementById('profileOverlay');
      const listEl = document.getElementById('profileList');
      const error = document.getElementById('profileError');
      document.getElementById('profileTitle').textContent = title;
      document.getElementById('profileBody').textContent = body;
      document.getElementById('profileExact').checked = false;
      error.textContent = '';
      let chosen = 0;
      const draw = () => {
        listEl.innerHTML = '';
        profiles.forEach((p, i) => {
          const row = document.createElement('div');
          row.className = 'profile-row' + (i === chosen ? ' active' : '');
          row.innerHTML = '<div style="flex:1"><div class="profile-name"></div><div class="profile-sub"></div></div><button class="ghost" title="Delete this profile">Delete</button>';
          row.querySelector('.profile-name').textContent = p.name;
          row.querySelector('.profile-sub').textContent = `${p.block.length} blocked · ${p.pin.length} with PIN`;
          row.addEventListener('click', () => { chosen = i; draw(); });
          row.querySelector('button').addEventListener('click', async (e) => {
            e.stopPropagation();
            profiles.splice(i, 1);
            await callBackend('save_profiles', { profiles });
            if (!profiles.length) { close(null); return; }
            chosen = 0;
            draw();
          });
          listEl.appendChild(row);
        });
      };
      const okBtn = document.getElementById('profileApplyBtn');
      const cancelBtn = document.getElementById('profileCancelBtn');
      const close = (v) => {
        overlay.classList.remove('active');
        okBtn.removeEventListener('click', ok);
        cancelBtn.removeEventListener('click', cancel);
        resolve(v);
      };
      const ok = () => close({ profile: profiles[chosen], exact: document.getElementById('profileExact').checked });
      const cancel = () => close(null);
      okBtn.addEventListener('click', ok);
      cancelBtn.addEventListener('click', cancel);
      draw();
      overlay.classList.add('active');
    });
  }

  async function applyProfileTo(phones) {
    const connected = phones.filter((d) => deviceStatus(d) === 'connected');
    if (!connected.length) {
      showNotice('Apply a profile', 'None of these phones is connected.');
      return;
    }
    const names = connected.map((d) => d.alias).join(', ');
    const choice = await chooseProfile('Apply a profile', `To: ${names}.`);
    if (!choice) return;
    const { profile, exact } = choice;
    const lines = phones.filter((d) => deviceStatus(d) !== 'connected').map((d) => `${d.alias}: not connected, skipped`);
    let pin = null;
    for (const d of connected) {
      setAppsStatusIfShown(`Applying "${profile.name}" to ${d.alias}…`, 'busy');
      const args = { target: findLiveAddress(d) || d.host_port, alias: d.alias, profile, exact, pin };
      let result = await callBackend('apply_profile', args);
      if (result && result.needs_pin) {
        // One PIN for every phone of this run that has none yet.
        if (!pin) pin = await askNewPin('Choose a PIN', `The profile protects apps with a PIN, and "${d.alias}" has none yet. The same PIN is set on every phone of this run that has none. ${PIN_FORGOTTEN}`);
        if (!pin) { lines.push(`${d.alias}: skipped, no PIN chosen`); continue; }
        result = await callBackend('apply_profile', { ...args, pin });
      }
      if (!result || typeof result !== 'object') {
        lines.push(`${d.alias}: ${String(result || 'did not answer')}`);
        continue;
      }
      lines.push(`${d.alias}: ${result.message}` + (result.lines && result.lines.length ? '\n   ' + result.lines.join('\n   ') : ''));
    }
    setAppsStatusIfShown('');
    if (appsDevice && connected.some((d) => d.id === appsDevice.id) && appsViewActive()) await loadApps();
    showNotice(`"${profile.name}" applied`, lines.join('\n'));
  }

  // The watcher writes a line when it finds a phone's PIN protection off and
  // turns it back on; each new one is said once.
  async function checkPinAlerts() {
    const lines = await callBackend('pin_alerts');
    if (!Array.isArray(lines)) return;
    let seen = 0;
    try { seen = parseInt(localStorage.getItem('mirrylPinAlertsSeen') || '0', 10) || 0; } catch (_) { /* no storage */ }
    if (lines.length < seen) seen = 0; // the log was trimmed or removed
    const fresh = lines.slice(seen);
    try { localStorage.setItem('mirrylPinAlertsSeen', String(lines.length)); } catch (_) { /* no storage */ }
    if (!fresh.length) return;
    // The watcher knows phones by serial; the person knows them by name.
    const named = fresh.slice(-5).map((line) => devices.reduce(
      (text, d) => (d.serial && text.includes(d.serial) ? text.split(d.serial).join(`${d.alias} (${d.serial})`) : text), line));
    showNotice('PIN protection was turned back on',
      'Someone turned off the PIN protection on a phone; Mirryl turned it back on.\n\n' + named.join('\n'));
  }
  if (isTauri) {
    setTimeout(checkPinAlerts, 4000);
    setInterval(checkPinAlerts, 60000);
  }

  document.getElementById('dmBulkInstallBtn').addEventListener('click', () => installOnPhones(selectedDevices()));
  document.getElementById('dmBulkProfileBtn').addEventListener('click', () => applyProfileTo(selectedDevices()));
  document.getElementById('dmBulkExportBtn').addEventListener('click', () => exportInventory(selectedDevices()));

  function hideAppMenu() {
    if (!appsMenuApp) return;
    appsMenuApp = null;
    document.getElementById('appsMenu').style.display = 'none';
    document.querySelectorAll('#appsGrid .app-tile.menu-open').forEach((t) => t.classList.remove('menu-open'));
  }

  function showAppMenu(a, x, y) {
    hideAppMenu();
    appsMenuApp = a;
    const menu = document.getElementById('appsMenu');
    const items = [];
    if (a.activity) items.push(['open', 'Open on the phone']);
    items.push(['play_store', 'Show it in the Play Store']);
    if (a.apks && a.apks.length) items.push(['backup', 'Save a copy of the APK']);
    // Blocking reaches every system app, protected or not; unblocking is
    // always there. The rest leaves the protected ones alone (apps.rs,
    // may_act, says the same on the other side).
    const blockLine = a.suspended ? ['unblock', 'Unblock'] : ['block', 'Block…'];
    if (!a.protected) {
      items.push(['force_stop', 'Stop it', 'group-start']);
      items.push(blockLine);
      items.push(a.enabled ? ['disable', 'Turn off…'] : ['enable', 'Turn on']);
      items.push(['clear_data', 'Delete its data…', 'group-start danger']);
      if (!a.system) items.push(['uninstall', 'Uninstall…', 'danger']);
    } else if (a.suspended || !a.enabled) {
      // Blocked, or turned off by Block because Android would not block it.
      items.push(['unblock', 'Unblock', 'group-start']);
    } else if (a.system) {
      items.push(['block', 'Block…', 'group-start']);
    }
    // The PIN: an app asks for it before opening; blocking stays as it was.
    items.push(a.locked ? ['unpin', 'Remove the PIN', 'group-start'] : ['pin', 'Protect with PIN…', 'group-start']);
    const note = a.protected
      ? (a.system
        ? 'The phone needs this one to keep working, so from here it can only be opened or blocked.'
        : 'Mirryl needs this one to reach the phone, so it can only be opened from here.')
      : a.system
        ? 'It came with the phone, so it is turned off instead of uninstalled.'
        : '';
    menu.innerHTML = `<div class="apps-menu-head"><div class="apps-menu-name"></div><div class="apps-menu-sub"></div></div>`
      + items.map(([action, text, cls]) => `<button data-action="${action}" class="${cls || ''}">${escapeHtml(text)}</button>`).join('')
      + (note ? '<div class="apps-menu-note"></div>' : '');
    menu.querySelector('.apps-menu-name').textContent = a.label;
    menu.querySelector('.apps-menu-sub').textContent = a.package + (a.version ? ' · ' + a.version : '');
    if (note) menu.querySelector('.apps-menu-note').textContent = note;
    menu.querySelectorAll('button[data-action]').forEach((b) => {
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        doAppAction(a, b.dataset.action);
      });
    });
    menu.style.display = 'block';
    // Inside the window, whichever corner of the pointer it fits from.
    const w = menu.offsetWidth;
    const h = menu.offsetHeight;
    menu.style.left = Math.max(8, Math.min(x, window.innerWidth - w - 8)) + 'px';
    menu.style.top = Math.max(8, y + h > window.innerHeight - 8 ? y - h : y) + 'px';
    document.querySelectorAll('#appsGrid .app-tile').forEach((t) => {
      if (t.title === a.package) t.classList.add('menu-open');
    });
  }

  document.addEventListener('click', (e) => {
    if (appsMenuApp && !e.target.closest('#appsMenu')) hideAppMenu();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (appsMenuApp) hideAppMenu();
    else if (appsViewActive() && (appsSelected.size || appsSelectMode)) clearSelection();
  });
  document.getElementById('homePane').addEventListener('scroll', hideAppMenu);

  async function installApks(paths) {
    const apks = (paths || []).filter((p) => /\.apk$/i.test(p));
    if (!appsDevice) return;
    if (!apks.length) {
      showNotice('Nothing to install', 'Only .apk files can be installed from here.');
      return;
    }
    const device = appsDevice;
    const names = apks.map((p) => p.split(/[\\/]/).pop());
    setAppsStatus(`Installing ${names.join(', ')} on ${device.alias}… look at the phone in case it asks to confirm.`, 'busy');
    const results = await callBackend('install_apks', { target: appsTarget(), alias: device.alias, paths: apks });
    if (appsDevice !== device) return;
    if (!Array.isArray(results)) {
      setAppsStatus(String(results || 'The phone did not answer.').replace(/^Error:\s*/, ''), 'error');
      return;
    }
    const failedOnes = results.filter((r) => !r.ok);
    setAppsStatus(failedOnes.length
      ? `${results.length - failedOnes.length} of ${results.length} installed.`
      : `${results.length === 1 ? names[0] + ' was' : results.length + ' apps were'} installed.`, failedOnes.length ? 'error' : '');
    if (failedOnes.length) showNotice('Not everything was installed', failedOnes.map((r) => r.message).join('\n\n'));
    if (results.length > failedOnes.length) loadApps();
  }

  document.getElementById('appsInstallBtn').addEventListener('click', async () => {
    const paths = await callBackend('pick_apks');
    if (Array.isArray(paths) && paths.length) installApks(paths);
  });
  document.getElementById('appsRefreshBtn').addEventListener('click', loadApps);
  document.getElementById('appsSearch').addEventListener('input', renderApps);
  document.querySelectorAll('#appsFilter button').forEach((b) => {
    b.addEventListener('click', () => {
      appsFilter = b.dataset.filter;
      document.querySelectorAll('#appsFilter button').forEach((x) => x.classList.toggle('active', x === b));
      renderApps();
    });
  });

  async function searchPlayStore() {
    const input = document.getElementById('appsStoreQuery');
    if (!appsDevice || !input.value.trim()) return;
    const result = await callBackend('play_store_search', { target: appsTarget(), query: input.value });
    if (result && typeof result === 'object') setAppsStatus(result.message, result.ok ? '' : 'error');
  }
  document.getElementById('appsStoreBtn').addEventListener('click', searchPlayStore);
  document.getElementById('appsStoreQuery').addEventListener('keydown', (e) => { if (e.key === 'Enter') searchPlayStore(); });

  // Files dropped on the window. Tauri hands over their paths (the page's own
  // drop event has none); only while the Apps screen is in front.
  if (isTauri && window.__TAURI__.event) {
    const drop = document.getElementById('appsDrop');
    const showDrop = (on) => drop.classList.toggle('active', on && appsViewActive() && !!appsDevice);
    window.__TAURI__.event.listen('tauri://drag-enter', () => showDrop(true));
    window.__TAURI__.event.listen('tauri://drag-over', () => showDrop(true));
    window.__TAURI__.event.listen('tauri://drag-leave', () => showDrop(false));
    window.__TAURI__.event.listen('tauri://drag-drop', (event) => {
      const wasActive = drop.classList.contains('active');
      showDrop(false);
      if (wasActive) installApks((event.payload && event.payload.paths) || []);
    });
  }

  // ---------- Licenses / About ----------
  document.getElementById('backToDevicesLink').addEventListener('click',
    () => showView('view-home'));
  // The only address Mirryl ever opens; the Rust command holds the URL.
  document.getElementById('repoLink').addEventListener('click', () => callBackend('open_repository'));

  // Filled from the backend rather than written into the HTML, so it can
  // never go stale.
  (async () => {
    const version = await callBackend('app_version');
    if (typeof version === 'string' && version) {
      document.getElementById('aboutVersion').textContent = 'Version ' + version;
      appVersion = version;
      renderStatusBar();
    }
  })();

  let licenseTextLoaded = false;
  document.getElementById('viewFullLicenseLink').addEventListener('click', async () => {
    showView('view-license-full');
    if (!licenseTextLoaded) {
      const box = document.getElementById('licenseFullText');
      box.textContent = await callBackend('get_third_party_licenses');
      licenseTextLoaded = true;
    }
  });
  document.getElementById('backFromLicenseFullLink').addEventListener('click', () => showView('view-licenses'));
  document.getElementById('mplSourceLink').addEventListener('click', () => callBackend('open_mpl_source_folder'));
  document.getElementById('componentInventoryLink').addEventListener('click', () => callBackend('open_component_inventory_folder'));
  document.getElementById('eulaLink').addEventListener('click', () => callBackend('open_eula_folder'));
  document.getElementById('privacyLink').addEventListener('click', () => callBackend('open_privacy_folder'));
  document.getElementById('userGuideLink').addEventListener('click', async () => {
    const result = await callBackend('open_user_guide');
    if (typeof result === 'string' && result.startsWith('Error')) showNotice('User guide', result.replace(/^Error:\s*/, ''));
  });

  // ---------- Mirror tabs ----------
  // Every open mirror is a tab next to Home. The engine runs as its own
  // process per phone (see spawn_in_tab in main.rs), opens with no window, and
  // the app shows its page here, in an iframe. The
  // tab can send the mirror to a window of its own and take it back, and the
  // session - the connection, the agent on the phone - carries on throughout.
  //
  // States: 'starting' (connecting, the engine has not answered yet), 'live'
  // (the picture is in the tab), 'switching' (moving between tab and window),
  // 'window' (in its own window) and 'closed' (the engine ended; the tab stays
  // to say why until it is closed or opened again).
  const mirrorTabs = new Map(); // device key (String(d.id)) -> tab
  let activeTabKey = 'home';
  let appVersion = '';

  const POP_OUT_ICON = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3.5" y="8.5" width="12" height="12" rx="2"/><path d="M9.5 3.5h11v11"/></svg>';
  const BRING_BACK_ICON = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 4 10 14"/><path d="M10 7v7h7"/><path d="M4 11v9h9"/></svg>';
  const FILL_ICON = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/></svg>';
  const CLOSE_ICON = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M6 6l12 12M18 6 6 18"/></svg>';

  /**
   * Puts the keyboard inside the mirror. Focusing the <iframe> element alone
   * is not enough: the parent keeps the keys and none reach the phone (the
   * frame is the active element while its own document says it has no
   * focus). Focusing the frame's window is what moves them in.
   */
  function focusFrame(t) {
    if (!t || !t.frame) return;
    t.frame.focus();
    try { t.frame.contentWindow && t.frame.contentWindow.focus(); } catch (_) { /* gone meanwhile */ }
  }

  function activateTab(key) {
    activeTabKey = mirrorTabs.has(key) ? key : 'home';
    const home = activeTabKey === 'home';
    document.getElementById('homeTab').classList.toggle('active', home);
    document.getElementById('homePane').classList.toggle('active', home);
    mirrorTabs.forEach((t) => {
      const on = t.key === activeTabKey;
      t.tabEl.classList.toggle('active', on);
      t.pane.classList.toggle('active', on);
    });
    const t = mirrorTabs.get(activeTabKey);
    if (t) {
      t.tabEl.scrollIntoView({ block: 'nearest', inline: 'nearest' });
      // Keys go to the phone only while the picture has the focus.
      if (t.frame) setTimeout(() => focusFrame(t), 0);
    }
    renderStatusBar();
  }
  // Home is the first icon of the bar on the left; the row of tabs is only
  // for mirrors. It always goes to the main screen, from a mirror or from any
  // other screen.
  document.getElementById('homeTab').addEventListener('click', () => showView('view-home'));

  /** Creates the tab for a device in 'starting' and brings it to the front. */
  function openMirrorTab(d) {
    const key = String(d.id);
    const tabEl = document.createElement('div');
    tabEl.className = 'tab mirror-tab';
    tabEl.setAttribute('role', 'tab');
    tabEl.innerHTML = `
      <span class="tab-dot"></span>
      <span class="tab-label"></span>
      <button class="tab-btn tab-fill" title="Fill the space (cuts the edges) - press again for the whole screen" aria-label="Fill the space">${FILL_ICON}</button>
      <button class="tab-btn tab-pop"></button>
      <button class="tab-btn tab-close" title="Close this mirror" aria-label="Close this mirror">${CLOSE_ICON}</button>`;
    const pane = document.createElement('div');
    pane.className = 'pane mirror-pane';
    pane.innerHTML = '<div class="mirror-overlay"></div>';
    const t = {
      key, deviceId: d.id, alias: d.alias, state: 'starting',
      url: '', error: '', closing: false, frame: null, tabEl, pane,
    };
    tabEl.addEventListener('click', (e) => {
      if (e.target.closest('.tab-btn')) return;
      activateTab(key);
    });
    // Middle-click closes, as tabs do everywhere else.
    tabEl.addEventListener('auxclick', (e) => { if (e.button === 1) closeMirrorTab(t); });
    tabEl.querySelector('.tab-close').addEventListener('click', () => closeMirrorTab(t));
    // The page is another origin: ask it by message, and give it the keyboard
    // back afterwards.
    tabEl.querySelector('.tab-fill').addEventListener('click', () => {
      if (t.frame && t.frame.contentWindow) t.frame.contentWindow.postMessage({ mirryl: 'toggle-fill' }, '*');
      focusFrame(t);
    });
    tabEl.querySelector('.tab-pop').addEventListener('click', () => {
      if (t.state === 'live') popOut(t);
      else if (t.state === 'window') bringBack(t);
    });
    pane.querySelector('.mirror-overlay').addEventListener('click', (e) => {
      const action = e.target.closest('[data-action]')?.dataset.action;
      if (action === 'bring-back') bringBack(t);
      else if (action === 'show-window') callBackend('mirror_command', { key, command: 'window' });
      else if (action === 'reopen') reopenMirror(t);
      else if (action === 'close') closeMirrorTab(t);
    });
    document.getElementById('mirrorTabs').appendChild(tabEl);
    document.getElementById('panes').appendChild(pane);
    mirrorTabs.set(key, t);
    renderMirrorTab(t);
    activateTab(key);
    return t;
  }

  function renderMirrorTab(t) {
    t.tabEl.dataset.state = t.state;
    t.pane.dataset.state = t.state;
    t.tabEl.querySelector('.tab-label').textContent = t.alias;
    t.tabEl.title = t.alias;
    const pop = t.tabEl.querySelector('.tab-pop');
    const inWindow = t.state === 'window';
    pop.innerHTML = inWindow ? BRING_BACK_ICON : POP_OUT_ICON;
    pop.title = inWindow ? 'Bring it back to this tab' : 'Open in its own window';
    pop.setAttribute('aria-label', pop.title);
    pop.disabled = !(t.state === 'live' || inWindow);
    t.tabEl.querySelector('.tab-fill').disabled = t.state !== 'live';

    const overlay = t.pane.querySelector('.mirror-overlay');
    const name = escapeHtml(t.alias);
    if (t.state === 'starting') {
      overlay.innerHTML = `<div class="mirror-card"><div class="status-dot-spin"></div>
        <div class="mirror-card-title">Connecting to ${name}…</div>
        <div class="mirror-card-body">The picture appears here in a second or two.</div></div>`;
    } else if (t.state === 'switching') {
      overlay.innerHTML = `<div class="mirror-card"><div class="status-dot-spin"></div>
        <div class="mirror-card-title">Moving ${name}…</div></div>`;
    } else if (inWindow) {
      overlay.innerHTML = `<div class="mirror-card">
        <div class="mirror-card-title">${name} is open in its own window</div>
        <div class="mirror-card-body">Closing that window ends the mirror. To have it here again, bring it back.</div>
        <div class="mirror-card-buttons">
          <button data-action="bring-back">Bring it back here</button>
          <button class="secondary" data-action="show-window">Show the window</button>
        </div></div>`;
    } else if (t.state === 'closed') {
      overlay.innerHTML = `<div class="mirror-card">
        <div class="mirror-card-title">The mirror of ${name} closed</div>
        <div class="mirror-card-body">${t.error ? escapeHtml(t.error) : 'The phone stopped answering, or the mirror was closed.'}</div>
        <div class="mirror-card-buttons">
          <button data-action="reopen">Open it again</button>
          <button class="secondary" data-action="close">Close this tab</button>
        </div></div>`;
    } else {
      overlay.innerHTML = '';
    }
    if (activeTabKey === t.key) renderStatusBar();
  }

  /** Puts the engine's page in the tab. */
  function attachFrame(t) {
    if (t.frame || !t.url) return;
    const frame = document.createElement('iframe');
    frame.className = 'mirror-frame';
    // autoplay: the phone's sound. The page is the engine's own, on
    // 127.0.0.1; it reaches nothing of this app's.
    frame.allow = 'autoplay; clipboard-write';
    frame.title = `${t.alias} mirror`;
    frame.src = t.url;
    frame.addEventListener('load', () => {
      if (activeTabKey === t.key) focusFrame(t);
    });
    t.pane.appendChild(frame);
    t.frame = frame;
  }

  /** Takes the page out of the tab, which ends its streams on this side. */
  function detachFrame(t) {
    if (!t.frame) return;
    t.frame.src = 'about:blank';
    t.frame.remove();
    t.frame = null;
  }

  function removeMirrorTab(t) {
    detachFrame(t);
    t.tabEl.remove();
    t.pane.remove();
    mirrorTabs.delete(t.key);
    if (activeTabKey === t.key) {
      const keys = [...mirrorTabs.keys()];
      activateTab(keys.length ? keys[keys.length - 1] : 'home');
    }
    renderDeviceList();
  }

  async function popOut(t) {
    if (t.state !== 'live') return;
    // The tab lets go of the page first: the window opens on the same page,
    // and each stream has room for one viewer.
    detachFrame(t);
    t.state = 'switching';
    renderMirrorTab(t);
    const result = await callBackend('mirror_command', { key: t.key, command: 'window' });
    if (result === 'gone') markMirrorClosed(t, '');
  }

  async function bringBack(t) {
    if (t.state !== 'window') return;
    t.state = 'switching';
    renderMirrorTab(t);
    activateTab(t.key);
    const result = await callBackend('mirror_command', { key: t.key, command: 'tab' });
    if (result === 'gone') markMirrorClosed(t, '');
  }

  async function closeMirrorTab(t) {
    if (t.state === 'closed') {
      removeMirrorTab(t);
      return;
    }
    // The engine's own teardown takes the agent off the phone; the tab goes
    // now and the 'exit' that follows finds nothing left to update.
    t.closing = true;
    removeMirrorTab(t);
    await callBackend('mirror_command', { key: t.key, command: 'close' });
  }

  function markMirrorClosed(t, reason) {
    detachFrame(t);
    t.state = 'closed';
    t.error = reason || '';
    renderMirrorTab(t);
  }

  function reopenMirror(t) {
    const d = devices.find((x) => String(x.id) === t.key);
    removeMirrorTab(t);
    if (d) connectAndMirror(d);
  }

  function onMirrorEvent(payload) {
    if (!payload || typeof payload.key !== 'string') return;
    const { key, kind, detail } = payload;
    const t = mirrorTabs.get(key);
    if (kind === 'exit') {
      openMirrors.delete(key);
      mirrorKind.delete(key);
    }
    if (!t || t.closing) {
      // A mirror whose tab was closed while it was still connecting: it came
      // up with nowhere to show it, so it goes.
      if (kind === 'ready') callBackend('mirror_command', { key, command: 'close' });
      if (kind === 'exit') renderDeviceList();
      return;
    }
    if (kind === 'ready') {
      t.url = detail;
      t.state = 'live';
      connectingUntil.delete(t.deviceId);
      attachFrame(t);
    } else if (kind === 'window') {
      t.state = 'window';
      detachFrame(t);
    } else if (kind === 'tab') {
      t.state = 'live';
      attachFrame(t);
      if (activeTabKey === t.key) focusFrame(t);
    } else if (kind === 'window-failed') {
      showNotice("Couldn't open its own window", detail || 'Unknown error.');
    } else if (kind === 'exit') {
      // Closing its own window with the X is how a mirror out of its tab is
      // closed for good, so its tab goes with it.
      if (t.state === 'window' || (t.state === 'switching' && !t.frame)) {
        removeMirrorTab(t);
        return;
      }
      connectingUntil.delete(t.deviceId);
      markMirrorClosed(t, detail);
    }
    renderMirrorTab(t);
    renderDeviceList();
  }

  if (isTauri && window.__TAURI__.event) {
    window.__TAURI__.event.listen('mirror-event', (event) => onMirrorEvent(event.payload));
  }

  // Tab keys, while the focus is on the app rather than in a mirror (inside a
  // mirror, every key goes to the phone).
  window.addEventListener('keydown', (event) => {
    if (capturingAction || !event.ctrlKey) return;
    const order = ['home', ...mirrorTabs.keys()];
    if (event.key === 'Tab') {
      event.preventDefault();
      const at = order.indexOf(activeTabKey);
      const step = event.shiftKey ? -1 : 1;
      activateTab(order[(at + step + order.length) % order.length]);
    } else if (event.key.toLowerCase() === 'w' && mirrorTabs.has(activeTabKey)) {
      event.preventDefault();
      closeMirrorTab(mirrorTabs.get(activeTabKey));
    }
  });

  // ---------- Status bar ----------
  function renderStatusBar() {
    const connected = devices.filter((d) => deviceStatus(d) === 'connected').length;
    document.getElementById('statusDevices').textContent = devices.length === 0
      ? 'No devices'
      : `${devices.length} device${devices.length === 1 ? '' : 's'} · ${connected} connected`;
    const t = mirrorTabs.get(activeTabKey);
    let active = '';
    if (t) {
      const kind = mirrorKind.get(t.key);
      const link = kind === 'usb' ? ' over USB' : kind === 'wifi' ? ' over Wi-Fi' : '';
      const state = {
        starting: 'connecting', switching: 'moving', live: `mirroring${link}`,
        window: `in its own window${link}`, closed: 'closed',
      }[t.state] || '';
      active = `${t.alias} · ${state}`;
    }
    document.getElementById('statusActive').textContent = active;
    document.getElementById('statusVersion').textContent = appVersion ? `Mirryl ${appVersion}` : '';
  }

  // ---------- Activity bar and Home ----------
  document.getElementById('actAdd').addEventListener('click', openWizard);
  document.getElementById('actLicenses').addEventListener('click', () => showView('view-licenses'));
  document.getElementById('homeAddCard').addEventListener('click', openWizard);
  document.getElementById('homeSettingsCard').addEventListener('click', () => showView('view-settings'));
  document.getElementById('homeLicensesCard').addEventListener('click', () => showView('view-licenses'));

  // ---------- Initial render ----------
  showView('view-home');
  goToStep(1);
  // Reconcile on startup: clear any phantom adb connections left over from a
  // previous session so the LIVE STATUS opens in sync with the device list.
  loadDevices(true);
  // Once per launch, in the background: a read-only check that needs no
  // administrator, and catches missing firewall rules and a watcher pointing
  // at a deleted folder (or at another copy's watcher.ps1, or registered with
  // RunLevel Highest).
  refreshSystemHealth();
}
