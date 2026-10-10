'use strict';

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
  thread: [['path', { d: 'M3 4.5A1.5 1.5 0 0 1 4.5 3h7A1.5 1.5 0 0 1 13 4.5v5A1.5 1.5 0 0 1 11.5 11H7l-3 2.5V11h0A1 1 0 0 1 3 10z' }]],
  inbox: [['path', { d: 'M2.5 9.5 4 4h8l1.5 5.5V12.5h-11zM2.5 9.5h3.2l.8 1.5h3l.8-1.5h3.2' }]],
  chevron: [['path', { d: 'm6 3.5 4.5 4.5L6 12.5' }]],
};

function icon(name, cls) {
  const parts = (ICON_PATHS[name] || ICON_PATHS.queued).map(([tag, attrs]) => svg(tag, attrs));
  return svg('svg', { class: ['icon', 'icon-' + name, cls], viewBox: '0 0 16 16', 'aria-hidden': 'true' }, parts);
}

const LEG_STAGE = {
  queued: { label: 'not sent yet', tone: 'idle' },
  delivered: { label: 'received', tone: 'flight' },
  working: { label: 'working on it', tone: 'flight' },
  replied: { label: 'replied', tone: 'done' },
  completed: { label: 'done', tone: 'done' },
  held: { label: 'on hold', tone: 'alert' },
  failed: { label: 'failed', tone: 'alert' },
  cancelled: { label: 'cancelled', tone: 'idle' },
};

const DELIVERY = {
  queued: { label: 'not sent yet', tone: 'idle', glyph: null },
  dispatching: { label: 'sending', tone: 'idle', glyph: null },
  submitted: { label: 'not picked up yet', tone: 'flight', glyph: null },
  acknowledged: { label: 'working on it', tone: 'wait', glyph: null },
  settled: { label: 'done', tone: 'done', glyph: null },
  held: { label: 'on hold', tone: 'alert', glyph: 'held' },
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
  nudge: 'Reminder',
  hook: 'Automatic',
};

