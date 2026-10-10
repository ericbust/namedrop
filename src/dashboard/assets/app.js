'use strict';

// Every node is built through el()/svg(), which only ever set textContent.
// Collaborator text is untrusted; it must never reach an HTML parser.

const SVG_NS = 'http://www.w3.org/2000/svg';

function append(node, children) {
  for (const child of Array.isArray(children) ? children : [children]) {
    if (child == null || child === false) continue;
    if (Array.isArray(child)) append(node, child);
    else if (child instanceof Node) node.appendChild(child);
    else node.appendChild(document.createTextNode(String(child)));
  }
  return node;
}

function setAttrs(node, attrs) {
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value == null || value === false) continue;
    if (key === 'class') node.setAttribute('class', Array.isArray(value) ? value.filter(Boolean).join(' ') : value);
    else if (key === 'text') node.textContent = value;
    else if (key === 'data') Object.assign(node.dataset, value);
    else if (key === 'css') for (const [prop, v] of Object.entries(value)) node.style.setProperty(prop, v);
    else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  return node;
}

function el(tag, attrs, ...children) {
  return append(setAttrs(document.createElement(tag), attrs), children);
}

function svg(tag, attrs, ...children) {
  return append(setAttrs(document.createElementNS(SVG_NS, tag), attrs), children);
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
  return node;
}

// ---------- icons (16x16 strokes, colored by currentColor) ----------

const ICON_PATHS = {
  queued: [['circle', { cx: 8, cy: 8, r: 4.5 }]],
  delivered: [['circle', { cx: 8, cy: 8, r: 4.5 }], ['path', { d: 'M8 3.5a4.5 4.5 0 0 1 0 9z', class: 'fill' }]],
  working: [['circle', { cx: 8, cy: 8, r: 5 }], ['circle', { cx: 8, cy: 8, r: 2.2, class: 'fill' }]],
  replied: [['path', { d: 'M6.5 3.5 3 7l3.5 3.5M3.5 7H9a3.5 3.5 0 0 1 3.5 3.5V12.5' }]],
  completed: [['path', { d: 'm3.5 8.5 3 3 6-7' }]],
  held: [['path', { d: 'M6 4v8M10 4v8' }]],
  failed: [['path', { d: 'm4.5 4.5 7 7m0-7-7 7' }]],
  cancelled: [['circle', { cx: 8, cy: 8, r: 4.5 }], ['path', { d: 'm5 11 6-6' }]],
  alert: [['path', { d: 'M8 2.5 14 13H2z' }], ['path', { d: 'M8 6.5v3M8 11.2v.3' }]],
  clock: [['circle', { cx: 8, cy: 8, r: 5.5 }], ['path', { d: 'M8 5v3.2l2 1.3' }]],
  terminal: [['path', { d: 'm3.5 5 3 3-3 3M8.5 11.5h4' }]],
  arrow: [['path', { d: 'M3 8h10m-3.5-3.5L13 8l-3.5 3.5' }]],
  down: [['path', { d: 'M8 3v10m-4-4 4 4 4-4' }]],
  close: [['path', { d: 'm4 4 8 8m0-8-8 8' }]],
  thread: [['path', { d: 'M4 3.5v9M4 8h5.5a2.5 2.5 0 0 0 0-5H8' }]],
  inbox: [['path', { d: 'M2.5 9.5 4 4h8l1.5 5.5V12.5h-11zM2.5 9.5h3.2l.8 1.5h3l.8-1.5h3.2' }]],
};

function icon(name, cls) {
  const parts = (ICON_PATHS[name] || ICON_PATHS.queued).map(([tag, attrs]) => svg(tag, attrs));
  return svg('svg', { class: ['icon', 'icon-' + name, cls], viewBox: '0 0 16 16', 'aria-hidden': 'true' }, parts);
}

// ---------- domain tables: one row per wire enum value ----------

// Tones are the only state colors: flight (in progress), wait, alert (needs you), done, idle.
const LEG_STAGE = {
  queued: { label: 'queued', tone: 'idle' },
  delivered: { label: 'delivered', tone: 'flight' },
  working: { label: 'working on it', tone: 'flight' },
  replied: { label: 'replied', tone: 'done' },
  completed: { label: 'done', tone: 'done' },
  held: { label: 'held', tone: 'alert' },
  failed: { label: 'failed', tone: 'alert' },
  cancelled: { label: 'cancelled', tone: 'idle' },
};

