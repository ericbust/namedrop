// Channels highlight on the Namedrop demo's stage. The real Channels page plays in a still
// window; the real Codex and Claude Code screens sit beside it; cards quote the real messages
// between them. renderAt(t) draws one deterministic frame for docs/demo/render.mjs.
import * as THREE from 'three';
import { CSS3DObject } from 'three/addons/renderers/CSS3DRenderer.js';
import { D, STILL, Win, camera, cssScene, bgMat, Card, RUNTIME, title, plate, captionAt, heroAt, drawStage, clamp, ease, esc } from '../../stage3d.js';

const TAKE = '/take/';
const [recording, timeline] = await Promise.all([
  fetch(TAKE + 'recording/recording.json').then(response => response.json()),
  fetch(TAKE + 'timeline.json').then(response => response.json()),
]);
document.fonts.add(await new FontFace('CalloutFont', `url(${TAKE}${timeline.callout.font})`).load());
RUNTIME.channels = ['Channels', 'var(--you)', [0.90, 0.90, 0.92]];
const COLOR = { codex: '#67e8f9', claude: '#fdba74', you: '#e5e5ea' };
const TEAL = '125, 226, 209';
const VIEW = timeline.viewport, APP = timeline.app;

// ---------- the Channels window: real frames, with the spotlight, ring and callout drawn over them ----------
class AppWindow {
  constructor() {
    this.name = 'you';
    this.runtime = 'channels';
    this.w = VIEW.width;
    this.h = VIEW.height + APP.bar;
    const el = document.createElement('div');
    el.className = 'win app';
    el.style.width = this.w + 'px';
    el.style.height = this.h + 'px';
    el.innerHTML = `<div class="bar"><i></i><i></i><i></i><span>${title('Namedrop Channels', 'Release planning')}</span></div>`;
    const body = document.createElement('div');
    body.className = 'body';
    this.canvas = document.createElement('canvas');
    this.canvas.width = VIEW.width * recording.deviceScaleFactor;
    this.canvas.height = VIEW.height * recording.deviceScaleFactor;
    body.appendChild(this.canvas);
    el.appendChild(body);
    this.el = el;
    this.obj = new CSS3DObject(el);
    cssScene.add(this.obj);
    const s = APP.scale;
    this.state = { x: APP.left + this.w * s / 2 - 960, y: 540 - (APP.top + this.h * s / 2), z: 0, s };
    this.obj.position.set(this.state.x, this.state.y, 0);
    this.obj.scale.setScalar(s);
  }

  // The same anchor as a Win: dx from the centre, dy up from the centre or the top edge when null.
  anchor(dx = 0, dy = null) {
    const p = this.state;
    return new THREE.Vector3(p.x + dx * p.s, p.y + (dy === null ? this.h / 2 : dy) * p.s, p.z);
  }

  fade(o) {
    this.el.style.opacity = o;
    this.obj.visible = o > 0.001;
  }
}

const images = new Map();
async function frame(file) {
  if (!images.has(file)) {
    const image = new Image();
    image.src = TAKE + 'recording/' + file;
    await image.decode();
    images.set(file, image);
    if (images.size > 6) images.delete(images.keys().next().value);
  }
  return images.get(file);
}

function nearest(items, t) {
  let low = 0, high = items.length - 1;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (items[middle].t <= t) low = middle;
    else high = middle - 1;
  }
  return items[low + 1] && items[low + 1].t - t < t - items[low].t ? items[low + 1] : items[low];
}

function bracket(keys, t) {
  let i = 0;
  while (i < keys.length - 2 && t >= keys[i + 1][0]) i++;
  const [t0, a] = keys[i], [t1, b] = keys[Math.min(i + 1, keys.length - 1)];
  return [a, b, t1 > t0 ? clamp((t - t0) / (t1 - t0)) : 1];
}
const sourceAt = (shot, t) => { const [a, b, p] = bracket(shot.source, t); return a + (b - a) * p; };
const spotAt = (shot, t) => { const [a, b, p] = bracket(shot.spot, t); return a + (b - a) * ease.inOut(p); };

function roundedRect(target, x, y, width, height, radius) {
  target.moveTo(x + radius, y);
  target.arcTo(x + width, y, x + width, y + height, radius);
  target.arcTo(x + width, y + height, x, y + height, radius);
  target.arcTo(x, y + height, x, y, radius);
  target.arcTo(x, y, x + width, y, radius);
  target.closePath();
}