const HEALTH = {
  calm: () => ({ tone: 'done', label: 'All calm' }),
  busy: (h) => ({ tone: 'flight', label: `Busy · ${h.in_flight} in progress` }),
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

function when(ms, prefix) {
  return el('time', { class: 'ago', datetime: new Date(ms).toISOString(), title: new Date(ms).toLocaleString(), data: { at: String(ms), prefix: prefix || '' } },
    (prefix || '') + ago(ms));
}

function tick() {
  for (const node of document.querySelectorAll('time.ago')) {
    node.textContent = node.dataset.prefix + ago(Number(node.dataset.at));
  }
}

function chip(tone, iconName, label, cls) {
  return el('span', { class: ['chip', 'tone-' + tone, cls] }, iconName ? icon(iconName) : null, label);
}

function code(text) {
  return el('code', { class: 'cmd' }, text);
}

function emptyPanel(title, ...lines) {
  return el('div', { class: 'empty' }, el('div', { class: 'empty-mark' }, icon('inbox')), el('h2', { text: title }), lines);
}

const state = {
  snap: null,
  connection: 'connecting',
  pulse: 0,
  memo: null,
  fresh: { messages: new Set(), legs: new Set(), mail: new Set() },
  unseen: 0,
  selected: null,
  drawerEntering: false,
  tab: 'messages',
  showDormant: false,
  showFinished: { handoffs: false, coords: false },
};

const root = document.getElementById('app');

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

function renderSpace() {
  const snap = state.snap;
  const before = document.querySelector('.stage');
  const scroll = before
    ? { top: before.scrollTop, left: before.scrollLeft, atBottom: before.scrollTop + before.clientHeight >= before.scrollHeight - 48 }
    : { top: 0, left: 0, atBottom: true };
  const view = [renderHeader(snap), renderDisconnected(), renderAttention(snap)];
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
  view.push(renderDrawer(snap));
  clear(root);
  append(root, view);
  root.classList.toggle('is-stale', state.connection === 'restarting' || state.connection === 'disconnected');
  tip.hidden = true;

  const stage = document.querySelector('.stage');
  if (!stage) return;
  const roster = stage.querySelector('.roster');
  if (roster) stage.style.setProperty('scroll-padding-top', roster.offsetHeight + 12 + 'px');
  stage.scrollLeft = scroll.left;
  if (scroll.atBottom) {
    stage.scrollTop = stage.scrollHeight;
    state.unseen = 0;
  } else {
    stage.scrollTop = scroll.top;
  }
  stage.addEventListener('scroll', onStageScroll, { passive: true });
  syncNewPill();
}

function onStageScroll(event) {
  const stage = event.currentTarget;
  if (state.unseen && stage.scrollTop + stage.clientHeight >= stage.scrollHeight - 48) {
    state.unseen = 0;
    syncNewPill();
  }
}

function syncNewPill() {
  const pill = document.querySelector('.new-pill');
  if (!pill) return;
  pill.hidden = !state.unseen;
  pill.lastChild.textContent = state.unseen === 1 ? '1 new message' : `${state.unseen} new messages`;
}

// Identity follows the participant, never its lane rank: a slot is assigned the
// first time an address appears and kept for the life of the page.
const IDENTITY_SLOTS = 6;
const identity = new Map();

function identityClass(addressId) {
  if (addressId === 'human') return 'id-human';
  if (!identity.has(addressId)) identity.set(addressId, identity.size < IDENTITY_SLOTS ? identity.size : 'none');
  return 'id-' + identity.get(addressId);
}

// Retired or offline, with nothing in this window and nothing in the mailbox:
// a full lane for them only pushes live recipients off the right edge.
function isDormant(p, talking) {
  return (p.activity === 'retired' || p.activity === 'offline') && !p.mailbox.length && !talking.has(p.address_id);
}

function indexSnapshot(snap) {
  const talking = new Set(snap.messages.flatMap((m) => [m.author, ...m.legs.map((leg) => leg.to)]));
  const dormant = snap.participants.filter((p) => isDormant(p, talking));
  const lanes = state.showDormant ? snap.participants : snap.participants.filter((p) => !dormant.includes(p));
  const byId = new Map(lanes.map((p, i) => [p.address_id, { p, i }]));
  const people = new Map(snap.participants.map((p) => [p.address_id, p]));
  const messages = new Map(snap.messages.map((m) => [m.message_id, m]));
  const threads = new Map(snap.threads.map((t) => [t.thread_id, t]));
  for (const p of snap.participants) identityClass(p.address_id);
  return { snap, lanes, byId, people, dormant: dormant.length, messages, threads };
}

function aliasOf(model, addressId) {
  if (addressId === 'human') return 'You';
  const p = model.people.get(addressId);
  return p ? p.alias : addressId;
}

function laneX(model, addressId) {
  const hit = model.byId.get(addressId);
  const i = hit ? hit.i : 0;
  return ((i + 0.5) / model.lanes.length) * 100;
}

function initials(p) {
  if (p.address_id === 'human') return 'You';
  const words = p.alias.replace(/^@/, '').split(/[-_.\s]+/).filter(Boolean);
  return (words.length > 1 ? words[0][0] + words[1][0] : (words[0] || '?')[0]).toUpperCase();
}

function legKey(messageId, to) {
  return messageId + '\u0000' + to;
}

function diffSnapshot(prev, next) {
  const fresh = { messages: new Set(), legs: new Set(), mail: new Set() };
  const memo = {
    messages: new Set(next.messages.map((m) => m.message_id)),
    legs: new Map(),
    mail: new Map(),
  };
  for (const m of next.messages) {
    for (const leg of m.legs) memo.legs.set(legKey(m.message_id, leg.to), JSON.stringify(leg.stage));
  }
  for (const p of next.participants) {
    for (const item of p.mailbox) memo.mail.set(item.delivery_id, item.state);
  }
  if (prev) {
    for (const id of memo.messages) if (!prev.messages.has(id)) fresh.messages.add(id);
    for (const [key, stage] of memo.legs) if (prev.legs.get(key) !== stage) fresh.legs.add(key);
    for (const [id, st] of memo.mail) if (prev.mail.get(id) !== st) fresh.mail.add(id);
  }
  return { memo, fresh };
}

function renderBoard(snap) {
  const model = indexSnapshot(snap);
  const lanes = model.lanes.length;
  const sheet = el('div', { class: 'sheet', css: { '--lanes': String(lanes) } },
    renderRoster(model),
    renderRows(model));
  const stuck = snap.coordinations.filter((c) => c.outcome === 'blocked').length;
  return el('main', { class: ['board', 'tab-' + state.tab] },
    el('nav', { class: 'tabs', 'aria-label': 'View' },
      tabButton('messages', 'Messages', snap.messages.length),
      tabButton('work', 'Work', stuck ? `${stuck} blocked` : null, stuck ? 'alert' : null)),
    el('div', { class: 'stage-wrap' },
      el('section', { class: 'stage', 'aria-label': 'Messages between participants' }, sheet),
      el('button', { class: 'new-pill', type: 'button', hidden: true, onclick: scrollToEnd }, icon('down'), el('span'))),
    renderRail(model));
}

function tabButton(tab, label, badge, tone) {
  return el('button', {
    class: ['tab', state.tab === tab ? 'is-active' : null],
    type: 'button',
    'aria-pressed': state.tab === tab ? 'true' : 'false',
    onclick: () => { state.tab = tab; renderSpace(); },
  }, label, badge != null ? el('span', { class: ['tab-badge', tone ? 'tone-' + tone : null], text: String(badge) }) : null);
}

function scrollToEnd() {
  const stage = document.querySelector('.stage');
  if (stage) stage.scrollTo({ top: stage.scrollHeight, behavior: motionOk() ? 'smooth' : 'auto' });
}

function motionOk() {
  return !window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function renderRoster(model) {
  return el('div', { class: 'roster' },
    el('div', { class: 'gutter-cell' }, renderDormantToggle(model)),
    model.lanes.map((p) => renderLaneHead(model, p)));
}

function renderDormantToggle(model) {
  if (!model.dormant) return null;
  const n = model.dormant;
  return el('button', {
    class: ['dormant-toggle', state.showDormant ? 'is-open' : null],
    type: 'button',
    'aria-pressed': state.showDormant ? 'true' : 'false',
    title: state.showDormant
      ? 'Hide retired and offline participants with nothing in this window'
      : `Show ${n} retired or offline ${n === 1 ? 'participant' : 'participants'} with nothing in this window`,
    onclick: () => { state.showDormant = !state.showDormant; renderSpace(); },
  }, state.showDormant ? 'hide retired' : `+${n} retired`);
}

function avatar(p, size) {
  const kind = p.kind.type;
  const act = ACTIVITY[p.activity] || ACTIVITY.idle;
  return el('span', { class: ['avatar', size, identityClass(p.address_id), 'act-' + p.activity, 'tone-' + act.tone] },
    el('span', { class: 'disc' }, kind === 'terminal' ? icon('terminal') : initials(p)));
}

function runtimeLabel(p) {
  if (p.kind.type === 'terminal') return 'terminal';
  if (p.kind.type === 'human') return null;
  return RUNTIME[p.kind.runtime] || p.kind.runtime || 'agent';
}

function subtitle(p) {
  if (p.address_id === 'human') return 'at the keyboard';
  const bare = (text) => text.replace(/^@/, '').toLowerCase();
  const role = p.role && bare(p.role.text) !== bare(p.alias) ? p.role : null;
  const text = p.persona || role || p.pane_label;
  if (text) return text.text;
  return p.kind.type === 'agent' ? 'Generic' : null;
}

function renderLaneHead(model, p) {
  const act = ACTIVITY[p.activity] || ACTIVITY.idle;
  const runtime = runtimeLabel(p);
  const sub = subtitle(p);
  const showActivity = p.address_id !== 'human' || p.activity !== 'idle';
  return el('div', { class: ['lane-head', 'act-' + p.activity], data: { address: p.address_id } },
    el('div', { class: 'who' },
      avatar(p, 'lg'),
      el('div', { class: 'names' },
        el('div', { class: 'alias', text: p.address_id === 'human' ? 'You' : p.alias }),
        sub ? el('div', { class: 'sub', text: sub, title: sub }) : null)),
    el('div', { class: 'meta' },
      showActivity ? el('span', { class: ['activity', 'tone-' + act.tone] }, el('span', { class: 'act-dot' }), act.label) : null,
      runtime ? el('span', { class: 'runtime', text: runtime }) : null),
    p.waiting_on.length
      ? el('div', { class: 'waiting tone-wait', title: 'waiting on ' + p.waiting_on.join(', ') }, icon('clock'),
        el('span', { class: 'waiting-text' }, 'waiting on ',
          el('b', { text: p.waiting_on.length === 1 ? p.waiting_on[0] : `${p.waiting_on.length} agents` })))
      : null,
    renderTray(model, p));
}

const TRAY_VISIBLE = 3;

function envelopeIcon(mailState) {
  const open = mailState === 'acknowledged';
  const parts = open
    ? [svg('path', { class: 'env-body', d: 'M1.5 7.5h17v7.5h-17z' }), svg('path', { class: 'env-flap', d: 'M1.5 7.5 10 1.8l8.5 5.7' }), svg('path', { class: 'env-fold', d: 'M1.5 7.5 10 12.5l8.5-5' })]
    : [svg('rect', { class: 'env-body', x: 1.5, y: 3, width: 17, height: 12, rx: 2 }), svg('path', { class: 'env-fold', d: 'M2.2 4 10 9.6 17.8 4' })];
  const badge = DELIVERY[mailState] && DELIVERY[mailState].glyph;
  if (badge) {
    parts.push(svg('circle', { class: 'env-badge', cx: 16.5, cy: 3.8, r: 3.8 }));
    parts.push(svg('path', { class: 'env-badge-mark', d: badge === 'held' ? 'M15.4 2.4v2.8M17.6 2.4v2.8' : badge === 'failed' ? 'm15.2 2.5 2.6 2.6m0-2.6-2.6 2.6' : 'm15 5.3 3-3' }));
  }
  return svg('svg', { class: 'env-icon', viewBox: '0 0 21 16', 'aria-hidden': 'true' }, parts);
}

const tips = new WeakMap();

function tipped(node, build) {
  tips.set(node, build);
  node.classList.add('has-tip');
  return node;
}

function mailTip(item) {
  const d = DELIVERY[item.state] || DELIVERY.queued;
  return [
    el('div', { class: 'tip-head' }, 'from ', el('b', { text: item.from }), ' · ', d.label),
    el('div', { class: 'tip-body', text: item.preview.text + (item.preview.truncated ? '…' : '') }),
    el('div', { class: 'tip-foot' }, 'since ', when(item.since_ms), item.attempts > 1 ? ` · try ${item.attempts}` : null),
  ];
}

function renderTray(model, p) {
  const items = p.mailbox;
  if (p.address_id === 'human' && !items.length) return null;
  const visible = items.slice(0, TRAY_VISIBLE);
  const urgent = items.filter((item) => (DELIVERY[item.state] || DELIVERY.queued).tone === 'alert').length;
  const label = !items.length ? 'nothing waiting'
    : urgent ? (urgent === items.length ? `${urgent} on hold` : `${urgent} on hold of ${items.length}`)
      : `${items.length} waiting`;
  return el('div', { class: ['tray', items.length ? null : 'is-empty', urgent ? 'tone-alert' : null] },
    el('div', { class: 'envelopes' },
      visible.map((item) => {
        const d = DELIVERY[item.state] || DELIVERY.queued;
        return tipped(el('button', {
          class: ['env', 'env-' + item.state, 'tone-' + d.tone, state.fresh.mail.has(item.delivery_id) ? 'drop' : null],
          type: 'button',
          'aria-label': `From ${item.from}, ${d.label}: ${item.preview.text}`,
          onclick: () => (model.messages.has(item.message_id) ? jumpTo(item.message_id) : null),
        }, envelopeIcon(item.state)), () => mailTip(item));
      }),
      items.length > TRAY_VISIBLE ? el('span', { class: 'env-more', text: `+${items.length - TRAY_VISIBLE}` }) : null),
    el('span', { class: 'tray-label', text: label }));
}

const tip = el('div', { class: 'tip', role: 'tooltip', hidden: true });
document.body.appendChild(tip);

function showTip(target) {
  const build = tips.get(target);
  if (!build) return;
  clear(tip);
  append(tip, build());
  tip.hidden = false;
  const r = target.getBoundingClientRect();
  const w = tip.offsetWidth;
  const left = Math.max(8, Math.min(window.innerWidth - w - 8, r.left + r.width / 2 - w / 2));
  tip.style.left = left + 'px';
  tip.style.top = r.bottom + 8 + 'px';
}

document.addEventListener('pointerover', (e) => {
  const target = e.target instanceof Element ? e.target.closest('.has-tip') : null;
  if (target) showTip(target);
  else tip.hidden = true;
});
document.addEventListener('focusin', (e) => {
  if (e.target instanceof Element && e.target.classList.contains('has-tip')) showTip(e.target);
});
document.addEventListener('focusout', () => { tip.hidden = true; });

const GAP_MS = 2 * 60_000;

function renderRows(model) {
  const { snap } = model;
  if (!snap.messages.length) {
    return el('div', { class: 'rows empty-rows' }, emptyPanel('No messages yet',
      el('p', { text: 'When you or an agent @mentions someone, the message appears here as an arrow from its author to each recipient.' }),
      el('p', { class: 'muted', text: 'The mark at each arrowhead shows how far each message got: not sent yet, received, working on it, replied, or on hold.' })));
  }
  const rows = [];
  const threadOrder = new Map();
  let prev = null;
  for (const m of snap.messages) {
    if (prev && m.at_ms - prev.at_ms > GAP_MS) {
      rows.push(el('div', { class: 'gap' }, el('span', { text: `${duration(m.at_ms - prev.at_ms)} later` })));
    }
    if (!threadOrder.has(m.thread_id)) threadOrder.set(m.thread_id, threadOrder.size);
    const band = 'band-' + (threadOrder.get(m.thread_id) % 3);
    if (!prev || prev.thread_id !== m.thread_id) rows.push(renderThreadHead(model, m, band));
    rows.push(renderRow(model, m, band));
    prev = m;
  }
  return el('div', { class: 'rows' }, rows);
}

function renderThreadHead(model, m, band) {
  const thread = model.threads.get(m.thread_id);
  const title = thread ? thread.title.text : 'Conversation';
  const count = thread ? thread.message_count : null;
  return el('div', { class: ['thread-head', band] },
    el('div', { class: 'gutter-cell' }, el('span', { class: 'band-bar start' })),
    el('div', { class: 'thread-title' }, icon('thread'), el('span', { text: title }),
      count ? el('span', { class: 'muted', text: count === 1 ? '1 message' : `${count} messages` }) : null));
}

function isReply(m) {
  return m.kind === 'reply' || m.in_reply_to != null;
}

function renderRow(model, m, band) {
  const fresh = state.fresh;
  const authorX = laneX(model, m.author);
  const authorIdx = model.byId.has(m.author) ? model.byId.get(m.author).i : 0;
  const anchorRight = model.lanes.length > 1 && authorIdx > (model.lanes.length - 1) / 2;
  const isNew = fresh.messages.has(m.message_id);
  const reply = isReply(m);

  const strip = el('div', { class: 'strip' });
  for (const leg of m.legs) {
    if (leg.to === m.author) continue;
    const toX = laneX(model, leg.to);
    const rightward = toX > authorX;
    append(strip, el('span', {
      class: ['wire', reply ? 'dashed' : null, rightward ? 'rightward' : 'leftward', isNew ? 'draw' : null],
      css: { left: Math.min(authorX, toX) + '%', width: Math.abs(toX - authorX) + '%' },
    }));
    append(strip, el('span', { class: ['head', rightward ? 'rightward' : 'leftward', isNew ? 'late' : null], css: { left: toX + '%' } }));
  }
  append(strip, el('span', { class: ['origin', identityClass(m.author), isNew ? 'pop' : null], css: { left: authorX + '%' } }));
  for (const leg of m.legs) append(strip, legChip(model, m, leg));

  const card = el('article', {
    class: ['card', identityClass(m.author), anchorRight ? 'anchor-right' : null, isNew ? 'enter' : null],
    tabindex: '0',
    title: 'Open the full message',
    onclick: () => openDrawer(m.message_id),
    onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openDrawer(m.message_id); } },
  },
  el('header', { class: 'card-head' },
    el('span', { class: 'author', text: aliasOf(model, m.author) }),
    el('span', { class: ['kind', 'kind-' + m.kind], text: MESSAGE_KIND[m.kind] || m.kind }),
    el('span', { class: 'to' }, icon('arrow'), m.legs.map((leg) => el('span', { class: 'to-one' }, aliasOf(model, leg.to), stageMini(leg)))),
    el('span', { class: 'spacer' }),
    when(m.at_ms),
    icon('chevron', 'open-hint')),
  el('p', { class: 'body', text: m.body.text }));

  const cardWrap = el('div', { class: ['card-wrap', anchorRight ? 'anchor-right' : null], css: { '--edge': (anchorRight ? 100 - authorX : authorX) + '%' } }, card);

  return el('div', { class: ['row', band, state.selected === m.message_id ? 'selected' : null], id: 'msg-' + m.message_id },
    el('div', { class: 'gutter-cell' }, el('span', { class: 'band-bar' })),
    el('div', { class: 'lanes-cell' }, strip, cardWrap));
}

