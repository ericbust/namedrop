// Shared stage for the README movies: real captured screens staged in 3D.
// The Namedrop demo (scene.js) and the Channels highlight (channels/motion) both build on it.
import * as THREE from 'three';
import { CSS3DRenderer, CSS3DObject } from 'three/addons/renderers/CSS3DRenderer.js';

export const W = 1920, H = 1080, FOV = 35;
export const D = (H / 2) / Math.tan(THREE.MathUtils.degToRad(FOV / 2));
export const params = new URLSearchParams(location.search);
export const STILL = params.has('still'); // static background: much smaller GIFs

// ---------- measure the monospace cell ----------
const probe = document.createElement('div');
probe.className = 'row';
probe.style.cssText = 'position:absolute;visibility:hidden;display:inline-block;height:auto;line-height:normal';
probe.textContent = 'M'.repeat(100);
document.body.appendChild(probe);
export const CW = probe.getBoundingClientRect().width / 100;
export const LH = Math.round(probe.getBoundingClientRect().height * 100) / 100;
probe.remove();
document.documentElement.style.setProperty('--lh', LH + 'px');
export const PAD_X = 18, PAD_Y = 12, BAR = 38;

// ---------- ANSI -> cells ----------
const BASE = ['#1e1e24', '#ff6b6b', '#5fd38d', '#e5c07b', '#6aa2ff', '#c678dd', '#56b6c2', '#d9d9de',
  '#6e6e76', '#ff8787', '#7ee2a8', '#f0d58c', '#8fb8ff', '#d99cf0', '#7fd4df', '#ffffff'];