// The spotlight only darkens app pixels outside the focal ring and outlines the ring.
function spotlight(target, ring, alpha) {
  target.save();
  target.beginPath();
  target.rect(0, 0, VIEW.width, VIEW.height);
  roundedRect(target, ring.x, ring.y, ring.width, ring.height, 10);
  target.fillStyle = `rgba(5, 5, 9, ${0.32 * alpha})`;
  target.fill('evenodd');
  target.beginPath();
  roundedRect(target, ring.x, ring.y, ring.width, ring.height, 10);
  target.lineWidth = 2.5;
  target.strokeStyle = `rgba(${TEAL}, ${alpha})`;
  target.shadowColor = `rgba(${TEAL}, ${0.6 * alpha})`;
  target.shadowBlur = 22;
  target.stroke();
  target.restore();
}

// The callout repeats real on-screen text in large type, in the style of the stage's cards.
// plan.py checks the text and lays out the box with the same font file.
function callout(target, { who, text, box }, agent, alpha) {
  const { main, who: small, pad } = timeline.callout;
  target.save();
  target.globalAlpha = alpha;
  target.beginPath();
  roundedRect(target, box.x, box.y, box.width, box.height, 14);
  target.fillStyle = 'rgba(14, 14, 22, 0.96)';
  target.shadowColor = 'rgba(0, 0, 0, 0.45)';
  target.shadowBlur = 24;
  target.fill();
  target.shadowBlur = 0;
  const border = target.createLinearGradient(box.x, 0, box.x + box.width, 0);
  border.addColorStop(0, COLOR.you);
  border.addColorStop(1, COLOR[agent]);
  target.strokeStyle = border;
  target.lineWidth = 2;
  target.stroke();
  target.textAlign = 'left';
  target.textBaseline = 'top';
  let y = box.y + 14;
  if (who) {
    target.fillStyle = COLOR[agent];
    target.beginPath();
    target.arc(box.x + pad + 5, y + small * 0.55, 5, 0, Math.PI * 2);
    target.fill();
    target.font = `${small}px CalloutFont`;
    target.fillStyle = '#c9ccd6';
    target.fillText(who, box.x + pad + 18, y);
    y += small * 1.35;
  }
  target.font = `${main}px CalloutFont`;
  target.fillStyle = '#f5f5f7';
  target.fillText(text, box.x + pad, y + main * 0.1);
  target.restore();
}

function label(target, text, alpha) {
  target.save();
  target.globalAlpha = alpha;
  target.font = '28px CalloutFont';
  const width = target.measureText(text).width + 48, x = (VIEW.width - width) / 2, y = VIEW.height - 78;
  target.beginPath();
  roundedRect(target, x, y, width, 48, 24);
  target.fillStyle = 'rgba(14, 14, 22, 0.92)';
  target.fill();
  target.strokeStyle = `rgba(${TEAL}, 0.9)`;
  target.lineWidth = 2;
  target.stroke();
  target.fillStyle = '#f5f5f7';
  target.textAlign = 'center';
  target.textBaseline = 'middle';
  target.fillText(text, VIEW.width / 2, y + 25);
  target.restore();
}

const SHOTS = timeline.shots;
const shotAt = t => { const i = SHOTS.findIndex(shot => t < shot.end); return i < 0 ? SHOTS.length - 1 : i; };
const agentOf = shot => timeline.active.filter(([t0]) => shot.start >= t0 - 1e-6).at(-1)[1];

async function drawShot(target, shot, t) {
  const image = await frame(nearest(recording.frames, sourceAt(shot, t)).file);
  target.drawImage(image, 0, 0, VIEW.width, VIEW.height);
  const alpha = spotAt(shot, t);
  if (alpha > 0.001) spotlight(target, shot.ring, alpha);
  if (t >= shot.callout.from) callout(target, shot.callout, agentOf(shot), clamp((t - shot.callout.from) / 0.3));
  if (shot.label) label(target, shot.label, clamp((t - shot.start - shot.fade) / 0.35) * clamp((shot.end - t) / 0.2));
}

const app = new AppWindow();
const context = app.canvas.getContext('2d');
context.scale(recording.deviceScaleFactor, recording.deviceScaleFactor);
const offscreen = document.createElement('canvas');
offscreen.width = app.canvas.width;
offscreen.height = app.canvas.height;
const offscreenContext = offscreen.getContext('2d');
offscreenContext.scale(recording.deviceScaleFactor, recording.deviceScaleFactor);

async function drawApp(t) {
  const index = shotAt(t), shot = SHOTS[index], previous = SHOTS[index - 1];
  const time = Math.min(t, shot.end);
  const blend = previous && shot.fade ? ease.inOut(clamp((time - shot.start) / shot.fade)) : 1;
  if (blend < 1) {
    await drawShot(context, previous, previous.end);
    await drawShot(offscreenContext, shot, time);
    context.save();
    context.globalAlpha = blend;
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.drawImage(offscreen, 0, 0);
    context.restore();
  } else {
    await drawShot(context, shot, time);
  }
}