function stageMini(leg) {
  const st = LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued;
  return el('span', { class: ['mini', 'tone-' + st.tone], title: st.label }, icon(leg.stage.stage));
}

function legTitle(model, leg) {
  const st = LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued;
  const who = aliasOf(model, leg.to);
  if (leg.stage.stage === 'working') return `${who} is working on it (picked up ${ago(leg.stage.acked_at_ms)})`;
  if (leg.stage.stage === 'replied') return `${who} replied. Click to jump to the reply.`;
  if (leg.stage.stage === 'held') return `Message to ${who} is on hold`;
  return `${who}: ${st.label}`;
}

function legChip(model, m, leg) {
  const st = LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued;
  const changed = state.fresh.legs.has(legKey(m.message_id, leg.to)) && !state.fresh.messages.has(m.message_id);
  return tipped(el('button', {
    class: ['leg', 'tone-' + st.tone, 'stage-' + leg.stage.stage, changed ? 'swap' : null],
    type: 'button',
    'aria-label': legTitle(model, leg),
    css: { left: laneX(model, leg.to) + '%' },
    onclick: (e) => {
      e.stopPropagation();
      if (leg.stage.stage === 'replied') jumpTo(leg.stage.reply_id);
      else openDrawer(m.message_id);
    },
  }, icon(leg.stage.stage)), () => el('div', { class: 'tip-head', text: legTitle(model, leg) }));
}