const DELIVERY = {
  queued: { label: 'queued', tone: 'idle', glyph: null },
  dispatching: { label: 'dispatching', tone: 'idle', glyph: null },
  submitted: { label: 'delivered, unread', tone: 'flight', glyph: null },
  acknowledged: { label: 'working on it', tone: 'wait', glyph: null },
  settled: { label: 'settled', tone: 'done', glyph: null },
  held: { label: 'held', tone: 'alert', glyph: 'held' },
  failed: { label: 'failed', tone: 'alert', glyph: 'failed' },
  cancelled: { label: 'cancelled', tone: 'idle', glyph: 'cancelled' },
};

const ACTIVITY = {
  working: { label: 'working', tone: 'flight' },
  waiting: { label: 'waiting', tone: 'wait' },
  blocked: { label: 'blocked', tone: 'alert' },
  idle: { label: 'idle', tone: 'idle' },
  offline: { label: 'offline', tone: 'idle' },
  retired: { label: 'retired', tone: 'idle' },
};

const MESSAGE_KIND = {
  mention: 'Message',
  reply: 'Reply',
  handoff: 'Handoff',
  result_set: 'Result',
  nudge: 'Nudge',
  hook: 'Hook',
};

const HEALTH = {
  calm: () => ({ tone: 'done', label: 'All calm' }),
  busy: (h) => ({ tone: 'flight', label: `Busy · ${h.in_flight} in flight` }),
  needs_you: (h) => ({ tone: 'alert', label: h.items === 1 ? '1 needs you' : `${h.items} need you` }),
  paused: () => ({ tone: 'wait', label: 'Paused' }),
  inactive: () => ({ tone: 'idle', label: 'Inactive' }),
};

const CONNECTION = {
  connecting: { tone: 'idle', label: 'Connecting' },
  live: { tone: 'done', label: 'Live' },
  restarting: { tone: 'wait', label: 'Updating Namedrop…' },
  disconnected: { tone: 'alert', label: 'Disconnected' },
};

const RUNTIME = { 'claude-code': 'Claude Code', codex: 'Codex' };

// ---------- time ----------

const clock = { skew: 0 };
const serverNow = () => Date.now() - clock.skew;

function duration(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  return `${Math.floor(h / 24)} d`;
}

function ago(ms) {
  const d = serverNow() - ms;
  if (d < 10_000) return 'just now';
  return `${duration(d)} ago`;
}