function x256(n) {
  if (n < 16) return BASE[n];
  if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
  n -= 16; const s = [0, 95, 135, 175, 215, 255];
  return `rgb(${s[Math.floor(n / 36)]},${s[Math.floor(n / 6) % 6]},${s[n % 6]})`;
}
function applySgr(codes, st) {
  const c = codes === '' ? [0] : codes.split(';').map(Number);
  for (let i = 0; i < c.length; i++) {
    const n = c[i];
    if (n === 0) Object.assign(st, { fg: null, bg: null, b: false, d: false, it: false, u: false });
    else if (n === 1) st.b = true; else if (n === 2) st.d = true; else if (n === 3) st.it = true; else if (n === 4) st.u = true;
    else if (n === 22) { st.b = false; st.d = false; } else if (n === 23) st.it = false; else if (n === 24) st.u = false;
    else if (n === 39) st.fg = null; else if (n === 49) st.bg = null;
    else if (n >= 30 && n <= 37) st.fg = BASE[n - 30]; else if (n >= 90 && n <= 97) st.fg = BASE[n - 82];
    else if (n >= 40 && n <= 47) st.bg = BASE[n - 40]; else if (n >= 100 && n <= 107) st.bg = BASE[n - 92];
    else if ((n === 38 || n === 48) && c[i + 1] === 2) {
      const col = `rgb(${c[i + 2]},${c[i + 3]},${c[i + 4]})`;
      if (n === 38) st.fg = col; else st.bg = col === 'rgb(0,0,0)' ? null : col; // true black reads as a hole
      i += 4;
    } else if ((n === 38 || n === 48) && c[i + 1] === 5) {
      if (n === 38) st.fg = x256(c[i + 2]); else st.bg = x256(c[i + 2]);
      i += 2;
    }
  }
}
export function cellsOf(line) {
  const st = { fg: null, bg: null, b: false, d: false, it: false, u: false }, cells = [];
  const re = /\x1b\[([0-9;]*)m/g; let last = 0, m;
  const push = (text) => {
    const css = (st.fg ? `color:${st.fg};` : '') + (st.bg ? `background:${st.bg};` : '') +
      (st.b ? 'font-weight:700;' : '') + (st.d ? 'opacity:.6;' : '') + (st.it ? 'font-style:italic;' : '') +
      (st.u ? 'text-decoration:underline;' : '');
    for (const ch of text) cells.push({ ch, css });
  };
  while ((m = re.exec(line))) { push(line.slice(last, m.index)); applySgr(m[1], st); last = re.lastIndex; }
  push(line.slice(last));
  return cells;
}
export const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
export function cellsHtml(cells, a = 0, b = cells.length) {
  let out = '', run = '', css = null;
  const flush = () => { if (run) out += css ? `<span style="${css}">${esc(run)}</span>` : esc(run); run = ''; };
  for (let i = a; i < b; i++) {
    const c = cells[i];
    if (c.css !== css) { flush(); css = c.css; }
    run += c.ch;
  }
  flush();
  return out;
}
export const plainOf = (cells) => cells.map((c) => c.ch).join('');

// ---------- windows ----------
export class Win {
  constructor(title, keys, opts = {}) {
    this.keys = keys;
    this.opts = opts;
    const frames = keys.map((k) => FRAMES[k]);
    this.cols = opts.cols || Math.max(...frames.map((f) => f.cols));
    this.rows = opts.rows || Math.max(...frames.map((f) => f.rows.length));
    this.w = Math.ceil(this.cols * CW + PAD_X * 2);
    this.h = Math.ceil(this.rows * LH + PAD_Y * 2 + BAR);
    // Each frame's own height. With opts.fit the window takes it, keeping its top edge in place.
    this.fits = Object.fromEntries(keys.map((k) => [k, Math.min(this.h, Math.ceil(FRAMES[k].rows.length * LH + PAD_Y * 2 + BAR))]));
    this.cur = this.h;
    const el = document.createElement('div');
    el.className = 'win';
    el.style.width = this.w + 'px';
    el.style.height = this.h + 'px';
    el.innerHTML = `<div class="bar"><i></i><i></i><i></i><span>${title}</span></div>`;
    const body = document.createElement('div');
    body.className = 'body';
    body.style.height = (this.h - BAR) + 'px';
    el.appendChild(body);
    this.el = el;
    this.body = body;
    this.frames = {};
    for (const key of keys) this.frames[key] = this.buildFrame(key, body);
    this.obj = new CSS3DObject(el);
    this.pose = null; this.schedule = []; this.typing = {}; this.hls = []; this.spots = {};
  }

  buildFrame(key, body) {
    const f = FRAMES[key];
    const el = document.createElement('div');
    el.className = 'frame';
    const rows = f.rows.map(cellsOf);
    const frame = { el, rows, rowEls: [], typeSpans: [] };
    rows.forEach((cells) => {
      const r = document.createElement('div');
      r.className = 'row';
      r.innerHTML = cellsHtml(cells);
      el.appendChild(r);
      frame.rowEls.push(r);
    });
    body.appendChild(el);
    return frame;
  }

  // Reveal typed prompt text: {match, rows, t0, t1}
  type(key, match, rowsCount, t0, t1) {
    const frame = this.frames[key];
    const start = frame.rows.findIndex((cells) => plainOf(cells).includes(match));
    const spans = [];
    let total = 0;
    for (let r = start; r < start + rowsCount; r++) {
      const cells = frame.rows[r];
      const text = plainOf(cells);
      let from = r === start ? text.indexOf(match) : 2;
      let to = text.trimEnd().length;
      const el = frame.rowEls[r];
      el.innerHTML = cellsHtml(cells, 0, from) + `<span class="type">${cellsHtml(cells, from, to)}</span>` +
        `<span class="caret"></span>` + cellsHtml(cells, to);
      spans.push({ el, type: el.querySelector('.type'), caret: el.querySelector('.caret'), len: to - from, offset: total });
      total += to - from;
    }
    this.typing[key] = { spans, total, t0, t1 };
    return this;
  }

  highlight(key, match, count, t0, t1) {
    const frame = this.frames[key];
    const row = frame.rows.findIndex((cells) => plainOf(cells).includes(match));
    const el = document.createElement('div');
    el.className = 'hl';
    el.style.top = (PAD_Y + row * LH - 2) + 'px';
    el.style.height = (count * LH + 4) + 'px';
    frame.el.appendChild(el);
    this.hls.push({ el, t0, t1, width: this.w - 20 });
    return this;
  }

  // Dim everything around the popup box that starts on the row containing `title`.
  spot(key, title, t0) {
    const frame = this.frames[key];
    const texts = frame.rows.map(plainOf);
    const r0 = texts.findIndex((t) => t.includes('┌' + title));
    const c0 = [...texts[r0]].indexOf('┌');
    const c1 = [...texts[r0]].indexOf('┐', c0);
    const r1 = texts.findIndex((t, i) => i > r0 && [...t][c0] === '└');
    const el = document.createElement('div');
    el.className = 'spot';
    const L = PAD_X + c0 * CW, T = PAD_Y + r0 * LH, R = PAD_X + (c1 + 1) * CW, B = PAD_Y + (r1 + 1) * LH;
    const fw = this.w, fh = PAD_Y * 2 + frame.rows.length * LH;
    // four panels around the popup; huge box-shadows are dropped under 3D transforms
    for (const [x, y, w, h] of [[0, 0, fw, T], [0, B, fw, fh - B], [0, T, L, B - T], [R, T, fw - R, B - T]]) {
      const panel = document.createElement('div');
      Object.assign(panel.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
      el.appendChild(panel);
    }
    const ring = document.createElement('div');
    ring.className = 'ring';
    Object.assign(ring.style, { left: (L - 3) + 'px', top: (T - 3) + 'px', width: (R - L + 6) + 'px', height: (B - T + 6) + 'px' });
    el.appendChild(ring);
    frame.el.appendChild(el);
    this.spots[key] = { el, t0 };
    // popup centre in window-local coordinates (origin at window centre, y up)
    const cx = PAD_X + ((c0 + c1 + 1) / 2) * CW - this.w / 2;
    const cy = this.h / 2 - (BAR + PAD_Y + ((r0 + r1 + 1) / 2) * LH);
    return { cx, cy };
  }

  // The height of the frames on screen at t, easing from one frame's height to the next as the schedule moves on.
  fitAt(t) {
    let h = this.fits[this.schedule[0][1]];
    for (const [t0, key] of this.schedule.slice(1)) {
      if (t < t0) break;
      h += (this.fits[key] - h) * ease.inOut(clamp((t - t0) / 0.45));
    }
    return h;
  }

  // A point on the window in world space: dx/dy from the centre of what is on screen, in window pixels (dy null = top edge).
  anchor(dx = 0, dy = null) {
    const p = this.state, cur = this.cur;
    const ry = THREE.MathUtils.degToRad(p.ry);
    const lx = dx * p.s;
    const ly = ((dy === null ? cur / 2 : dy) + (this.h - cur) / 2) * p.s;
    return new THREE.Vector3(p.x + Math.cos(ry) * lx, p.y + ly, p.z - Math.sin(ry) * lx);
  }

  update(t) {
    const p = this.state = sample(t, this.pose, POSE);
    const cur = this.cur = this.opts.fit ? this.fitAt(t) : this.h;
    this.el.style.height = cur + 'px';
    this.body.style.height = (cur - BAR) + 'px';
    const o = this.obj;
    o.rotation.set(THREE.MathUtils.degToRad(p.rx), THREE.MathUtils.degToRad(p.ry), 0);
    // the top edge stays put while the height changes
    o.position.set(0, (this.h - cur) / 2 * p.s, 0).applyEuler(o.rotation).add(new THREE.Vector3(p.x, p.y, p.z));
    o.scale.setScalar(p.s);
    this.el.style.opacity = p.o;
    this.el.style.filter = p.blur > 0.05 ? `blur(${p.blur}px)` : 'none';
    o.visible = p.o > 0.001;

    // Dip between frames (out, then in) so two screens never overlap mid-fade.
    const HALF = 0.18;
    this.schedule.forEach(([t0, key], i) => {
      const next = this.schedule[i + 1];
      let a = i === 0 ? 1 : clamp((t - t0 - HALF) / HALF);
      if (next) a *= 1 - clamp((t - next[0]) / HALF);
      this.frames[key].el.style.opacity = a;
    });
    for (const key of this.keys) if (!this.schedule.some(([, k]) => k === key)) this.frames[key].el.style.opacity = 0;

    for (const typing of Object.values(this.typing)) {
      const shown = Math.floor(clamp((t - typing.t0) / (typing.t1 - typing.t0)) * typing.total);
      const done = t > typing.t1 + 0.25;
      typing.spans.forEach((s, i) => {
        const n = Math.max(0, Math.min(s.len, shown - s.offset));
        s.type.style.width = (n * CW) + 'px';
        const active = !done && (shown - s.offset >= 0) && (shown < s.offset + s.len || i === typing.spans.length - 1);
        s.caret.style.visibility = active && t > typing.t0 - 0.6 ? 'visible' : 'hidden';
        s.caret.style.opacity = t < typing.t0 ? (Math.floor(t * 2.5) % 2 ? 0.2 : 1) : 1;
      });
    }
    for (const h of this.hls) {
      const a = clamp((t - h.t0) / 0.3) * (1 - clamp((t - h.t1) / 0.4));
      h.el.style.opacity = a;
      h.el.style.width = (ease.out(clamp((t - h.t0) / 0.6)) * h.width) + 'px';
    }
    for (const s of Object.values(this.spots)) s.el.style.opacity = ease.inOut(clamp((t - s.t0) / 0.6));
  }
}

// ---------- tweening ----------
export const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
export const ease = {
  inOut: (p) => (p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2),
  out: (p) => 1 - Math.pow(1 - p, 4),
  expo: (p) => (p === 1 ? 1 : 1 - Math.pow(2, -10 * p)),
  in: (p) => p * p * p,
  lin: (p) => p,
};
export const POSE = { x: 0, y: 0, z: 0, rx: 0, ry: 0, s: 1, o: 1, blur: 0 };
// keys: [[t, {props}, easeName?], ...]; props carry over between keys.
export function sample(t, keys, defaults) {
  const filled = [];
  let prev = { ...defaults };
  for (const [kt, v, e] of keys) { prev = { ...prev, ...v }; filled.push([kt, prev, e]); }
  if (t <= filled[0][0]) return filled[0][1];
  for (let i = 1; i < filled.length; i++) {
    const [t1, v1, e = 'inOut'] = filled[i];
    const [t0, v0] = filled[i - 1];
    if (t <= t1) {
      const p = ease[e](clamp((t - t0) / (t1 - t0)));
      const out = {};
      for (const k in v1) out[k] = v0[k] + (v1[k] - v0[k]) * p;
      return out;
    }
  }
  return filled[filled.length - 1][1];
}

// ---------- renderers ----------
export const gl = new THREE.WebGLRenderer({ canvas: document.getElementById('gl'), antialias: true, preserveDrawingBuffer: true });
gl.setPixelRatio(window.devicePixelRatio); // render.mjs renders at 2x for crisp text and beams
gl.setSize(W, H, false);
gl.autoClear = false;
export const css = new CSS3DRenderer();
css.setSize(W, H);
document.getElementById('css').appendChild(css.domElement);
export const camera = new THREE.PerspectiveCamera(FOV, W / H, 1, 20000);
export const scene = new THREE.Scene();
export const cssScene = new THREE.Scene();

export const bgScene = new THREE.Scene();
export const bgCam = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
export const bgMat = new THREE.ShaderMaterial({
  uniforms: { t: { value: 0 } },
  vertexShader: 'varying vec2 vUv; void main(){ vUv = uv; gl_Position = vec4(position.xy, 0.0, 1.0); }',
  fragmentShader: `
    uniform float t; varying vec2 vUv;
    vec3 blob(vec2 uv, vec2 c, float r, vec3 col) { vec2 d = (uv - c) * vec2(1.7778, 1.0); return col * exp(-dot(d, d) / (r * r)); }
    void main() {
      vec2 uv = vUv;
      vec3 col = vec3(0.016, 0.016, 0.024);
      col += blob(uv, vec2(0.16 + 0.05 * sin(t * 0.21), 0.86 + 0.03 * cos(t * 0.17)), 0.62, vec3(0.20, 0.19, 0.58)) * 0.50;
      col += blob(uv, vec2(0.88 + 0.04 * cos(t * 0.19), 0.16 + 0.05 * sin(t * 0.23)), 0.66, vec3(0.03, 0.34, 0.37)) * 0.46;
      col += blob(uv, vec2(0.58 + 0.06 * sin(t * 0.13), 1.08), 0.52, vec3(0.46, 0.14, 0.52)) * 0.22;
      float v = smoothstep(1.3, 0.25, length((uv - 0.5) * vec2(1.4, 1.0)));
      col *= mix(0.5, 1.0, v);
      gl_FragColor = vec4(col, 1.0);
    }`,
  depthTest: false, depthWrite: false,
});
bgScene.add(new THREE.Mesh(new THREE.PlaneGeometry(2, 2), bgMat));

// ---------- beams ----------
const glowTex = (() => {
  const c = document.createElement('canvas'); c.width = c.height = 128;
  const g = c.getContext('2d'), grad = g.createRadialGradient(64, 64, 0, 64, 64, 64);
  grad.addColorStop(0, 'rgba(255,255,255,1)'); grad.addColorStop(0.18, 'rgba(190,240,230,0.9)');
  grad.addColorStop(0.45, 'rgba(125,200,255,0.25)'); grad.addColorStop(1, 'rgba(125,160,255,0)');
  g.fillStyle = grad; g.fillRect(0, 0, 128, 128);
  return new THREE.CanvasTexture(c);
})();
export const ACT = [[0.65, 0.71, 1.0], [0.49, 0.89, 0.82]]; // act 1, a lead and its workers: periwinkle to teal
// `base` keeps the whole travelled path faintly lit, not just the trail behind the pulse
const beamMaterial = (radiusGain, [colA, colB], base = 0) => new THREE.ShaderMaterial({
  uniforms: { head: { value: 0 }, fade: { value: 0 }, gain: { value: radiusGain }, idle: { value: 0 }, time: { value: 0 },
    base: { value: base }, colA: { value: new THREE.Vector3(...colA) }, colB: { value: new THREE.Vector3(...colB) } },
  vertexShader: 'varying vec2 vUv; void main(){ vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }',
  fragmentShader: `
    uniform float head, fade, gain, idle, time, base; uniform vec3 colA, colB; varying vec2 vUv;
    void main() {
      float u = vUv.x;
      float trail = (base + (1.0 - base) * smoothstep(head - 0.45, head, u)) * step(u, head);
      float glow = exp(-pow((u - head) * 12.0, 2.0));
      float flow = 0.45 + 1.1 * exp(-pow((fract(u - time * 0.55) - 0.5) * 7.0, 2.0));
      float a = ((trail * 0.55 + glow * 1.3) * (1.0 - idle) + flow * 0.55 * idle) * fade * gain;
      vec3 col = mix(colA, colB, u);
      gl_FragColor = vec4(col * a, a);
    }`,
  transparent: true, depthWrite: false, depthTest: false, blending: THREE.AdditiveBlending,
});
// A handoff: a pulse (with a label chip) travels from one window to another. With `holdUntil`, the link then stays
// on, softly flowing, while that assignment is open. `bend` pushes the curve's middle off the straight line. A message
// card drives its own beam instead: `head` gives the pulse's position at t, and the beam fades out after `until`.
export class Beam {
  constructor(from, to, t0, label, { dur = 1.1, from: fa = [0, null], to: ta = [0, null], lift = 240, bend = null,
    holdUntil = 0, colors = ACT, head = null, until = 0, base = 0 } = {}) {
    Object.assign(this, { from, to, t0, dur, fa, ta, holdUntil, headAt: head, until });
    this.bend = new THREE.Vector3(...(bend || [0, lift, 160]));
    this.core = new THREE.Mesh(new THREE.BufferGeometry(), beamMaterial(1, colors, base));
    this.halo = new THREE.Mesh(new THREE.BufferGeometry(), beamMaterial(0.35, colors, base));
    this.spark = new THREE.Sprite(new THREE.SpriteMaterial({ map: glowTex, blending: THREE.AdditiveBlending, depthTest: false, transparent: true }));
    this.spark.scale.setScalar(90);
    scene.add(this.halo, this.core, this.spark);
    const chip = document.createElement('div');
    chip.className = 'chip';
    chip.textContent = label;
    this.chip = new CSS3DObject(chip);
    this.chipEl = chip;
    cssScene.add(this.chip);
  }
  update(t) {
    const p = (t - this.t0) / this.dur;
    const end = this.headAt ? this.until + 0.6 : Math.max(this.t0 + this.dur * 1.5, this.holdUntil + 0.5);
    const on = t > this.t0 - 0.05 && t < end;
    this.core.visible = this.halo.visible = this.spark.visible = on;
    this.chip.visible = on && this.chipEl.textContent !== '';
    if (!on) return;
    const a = this.from.anchor(...this.fa), b = this.to.anchor(...this.ta);
    const mid = a.clone().lerp(b, 0.5).add(this.bend);
    const curve = new THREE.QuadraticBezierCurve3(a, mid, b);
    this.core.geometry.dispose(); this.halo.geometry.dispose();
    this.core.geometry = new THREE.TubeGeometry(curve, 96, 2.6, 8);
    this.halo.geometry = new THREE.TubeGeometry(curve, 96, 11, 8);
    let head, idle = 0, fade;
    if (this.headAt) {
      head = this.headAt(t);
      fade = clamp((t - this.t0) / 0.3) * (1 - clamp((t - this.until) / 0.6));
    } else {
      head = ease.inOut(clamp(p)) * 1.12;
      const held = this.holdUntil > 0;
      idle = held ? clamp((p - 1.0) / 0.25) : 0;
      fade = clamp(p / 0.08) * (held ? 1 - clamp((t - this.holdUntil) / 0.5) : 1 - clamp((p - 1.05) / 0.4));
    }
    for (const m of [this.core, this.halo]) {
      const u = m.material.uniforms;
      u.head.value = head; u.fade.value = fade; u.idle.value = idle; u.time.value = t;
    }
    const pt = curve.getPoint(clamp(head));
    this.spark.position.copy(pt);
    this.spark.material.opacity = fade * (1 - idle) * (head < 0.999 ? 1 : 0.4);
    // The label moves toward the camera along its line of sight: it stays over the beam, and in front of every window.
    this.chip.position.copy(pt).add(camera.position.clone().sub(pt).setLength(220)).add(new THREE.Vector3(0, 30, 0));
    this.chipEl.style.opacity = clamp(p / 0.15) * (1 - clamp((p - 0.92) / 0.2));
  }
}

// A tag above a window's top-left corner: [[t, html, kind], ...]. A status tag moves through wait | work | done;
// a name plate is one `plate codex|claude|shell` entry saying who the pane is and what it runs on.
export class Tag {
  constructor(win, states) {
    Object.assign(this, { win, states });
    this.el = document.createElement('div');
    this.el.className = 'tag';
    this.items = states.map(([, text, kind]) => {
      const item = document.createElement('span');
      item.className = 'tag-item ' + kind;
      item.innerHTML = `<i></i>${text}`;
      this.el.appendChild(item);
      return item;
    });
    this.obj = new CSS3DObject(this.el);
    cssScene.add(this.obj);
  }
  update(t) {
    const w = this.win, p = w.state;
    const o = clamp((t - this.states[0][0]) / 0.35) * p.o;
    this.obj.visible = o > 0.001;
    if (!this.obj.visible) return;
    this.obj.position.copy(w.anchor(-w.w / 2, null)).add(new THREE.Vector3(TAG_W / 2 + 4, 34, 0));
    this.obj.rotation.set(0, THREE.MathUtils.degToRad(p.ry), 0);
    this.el.style.opacity = o;
    const HALF = 0.15;
    this.states.forEach(([t0, , kind], i) => {
      const next = this.states[i + 1];
      let a = i === 0 ? 1 : clamp((t - t0 - HALF) / HALF);
      if (next) a *= 1 - clamp((t - next[0]) / HALF);
      this.items[i].style.opacity = a;
      if (kind === 'work') this.items[i].firstChild.style.opacity = 0.45 + 0.55 * (0.5 + 0.5 * Math.sin(t * 5));
    });
  }
}
export const TAG_W = 640;

// ---------- message cards ----------
// What each pane runs on: [label, CSS colour, beam colour]. `you` is the person typing a job.
export const RUNTIME = {
  codex: ['Codex', 'var(--codex)', [0.40, 0.91, 0.98]],
  claude: ['Claude Code', 'var(--claude)', [0.99, 0.73, 0.45]],
  shell: ['zsh', 'var(--shell)', [0.77, 0.71, 0.99]],
  you: ['you', 'var(--you)', [0.90, 0.90, 0.92]],
};
export const CARD_Y = 185; // the band the cards travel in, between the captions and the panes' name plates
export const who = (name, runtime) => {
  const [label, color] = RUNTIME[runtime];
  return `<span class="who"><i style="background:${color};box-shadow:0 0 12px ${color}"></i>${name}<em style="color:${color}">${label}</em></span>`;
};
// A message between two panes, quoted verbatim from the capture. It rises over the sender, pauses mid-screen to be
// read, then drops into the recipient while a beam traces the way. With no sender it is a job typed by the person:
// it appears over the pane, waits to be read and drops in.
export class Card {
  constructor(from, to, t0, body, { hold = 1.4 } = {}) {
    Object.assign(this, { from, to });
    const steps = from ? [0, 0.3, 1.0, 1.0 + hold, 1.7 + hold, 2.0 + hold] : [0, 0.3, 0.3, 0.3 + hold, 0.3 + hold, 0.6 + hold];
    this.k = steps.map((d) => t0 + d); // appear, reach the middle, leave it, reach the recipient, gone
    const sender = from ? [from.name, from.runtime] : ['you', 'you'];
    this.el = document.createElement('div');
    this.el.className = 'card';
    this.el.style.setProperty('--from', RUNTIME[sender[1]][1]);
    this.el.style.setProperty('--to', RUNTIME[to.runtime][1]);
    this.el.innerHTML = `<div class="hd">${who(...sender)}<span class="arrow">→</span>${who(to.name, to.runtime)}</div>${body}`;
    this.obj = new CSS3DObject(this.el);
    cssScene.add(this.obj);
    if (from) {
      this.beam = new Beam(from, to, this.k[0], '', { colors: [RUNTIME[from.runtime][2], RUNTIME[to.runtime][2]],
        bend: [0, 320, 120], head: (t) => this.u(t), until: this.k[4], base: 0.3 });
    }
  }
  get lands() { return this.k[4]; }
  get gone() { return this.k[5]; }
  // how far the card has come from the sender (0) to the recipient (1)
  u(t) {
    const [, b, c, d, e] = this.k;
    if (t < b) return 0;
    if (t < c) return 0.5 * ease.inOut((t - b) / (c - b));
    if (t < d) return 0.5;
    if (t < e) return 0.5 + 0.5 * ease.inOut((t - d) / (e - d));
    return 1;
  }
  update(t) {
    const [a, b, , , e, f] = this.k;
    this.obj.visible = t > a && t < f;
    if (!this.obj.visible) return;
    const u = this.from ? this.u(t) : 1;
    const xs = (this.from || this.to).state.x, xr = this.to.state.x;
    let y = CARD_Y, s = 1, o = 1;
    if (t < b) { const q = ease.out((t - a) / (b - a)); o = q; s = 0.94 + 0.06 * q; y -= (1 - q) * 26; }
    if (t > e) { const q = ease.in((t - e) / (f - e)); o = 1 - q; s = 1 - 0.25 * q; y -= q * 150; }
    this.obj.position.set(xs + (xr - xs) * u, y, 40);
    this.obj.scale.setScalar(s);
    this.el.style.opacity = o;
  }
}


export const title = (name, host) => `<b>${name}</b> &nbsp;·&nbsp; ${host}`;
export const plate = (win, name, runtime) => new Tag(win, [[0, `${name}<em>${RUNTIME[runtime][0]}</em>`, `plate ${runtime}`]]);

// ---------- captions ----------
export const hud = (id) => document.getElementById(id);
export function captionAt(el, t, t0, t1) {
  const a = clamp((t - t0) / 0.7), b = clamp((t - (t1 - 0.5)) / 0.5);
  const o = ease.out(a) * (1 - ease.in(b));
  el.style.opacity = o;
  el.style.transform = `translateY(${(1 - ease.out(a)) * 26 - ease.in(b) * 18}px)`;
  const blur = (1 - ease.out(a)) * 10 + ease.in(b) * 8;
  el.style.filter = blur > 0.05 ? `blur(${blur}px)` : 'none';
  el.style.visibility = o > 0.001 ? 'visible' : 'hidden';
}
export function heroAt(el, t, t0, t1) {
  const a = clamp((t - t0) / 1.1), b = clamp((t - (t1 - 0.6)) / 0.6);
  const o = ease.out(a) * (1 - ease.in(b));
  el.style.opacity = o;
  el.style.transform = `scale(${0.94 + 0.06 * ease.out(a) + 0.04 * ease.in(b)})`;
  const blur = (1 - ease.out(a)) * 18 + ease.in(b) * 10;
  el.style.filter = blur > 0.05 ? `blur(${blur}px)` : 'none';
  el.style.visibility = o > 0.001 ? 'visible' : 'hidden';
}

// Draw the frame: still background, beams, then the CSS3D windows on top.
export function drawStage() {
  gl.clear();
  gl.render(bgScene, bgCam);
  gl.render(scene, camera);
  css.render(cssScene, camera);
}