function jumpTo(messageId) {
  const row = document.getElementById('msg-' + messageId);
  if (!row) return;
  row.scrollIntoView({ block: 'center', behavior: motionOk() ? 'smooth' : 'auto' });
  row.classList.remove('flash');
  void row.offsetWidth;
  row.classList.add('flash');
}

function renderAttention(snap) {
  if (!snap || !snap.attention.length) return null;
  return el('section', { class: 'attention tone-alert', 'aria-label': 'Needs your attention' },
    snap.attention.map((item) => el('div', { class: 'attn-item' },
      icon('alert'),
      el('span', { class: 'attn-text', text: item.summary.text }),
      el('span', { class: 'attn-when' }, when(item.opened_at_ms)))));
}

const WORK_OUTCOME = {
  pending: { tone: 'idle', label: 'not started', icon: 'queued' },
  working: { tone: 'flight', label: 'working', icon: 'working' },
  unclassified: { tone: 'flight', label: 'reported back', icon: 'delivered' },
  needs_continuation: { tone: 'wait', label: 'needs follow-up', icon: 'clock' },
  completed: { tone: 'done', label: 'done', icon: 'completed' },
  blocked: { tone: 'alert', label: 'blocked', icon: 'held' },
};

const COORD_OUTCOME = {
  open: { tone: 'flight', label: 'open' },
  completed: { tone: 'done', label: 'complete' },
  blocked: { tone: 'alert', label: 'blocked' },
  cancelled: { tone: 'idle', label: 'cancelled' },
};