// ---------- the agents' real screens ----------
const END = timeline.end;
const QUIET = 0.5;
const agents = {};
for (const [alias, spec] of Object.entries(timeline.agents)) {
  const keys = [...new Set(spec.schedule.map(([, key]) => key))];
  const win = Object.assign(new Win(title('@' + alias, RUNTIME[alias][0]), keys, { rows: spec.rows }), { name: '@' + alias, runtime: alias });
  const [left, right] = spec.column, s = (right - left) / win.w;
  const base = { x: (left + right) / 2 - 960, y: 540 - (spec.top + win.h * s / 2), z: 0, s };
  const pose = [];
  for (const [t0, active] of timeline.active) pose.push([t0, { ...base, o: active === alias ? 1 : QUIET }], [t0 + 0.6, {}]);
  pose.push([END - 0.2, {}], [END + 0.5, { o: 0, blur: 10 }]);
  win.pose = pose;
  win.schedule = spec.schedule;
  cssScene.add(win.obj);
  agents[alias] = win;
}
const tags = Object.entries(agents).map(([alias, win]) => plate(win, '@' + alias, alias));

// A card sits over the agent's window, so it never covers the Channels window. Its beam runs
// between the edges of the two windows facing each other.
const STEPS = { to: [0, 0.25, 0.55, 0.9, 1.2, 1.35], from: [0, 0.3, 1.0, 1.7, 2.4, 2.7] };
class StageCard extends Card {
  constructor({ kind, agent, t0, text }) {
    const win = agents[agent];
    super(kind === 'to' ? app : win, kind === 'to' ? win : app, t0, `<div class="msg">${esc(text)}</div>`);
    this.k = STEPS[kind].map(d => t0 + d);
    this.over = win;
    const [fromEdge, toEdge] = kind === 'to' ? [[app.w / 2, -120], [-win.w / 2, 0]] : [[-win.w / 2, 0], [app.w / 2, -120]];
    Object.assign(this.beam, { fa: fromEdge, ta: toEdge, bend: new THREE.Vector3(0, 40, 160) });
  }
  update(t) {
    const [a, b, , , e, f] = this.k;
    this.obj.visible = t > a && t < f;
    if (!this.obj.visible) return;
    let lift = 0, s = 1, o = 1;
    if (t < b) { const q = ease.out((t - a) / (b - a)); o = q; s = 0.94 + 0.06 * q; lift = -(1 - q) * 26; }
    if (t > e) { const q = ease.in((t - e) / (f - e)); o = 1 - q; s = 1 - 0.2 * q; lift = -q * 40; }
    const p = this.over.anchor(0, 0);
    this.obj.position.set(p.x, p.y + lift, 60);
    this.obj.scale.setScalar(s);
    this.el.style.opacity = o;
  }
}
const cards = timeline.cards.map(card => new StageCard(card));
const beams = cards.map(card => card.beam);

// ---------- captions and the end card ----------
const hud = document.getElementById('hud');
const captions = timeline.captions.map(({ text, start, end }) => {
  const el = document.createElement('div');
  el.className = 'cap';
  el.innerHTML = `<h1>${esc(text)}</h1>`;
  hud.appendChild(el);
  return [el, start, end];
});
const endCard = document.createElement('div');
endCard.className = 'end';
endCard.innerHTML = '<h1 class="grad">Channels (Beta)</h1><p>Included in Namedrop 1.5.0</p><div class="pill"><b>$</b> namedrop channels</div>';
hud.appendChild(endCard);

async function renderAt(t) {
  bgMat.uniforms.t.value = STILL ? 0 : t;
  camera.position.set(0, 0, D);
  camera.lookAt(0, 0, 0);
  camera.updateMatrixWorld();
  await drawApp(t);
  app.fade(1 - ease.in(clamp((t - END + 0.2) / 0.7)));
  Object.values(agents).forEach(win => win.update(t));
  tags.forEach(tag => tag.update(t));
  cards.forEach(card => card.update(t));
  beams.forEach(beam => beam.update(t));
  for (const [el, start, end] of captions) captionAt(el, t, start, end);
  heroAt(endCard, t, END + 0.2, timeline.duration + 0.6);
  drawStage();
}

window.renderAt = renderAt;
window.DURATION = timeline.duration;
window.sceneReady = document.fonts.ready.then(() => true);
const params = new URLSearchParams(location.search);
if (!params.has('capture')) {
  const start = performance.now() - (Number(params.get('t')) || 0) * 1000;
  const loop = async () => { await renderAt(((performance.now() - start) / 1000) % window.DURATION); requestAnimationFrame(loop); };
  loop();
} else {
  await renderAt(Number(params.get('t')) || 0);
}