function clockTime(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

// A live relative time; tick() rewrites every one of these in place.
function when(ms, prefix) {
  return el('time', { class: 'ago', datetime: new Date(ms).toISOString(), title: new Date(ms).toLocaleString(), data: { at: String(ms), prefix: prefix || '' } },
    (prefix || '') + ago(ms));
}

function tick() {
  for (const node of document.querySelectorAll('time.ago')) {
    node.textContent = node.dataset.prefix + ago(Number(node.dataset.at));
  }
}

// ---------- shared view helpers ----------

function chip(tone, iconName, label, cls) {
  return el('span', { class: ['chip', 'tone-' + tone, cls] }, iconName ? icon(iconName) : null, label);
}

function code(text) {
  return el('code', { class: 'cmd' }, text);
}

function emptyPanel(title, ...lines) {
  return el('div', { class: 'empty' }, el('div', { class: 'empty-mark' }, icon('inbox')), el('h2', { text: title }), lines);
}

// ---------- state ----------

const state = {
  snap: null,
  connection: 'connecting',
  pulse: 0,
};

const root = document.getElementById('app');

// ---------- header ----------

function renderHeader(snap) {
  const conn = CONNECTION[state.connection];
  const space = snap && snap.space;
  const title = space ? (space.label ? space.label.text : space.workspace_id) : 'Namedrop';
  const health = snap ? HEALTH[snap.health.level](snap.health) : null;
  return el('header', { class: 'top' },
    el('div', { class: ['conn', 'tone-' + conn.tone], role: 'status', title: conn.label },
      el('span', { class: ['dot', state.pulse ? 'pulse-' + (state.pulse % 2) : null] }),
      el('span', { class: 'conn-label', text: conn.label })),
    el('div', { class: 'title' },
      el('span', { class: 'brand', text: 'namedrop' }),
      el('h1', { text: title }),
      space && space.label ? el('span', { class: 'ws', text: space.workspace_id }) : null),
    el('div', { class: 'spacer' }),
    health ? el('span', { class: ['pill', 'tone-' + health.tone] }, el('span', { class: 'dot' }), health.label) : null);
}

function renderDisconnected() {
  if (state.connection !== 'disconnected') return null;
  return el('div', { class: 'banner tone-alert', role: 'alert' }, icon('alert'),
    el('span', null, 'Lost the dashboard server. Run ', code('namedrop dashboard'), ' in this workspace to start it again; this page reconnects on its own.'));
}

// ---------- space page ----------

function renderSpace() {
  const snap = state.snap;
  const view = [renderHeader(snap), renderDisconnected()];
  if (!snap) {
    view.push(el('main', { class: 'board loading' }, el('div', { class: 'loading-mark' })));
  } else if (snap.space.state === 'not_activated' || snap.space.state === 'unknown') {
    view.push(el('main', { class: 'board solo' }, emptyPanel('Namedrop is not active in this workspace',
      el('p', null, 'Run ', code('namedrop activate'), ' from any pane in this Herdr workspace.'),
      el('p', { class: 'muted', text: 'Once agents have names, each one becomes a lane here, and every message between them is drawn live as an arrow from its author to each recipient.' }))));
  } else if (snap.space.state === 'deactivated') {
    view.push(el('main', { class: 'board solo' }, emptyPanel('Namedrop was deactivated here',
      el('p', null, 'History is kept. Run ', code('namedrop activate'), ' to resume.'))));
  } else {
    view.push(renderBoard(snap));
  }
  clear(root);
  append(root, view);
}

function renderBoard(snap) {
  return el('main', { class: 'board' }, el('section', { class: 'stage' }, emptyPanel('No messages yet')));
}

// ---------- index page ----------

const SPACE_STATE = {
  active: { tone: 'done', label: 'active' },
  paused: { tone: 'wait', label: 'paused' },
  not_activated: { tone: 'idle', label: 'not activated' },
  deactivated: { tone: 'idle', label: 'deactivated' },
  unknown: { tone: 'idle', label: 'unknown' },
};

async function renderIndex() {
  clear(root);
  const list = el('ul', { class: 'spaces' });
  append(root, [
    el('header', { class: 'top' }, el('div', { class: 'title' }, el('span', { class: 'brand', text: 'namedrop' }), el('h1', { text: 'Workspaces' }))),
    el('main', { class: 'index' }, el('p', { class: 'muted lede', text: 'Each Herdr workspace has its own dashboard. Pick one to watch its agents.' }), list),
  ]);
  let spaces;
  try {
    const res = await fetch('/api/spaces', { credentials: 'same-origin' });
    if (!res.ok) throw new Error(String(res.status));
    spaces = await res.json();
  } catch (err) {
    append(list, el('li', { class: 'space-error' }, 'Could not load workspaces. Run ', code('namedrop dashboard'), ' from a workspace to open its page.'));
    return;
  }
  if (!spaces.length) {
    append(list, el('li', { class: 'muted', text: 'No Herdr workspaces have used Namedrop yet.' }));
    return;
  }
  for (const space of spaces) {
    const s = SPACE_STATE[space.state] || SPACE_STATE.unknown;
    append(list, el('li', null, el('a', { class: 'space-card', href: '/w/' + encodeURIComponent(space.key) },
      el('span', { class: 'space-name', text: space.label ? space.label.text : space.workspace_id }),
      space.label ? el('span', { class: 'ws', text: space.workspace_id }) : null,
      el('span', { class: 'spacer' }),
      chip(s.tone, null, s.label),
      icon('arrow', 'go'))));
  }
}

// ---------- live connection ----------

const DISCONNECT_AFTER_MS = 30_000;

function connect() {
  const url = location.pathname.replace(/\/+$/, '') + '/events';
  let source = null;
  let downSince = null;

  const setConnection = (next) => {
    if (state.connection === next) return;
    state.connection = next;
    renderSpace();
  };

  const open = () => {
    source = new EventSource(url);
    source.addEventListener('open', () => { downSince = null; });
    source.addEventListener('snapshot', (event) => {
      downSince = null;
      let snap;
      try { snap = JSON.parse(event.data); } catch (err) { return; }
      if (!snap || snap.wire !== 1) return;
      clock.skew = Date.now() - snap.generated_at_ms;
      state.connection = 'live';
      state.pulse += 1;
      applySnapshot(snap);
    });
    source.addEventListener('restarting', () => setConnection('restarting'));
    source.addEventListener('error', () => {
      if (downSince == null) downSince = Date.now();
      if (state.connection === 'live') setConnection('connecting');
      if (source.readyState === EventSource.CLOSED) setTimeout(open, 2000);
    });
  };

  setInterval(() => {
    if (downSince != null && Date.now() - downSince > DISCONNECT_AFTER_MS) setConnection('disconnected');
  }, 1000);
  open();
}

function applySnapshot(snap) {
  state.snap = snap;
  renderSpace();
}

// ---------- boot ----------

if (/^\/w\/[^/]+\/?$/.test(location.pathname)) {
  renderSpace();
  connect();
} else {
  renderIndex();
}
setInterval(tick, 10_000);