const FINISHED_SHOWN = 3;
const RUNS_SHOWN = 6;
const COORD_RANK = { blocked: 0, open: 1 };

function who(model, addressId) {
  return el('span', { class: 'who-inline' }, avatarFor(model, addressId, 'sm'), el('span', { class: 'who-alias', text: aliasOf(model, addressId) }));
}

function railSection(title, count, body) {
  return el('section', { class: 'rail-section' },
    el('h2', { class: 'rail-title' }, title, count != null ? el('span', { class: 'count', text: String(count) }) : null),
    body);
}

function railList(key, live, finished, render) {
  const open = state.showFinished[key];
  const extra = finished.length - FINISHED_SHOWN;
  return [
    live.map(render),
    (open ? finished : finished.slice(0, FINISHED_SHOWN)).map(render),
    extra > 0 ? el('button', {
      class: 'show-finished',
      type: 'button',
      'aria-expanded': open ? 'true' : 'false',
      onclick: () => { state.showFinished[key] = !open; renderSpace(); },
    }, open ? 'hide completed' : `show ${extra} completed`) : null,
  ];
}

function isDoneStage(stage) {
  return stage === 'replied' || stage === 'completed';
}

function renderHandoff(model, m) {
  const done = m.legs.filter((leg) => isDoneStage(leg.stage.stage)).length;
  const stuck = m.legs.some((leg) => (LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued).tone === 'alert');
  return el('button', { class: ['work-card', 'handoff', stuck ? 'is-stuck' : null], type: 'button', onclick: () => openDrawer(m.message_id) },
    el('div', { class: 'work-head' }, who(model, m.author), icon('arrow', 'faint'),
      el('span', { class: 'muted', text: m.legs.length === 1 ? aliasOf(model, m.legs[0].to) : `${m.legs.length} agents` }),
      el('span', { class: 'spacer' }), when(m.at_ms)),
    el('div', { class: 'work-text', text: m.body.text }),
    el('div', { class: 'progress', role: 'img', 'aria-label': `${done} of ${m.legs.length} finished` },
      m.legs.map((leg) => el('span', { class: ['seg', 'tone-' + (LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued).tone] }))),
    el('div', { class: 'legs-list' },
      el('span', { class: 'progress-label', text: `${done} of ${m.legs.length} finished` }),
      m.legs.map((leg) => {
        const st = LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued;
        return el('span', { class: ['leg-tag', 'tone-' + st.tone], title: `${aliasOf(model, leg.to)}: ${st.label}` },
          el('span', { text: aliasOf(model, leg.to) }), icon(leg.stage.stage));
      })));
}

// A pending assignment whose delivery the assignee has acknowledged is under
// way; the store only reclassifies it once the assignee reports back.
function assignmentOutcome(model, c, a) {
  if (a.work_outcome !== 'pending' || !(c.outcome in COORD_RANK)) return a.work_outcome;
  const assignee = model.people.get(a.assignee);
  const owner = aliasOf(model, c.owner);
  if (assignee && assignee.mailbox.some((item) => item.from === owner && item.state === 'acknowledged')) return 'working';
  const latest = model.snap.messages.filter((m) => m.author === c.owner && m.legs.some((leg) => leg.to === a.assignee)).pop();
  const leg = latest && latest.legs.find((l) => l.to === a.assignee);
  return leg && leg.stage.stage === 'working' ? 'working' : 'pending';
}

function renderCoordination(model, c) {
  const outcome = COORD_OUTCOME[c.outcome] || COORD_OUTCOME.open;
  return el('div', { class: ['work-card', 'coord', 'outcome-' + c.outcome] },
    el('div', { class: 'work-head' }, chip(outcome.tone, null, outcome.label), el('span', { class: 'muted small' }, 'owned by '), who(model, c.owner),
      el('span', { class: 'spacer' }), when(c.updated_at_ms)),
    el('div', { class: 'deliverable', text: c.deliverable.text }),
    el('ul', { class: 'assignments' }, c.assignments.map((a) => {
      const w = WORK_OUTCOME[assignmentOutcome(model, c, a)] || WORK_OUTCOME.pending;
      return el('li', { class: 'assignment' },
        el('div', { class: 'assignment-line' }, who(model, a.assignee), el('span', { class: 'spacer' }), chip(w.tone, w.icon, w.label)),
        a.blocked_reason ? el('div', { class: 'blocked-reason tone-alert' }, a.blocked_reason.text) : null,
        a.waiting_on ? el('div', { class: 'waiting tone-wait' }, icon('clock'), el('span', { class: 'waiting-text' }, 'waiting on ', el('b', { text: aliasOf(model, a.waiting_on) }))) : null);
    })));
}

function renderRun(model, run) {
  const status = run.exit_code == null
    ? chip('flight', 'working', 'running')
    : run.exit_code === 0 ? chip('done', 'completed', 'exit 0') : chip('alert', 'failed', `exit ${run.exit_code}`);
  return el('li', { class: ['run', run.exit_code == null ? 'is-running' : null] },
    el('div', { class: 'run-line' }, who(model, run.terminal), el('span', { class: 'spacer' }), when(run.started_at_ms)),
    el('div', { class: 'run-line' }, el('code', { class: 'run-cmd', title: run.command.text }, '$ ' + run.command.text), status));
}

function renderKey() {
  return el('details', { class: 'key' },
    el('summary', null, 'How to read this'),
    el('div', { class: 'key-grid' },
      Object.entries(LEG_STAGE).map(([stage, st]) => el('span', { class: 'key-item' },
        el('span', { class: ['leg', 'static', 'tone-' + st.tone, 'stage-' + stage] }, icon(stage)), st.label))),
    el('p', { class: 'muted small', text: 'Each arrow runs from the author to a recipient. The mark at its end shows how far the message got. Dashed arrows are replies.' }),
    el('div', { class: 'key-grid' },
      ['queued', 'submitted', 'acknowledged', 'held'].map((st) => el('span', { class: 'key-item' },
        el('span', { class: ['env', 'static', 'env-' + st, 'tone-' + DELIVERY[st].tone] }, envelopeIcon(st)), DELIVERY[st].label))));
}

function renderRail(model) {
  const { snap } = model;
  const handoffs = snap.messages.filter((m) => m.kind === 'handoff' || m.legs.length > 1).reverse();
  const handoffDone = (m) => m.legs.every((leg) => isDoneStage(leg.stage.stage));
  const runs = snap.terminal_runs.slice().sort((a, b) => b.started_at_ms - a.started_at_ms).slice(0, RUNS_SHOWN);
  const rank = (c) => COORD_RANK[c.outcome] ?? 2;
  const coords = snap.coordinations.slice().sort((a, b) => rank(a) - rank(b) || b.updated_at_ms - a.updated_at_ms);
  const sections = [
    handoffs.length ? railSection('Handoffs', handoffs.length,
      railList('handoffs', handoffs.filter((m) => !handoffDone(m)), handoffs.filter(handoffDone), (m) => renderHandoff(model, m))) : null,
    coords.length ? railSection('Tracked work', coords.length,
      railList('coords', coords.filter((c) => rank(c) < 2), coords.filter((c) => rank(c) === 2), (c) => renderCoordination(model, c))) : null,
    runs.length ? railSection('Terminal runs', null, el('ul', { class: 'runs' }, runs.map((r) => renderRun(model, r)))) : null,
  ].filter(Boolean);
  return el('aside', { class: 'rail', 'aria-label': 'Work in progress' },
    sections.length ? sections : el('p', { class: 'rail-empty muted', text: 'Handoffs, tracked work, and terminal commands show up here as they happen.' }),
    renderKey());
}

function openDrawer(messageId) {
  state.drawerEntering = !state.selected;
  state.selected = messageId;
  renderSpace();
  const close = document.querySelector('.drawer .close');
  if (close) close.focus({ preventScroll: true });
}

function closeDrawer() {
  const was = state.selected;
  state.selected = null;
  renderSpace();
  const card = was && document.querySelector(`#msg-${CSS.escape(was)} .card`);
  if (card) card.focus({ preventScroll: true });
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && state.selected) closeDrawer();
});

function renderDrawer(snap) {
  if (!snap || !state.selected) return null;
  const model = indexSnapshot(snap);
  const m = model.messages.get(state.selected);
  if (!m) return null;
  const thread = model.threads.get(m.thread_id);
  const parent = m.in_reply_to ? model.messages.get(m.in_reply_to) : null;
  const entering = state.drawerEntering;
  state.drawerEntering = false;
  return [
    el('div', { class: ['scrim', entering ? 'enter' : null], onclick: closeDrawer }),
    el('aside', { class: ['drawer', entering ? 'enter' : null], role: 'dialog', 'aria-modal': 'true', 'aria-label': 'Message detail' },
      el('header', { class: 'drawer-head' },
        el('div', { class: 'drawer-who' },
          avatarFor(model, m.author),
          el('div', null,
            el('div', { class: 'drawer-title' }, el('b', { text: aliasOf(model, m.author) }),
              el('span', { class: ['kind', 'kind-' + m.kind], text: MESSAGE_KIND[m.kind] || m.kind })),
            el('div', { class: 'muted small' }, clockTime(m.at_ms), ' · ', when(m.at_ms)))),
        el('button', { class: 'close', type: 'button', 'aria-label': 'Close', onclick: closeDrawer }, icon('close'))),
      el('div', { class: 'drawer-body' },
        thread ? el('div', { class: 'drawer-thread' }, icon('thread'), thread.title.text) : null,
        parent ? el('button', { class: 'linkish', type: 'button', onclick: () => { closeDrawer(); jumpTo(parent.message_id); } },
          'In reply to ', el('b', { text: aliasOf(model, parent.author) }), ': ', parent.body.text.slice(0, 60) + (parent.body.text.length > 60 ? '…' : '')) : null,
        el('div', { class: 'full-body', text: m.body.text }),
        m.body.truncated ? el('p', { class: 'muted small', text: 'Shortened for the dashboard. Each recipient got the full message.' }) : null,
        el('h3', { text: m.legs.length === 1 ? 'Recipient' : `${m.legs.length} recipients` }),
        el('ol', { class: 'timeline' }, m.legs.map((leg) => renderLegDetail(model, m, leg)))))];
}

function avatarFor(model, addressId, size) {
  const p = model.people.get(addressId);
  if (p) return avatar(p, size || 'md');
  return el('span', { class: ['avatar', size || 'md', 'id-none'] }, el('span', { class: 'disc', text: '?' }));
}

function renderLegDetail(model, m, leg) {
  const st = LEG_STAGE[leg.stage.stage] || LEG_STAGE.queued;
  const recipient = model.people.get(leg.to);
  const mail = recipient ? recipient.mailbox.find((item) => item.message_id === m.message_id) : null;
  let detail = null;
  const stage = leg.stage;
  if (stage.stage === 'working') detail = el('span', null, 'picked up ', when(stage.acked_at_ms));
  else if (stage.stage === 'replied') {
    const reply = model.messages.get(stage.reply_id);
    detail = el('button', { class: 'linkish', type: 'button', onclick: () => { closeDrawer(); jumpTo(stage.reply_id); } },
      reply ? `replied ${duration(reply.at_ms - m.at_ms)} later` : 'replied', ' · view reply');
  } else if (stage.stage === 'completed') detail = el('span', { text: stage.kind === 'no_reply' ? 'finished without a reply' : 'finished' });
  else if (stage.stage === 'held') detail = el('span', { text: 'waiting for the recipient to come back' });
  else if (stage.stage === 'failed') detail = el('span', { text: 'could not be sent' });
  return el('li', { class: ['tl-item', 'tone-' + st.tone] },
    avatarFor(model, leg.to),
    el('div', { class: 'tl-text' },
      el('div', null, el('b', { text: aliasOf(model, leg.to) }), ' ', chip(st.tone, stage.stage, st.label)),
      detail ? el('div', { class: 'muted small' }, detail) : null,
      mail ? el('div', { class: 'muted small' }, `waiting: ${(DELIVERY[mail.state] || DELIVERY.queued).label}`,
        mail.attempts > 1 ? ` · try ${mail.attempts}` : null, ' · since ', when(mail.since_ms)) : null));
}

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
  const { memo, fresh } = diffSnapshot(state.memo, snap);
  state.memo = memo;
  state.fresh = fresh;
  state.unseen += fresh.messages.size;
  state.snap = snap;
  renderSpace();
  // Animations belong to the frame that caused them; later re-renders
  // (drawer, connection changes) must not replay them.
  state.fresh = { messages: new Set(), legs: new Set(), mail: new Set() };
}

if (/^\/w\/[^/]+\/?$/.test(location.pathname)) {
  renderSpace();
  connect();
} else {
  renderIndex();
}
setInterval(tick, 10_000);
