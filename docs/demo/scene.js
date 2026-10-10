// Namedrop demo: real captured pane output staged in 3D.
// renderAt(t) draws one deterministic frame; render.mjs steps it for video.
import * as THREE from 'three';
import { CSS3DRenderer, CSS3DObject } from 'three/addons/renderers/CSS3DRenderer.js';

const W = 1920, H = 1080, FOV = 35;
const D = (H / 2) / Math.tan(THREE.MathUtils.degToRad(FOV / 2));
const params = new URLSearchParams(location.search);
const STILL = params.has('still'); // static background: much smaller GIFs

// Section starts, in seconds. Everything in a section is timed from its start.
const T_NAME = 3.0, T_ASK = 6.3, T_PEERS = 17.2;
// the peer section: two jobs, three conversations, then the whole web
const P1 = T_PEERS + 3.4, P2 = P1 + 7.6, P3 = P2 + 7.6, P4 = P3 + 8.0;
const T_STATUS = P4 + 3.4, T_END = T_STATUS + 6.9;
export const DURATION = T_END + 4.6;

// ---------- measure the monospace cell ----------
const probe = document.createElement('div');
probe.className = 'row';
probe.style.cssText = 'position:absolute;visibility:hidden;display:inline-block;height:auto;line-height:normal';
probe.textContent = 'M'.repeat(100);
document.body.appendChild(probe);
const CW = probe.getBoundingClientRect().width / 100;
const LH = Math.round(probe.getBoundingClientRect().height * 100) / 100;
probe.remove();
document.documentElement.style.setProperty('--lh', LH + 'px');
const PAD_X = 18, PAD_Y = 12, BAR = 38;

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
function cellsOf(line) {
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
const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
function cellsHtml(cells, a = 0, b = cells.length) {
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
const plainOf = (cells) => cells.map((c) => c.ch).join('');

// ---------- windows ----------
class Win {
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
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const ease = {
  inOut: (p) => (p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2),
  out: (p) => 1 - Math.pow(1 - p, 4),
  expo: (p) => (p === 1 ? 1 : 1 - Math.pow(2, -10 * p)),
  in: (p) => p * p * p,
  lin: (p) => p,
};
const POSE = { x: 0, y: 0, z: 0, rx: 0, ry: 0, s: 1, o: 1, blur: 0 };
// keys: [[t, {props}, easeName?], ...]; props carry over between keys.
function sample(t, keys, defaults) {
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
const gl = new THREE.WebGLRenderer({ canvas: document.getElementById('gl'), antialias: true, preserveDrawingBuffer: true });
gl.setPixelRatio(window.devicePixelRatio); // render.mjs renders at 2x for crisp text and beams
gl.setSize(W, H, false);
gl.autoClear = false;
const css = new CSS3DRenderer();
css.setSize(W, H);
document.getElementById('css').appendChild(css.domElement);
const camera = new THREE.PerspectiveCamera(FOV, W / H, 1, 20000);
const scene = new THREE.Scene();
const cssScene = new THREE.Scene();

const bgScene = new THREE.Scene();
const bgCam = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
const bgMat = new THREE.ShaderMaterial({
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
const ACT = [[0.65, 0.71, 1.0], [0.49, 0.89, 0.82]]; // act 1, a lead and its workers: periwinkle to teal
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
class Beam {
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
class Tag {
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
const TAG_W = 640;

// ---------- message cards ----------
// What each pane runs on: [label, CSS colour, beam colour]. `you` is the person typing a job.
const RUNTIME = {
  codex: ['Codex', 'var(--codex)', [0.40, 0.91, 0.98]],
  claude: ['Claude Code', 'var(--claude)', [0.99, 0.73, 0.45]],
  shell: ['zsh', 'var(--shell)', [0.77, 0.71, 0.99]],
  you: ['you', 'var(--you)', [0.90, 0.90, 0.92]],
};
const CARD_Y = 185; // the band the cards travel in, between the captions and the panes' name plates
const who = (name, runtime) => {
  const [label, color] = RUNTIME[runtime];
  return `<span class="who"><i style="background:${color};box-shadow:0 0 12px ${color}"></i>${name}<em style="color:${color}">${label}</em></span>`;
};
// A message between two panes, quoted verbatim from the capture. It rises over the sender, pauses mid-screen to be
// read, then drops into the recipient while a beam traces the way. With no sender it is a job typed by the person:
// it appears over the pane, waits to be read and drops in.
class Card {
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

// ---------- cast ----------
const title = (name, host) => `<b>${name}</b> &nbsp;·&nbsp; ${host}`;
const plate = (win, name, runtime) => new Tag(win, [[0, `${name}<em>${RUNTIME[runtime][0]}</em>`, `plate ${runtime}`]]);
// naming
const nameRev = new Win(title('@reviewer', 'Claude Code'), ['name_rev_typed', 'name_rev_done'], { fit: true });
const nameCoder = new Win(title('@coder', 'Codex'), ['name_coder_typed', 'name_coder_done'], { fit: true });
// act 1: a lead hands work to two workers
const lead = new Win(title('@lead', 'Claude Code'), ['lead_typed', 'lead_sent', 'lead_answer'], { fit: true });
const rev = new Win(title('@reviewer', 'Claude Code'), ['rev_work', 'rev_work2', 'rev_reply'], { fit: true });
const tst = new Win(title('@tester', 'Claude Code'), ['tst_work', 'tst_work2', 'tst_reply'], { fit: true });
// peers: each window fits its frame, so a pane grows as its output arrives
const peerWin = (name, runtime, keys) => Object.assign(
  new Win(title(name, RUNTIME[runtime][0]), keys, { rows: 16, fit: true }), { name, runtime });
const pC = peerWin('@coder', 'codex', ['peer_coder_typed', 'peer_coder_asks', 'peer_coder_extends', 'peer_coder_rereview']);
const pT = peerWin('@tester', 'claude', ['peer_tester_typed', 'peer_tester_run', 'peer_tester_tells', 'peer_tester_pass']);
const pR = peerWin('@reviewer', 'claude', ['peer_rev_idle', 'peer_rev_work', 'peer_rev_findings', 'peer_rev_stale', 'peer_rev_fresh']);
const pB = peerWin('@build', 'shell', ['peer_build_idle', 'peer_build_fail', 'peer_build_pass']);
// Agent status last: stills.mjs crops to the last .win on the page
const ui = new Win('Herdr &nbsp;·&nbsp; <b>namedrop</b>', ['ui_status', 'ui_status_scrolled']);
const wins = [nameRev, nameCoder, lead, rev, tst, pC, pT, pR, pB, ui];
wins.forEach((w) => cssScene.add(w.obj));

// ---- S1: name an agent ----
const N = T_NAME;
nameRev.pose = [
  [N - 0.2, { x: -1300, y: 123, z: -700, ry: 30, s: 0.66, o: 0, blur: 10 }],
  [N + 0.8, { x: -455, z: 0, ry: 7, o: 1, blur: 0 }, 'expo'],
  [N + 3.0, {}],
  [N + 3.7, { x: -1400, z: -1200, ry: 35, o: 0, blur: 10 }],
];
nameCoder.pose = [
  [N - 0.1, { x: 1300, y: 63, z: -700, ry: -30, s: 0.66, o: 0, blur: 10 }],
  [N + 0.9, { x: 455, z: 0, ry: -7, o: 1, blur: 0 }, 'expo'],
  [N + 3.0, {}],
  [N + 3.7, { x: 1400, z: -1200, ry: -35, o: 0, blur: 10 }],
];
nameRev.schedule = [[N - 0.2, 'name_rev_typed'], [N + 2.1, 'name_rev_done']];
nameCoder.schedule = [[N - 0.1, 'name_coder_typed'], [N + 2.2, 'name_coder_done']];
nameRev.type('name_rev_typed', 'Name yourself @reviewer', 2, N + 0.8, N + 1.9)
  .highlight('name_rev_done', '@reviewer — "Reviews', 1, N + 2.5, N + 3.4);
nameCoder.type('name_coder_typed', 'Name yourself @coder', 1, N + 0.9, N + 1.9)
  .highlight('name_coder_done', 'Namedrop set me as @coder', 1, N + 2.6, N + 3.4);

// ---- S2: ask for help in plain words; S3: get one answer back ----
const A = T_ASK;
// the lead types centre stage, then steps aside as its workers arrive
lead.pose = [
  [A - 0.1, { x: -1400, y: -116, z: -600, ry: 30, s: 0.9, o: 0, blur: 10 }],
  [A + 1.0, { x: 0, z: 0, ry: 0, o: 1, blur: 0 }, 'expo'],
  [A + 2.4, {}],
  [A + 3.2, { x: -450, ry: 8, s: 0.7 }],
  [A + 7.0, {}],
  [A + 8.0, { x: 0, y: -100, z: 110, ry: 0, s: 1 }],
  [A + 10.4, {}],
  [A + 11.2, { x: -1500, z: -1300, ry: 30, o: 0, blur: 10 }],
];
lead.schedule = [[A - 0.1, 'lead_typed'], [A + 2.4, 'lead_sent'], [A + 7.0, 'lead_answer']];
lead.type('lead_typed', 'Ask @reviewer', 2, A + 0.8, A + 2.1)
  .highlight('lead_sent', 'Dispatched both', 3, A + 2.8, A + 6.8)
  .highlight('lead_answer', 'Coordination resolved', 1, A + 8.2, A + 10.3)
  .highlight('lead_answer', '1. Security gap', 4, A + 8.6, A + 10.3);
// the workers arrive with the handoffs, already working
rev.pose = [
  [A + 2.5, { x: 1600, y: 70, z: -900, ry: -40, s: 0.54, o: 0, blur: 10 }],
  [A + 3.4, { x: 560, z: -40, ry: -12, o: 1, blur: 0 }, 'expo'],
  [A + 7.0, {}],
  [A + 8.0, { x: 1500, z: -1200, ry: -40, o: 0, blur: 10 }],
];
rev.schedule = [[A + 2.5, 'rev_work'], [A + 4.4, 'rev_work2'], [A + 5.7, 'rev_reply']];
tst.pose = [
  [A + 2.7, { x: 1600, y: -135, z: -900, ry: -40, s: 0.54, o: 0, blur: 10 }],
  [A + 3.6, { x: 560, z: -40, ry: -12, o: 1, blur: 0 }, 'expo'],
  [A + 7.0, {}],
  [A + 8.0, { x: 1500, z: -1200, ry: -40, o: 0, blur: 10 }],
];
tst.schedule = [[A + 2.7, 'tst_work'], [A + 4.1, 'tst_work2'], [A + 4.9, 'tst_reply']];

// ---- S4–S8: peers, no lead, one conversation at a time ----
// A pair: the pane that asks on the left, the one that answers on the right, top edges level under the cards.
const P = T_PEERS;
const PS = 0.72;
const LEFT = { ...POSE, x: -480, y: -156, s: PS }, RIGHT = { ...POSE, x: 480, y: -156, s: PS };
const offstage = (pose) => ({ ...pose, x: Math.sign(pose.x) * 1500, z: -700, ry: -Math.sign(pose.x) * 25, o: 0, blur: 10 });
// At the end all four sit in a 2×2 web: each conversation's pair on one row, @tester under the @coder it wrote to.
const WS = 0.5;
const web = (x, y) => ({ ...POSE, x, y, s: WS });
const WEB_C = web(-470, 22), WEB_R = web(470, 22), WEB_T = web(-470, -247), WEB_B = web(470, -247);
const S = T_STATUS;
const leave = [[S - 0.5, {}], [S + 0.3, { z: -900, o: 0, blur: 12 }]];

// the person gives @coder and @tester their own jobs, at the same time
const jobs = [
  new Card(null, pC, P + 0.3, `<div class="msg">Fix the invisible-formatting gap in <code>validate_persona</code> and
    <code>terminal_safe_text</code> … get it reviewed with pinned Git context …</div>`, { hold: 2.4 }),
  new Card(null, pT, P + 0.5, `<div class="msg">Add tests to <code>tests/personas.rs</code> asserting that personas
    containing an RTL override … are rejected. Run them on <code>@build</code> …</div>`, { hold: 2.4 }),
];
// Codex asks Claude: @coder picks @reviewer by persona and sends the fix with pinned Git context
const ask = new Card(pC, pR, P1 + 0.7, `<div class="msg">Please review the invisible-formatting fix in
  <code>validate_persona</code> and <code>terminal_safe_text</code> for correctness, security gaps, and test coverage.</div>
  <div class="att"><span>pinned Git context</span><span>src/domain.rs</span><span>src/lifecycle.rs</span></div>`);
const findings = new Card(pR, pC, P1 + 4.3, `<div class="msg">Verdict: the approach is right … But the denylist has gaps,
  and I would fix those before merging.</div><div class="att"><span class="stale">HIGH</span>
  <span>U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR get through</span></div>`, { hold: 1.3 });
// @tester runs its tests on @build, a plain shell, and the result comes back
const run = new Card(pT, pB, P2 + 0.7, `<div class="msg mono">cd ~/namedrop &amp;&amp; cargo test --locked --test personas</div>`,
  { hold: 1.0 });
const result = new Card(pB, pT, P2 + 3.9, `<div class="msg">@build command finished with exit code 101.</div>
  <div class="msg mono">test result: FAILED. 4 passed; 1 failed</div>`, { hold: 1.2 });
// the code moved, so the first review is stale: @coder asks again and a fresh approval comes back
const reask = new Card(pC, pR, P3 + 0.8, `<div class="msg">I addressed the reported gaps … The first review is now stale;
  please review this new pinned diff and report any remaining findings.</div>
  <div class="att"><span class="stale">stale · pin 9744a7a</span><span>new pin a5df069</span></div>`, { hold: 1.6 });
const approve = new Card(pR, pC, P3 + 4.6, `<div class="msg">Re-review of pin a5df069 … Verdict: approve. No blocking
  findings. The HIGH item (U+2028/U+2029) and the earlier MEDIUM gaps are fixed.</div>
  <div class="att"><span class="fresh">fresh · pin a5df069</span></div>`, { hold: 1.3 });
const cards = [...jobs, ask, findings, run, result, reask, approve];

pC.pose = [
  [P - 0.2, offstage(LEFT)], [P + 0.7, LEFT, 'expo'],
  [P2 - 0.2, {}], [P2 + 0.5, offstage(LEFT)],             // steps out for @tester and @build
  [P3 - 0.2, {}], [P3 + 0.6, LEFT, 'expo'],                // back for the re-review
  [P4, {}], [P4 + 0.8, WEB_C], ...leave,
];
pT.pose = [
  [P - 0.1, offstage(RIGHT)], [P + 0.8, RIGHT, 'expo'],
  [P1 - 0.2, {}], [P1 + 0.5, offstage(RIGHT)],            // makes room for @reviewer
  [P2 - 0.3, offstage(LEFT)], [P2 + 0.6, LEFT, 'expo'],    // comes back on the left to ask @build
  [P3 - 0.2, {}], [P3 + 0.5, offstage(LEFT)],
  [P4 + 0.1, { ...WEB_T, y: WEB_T.y - 500, o: 0, blur: 10 }], [P4 + 0.9, WEB_T, 'expo'], ...leave,
];
pR.pose = [
  [P1 - 0.1, offstage(RIGHT)], [P1 + 0.7, RIGHT, 'expo'],
  [P2 - 0.2, {}], [P2 + 0.5, offstage(RIGHT)],
  [P3 - 0.2, {}], [P3 + 0.7, RIGHT, 'expo'],
  [P4, {}], [P4 + 0.8, WEB_R], ...leave,
];
pB.pose = [
  [P2 - 0.1, offstage(RIGHT)], [P2 + 0.7, RIGHT, 'expo'],
  [P3 - 0.2, {}], [P3 + 0.5, offstage(RIGHT)],
  [P4 + 0.2, { ...WEB_B, y: WEB_B.y - 500, o: 0, blur: 10 }], [P4 + 1.0, WEB_B, 'expo'], ...leave,
];

// each pane's own screen changes as its messages come and go
pC.schedule = [[P - 0.5, 'peer_coder_typed'], [P1 - 0.3, 'peer_coder_asks'], [P3 - 0.5, 'peer_coder_extends'],
  [reask.k[0] + 0.2, 'peer_coder_rereview']];
pT.schedule = [[P - 0.5, 'peer_tester_typed'], [P2 - 0.5, 'peer_tester_run'], [result.lands, 'peer_tester_tells'],
  [P4 - 0.3, 'peer_tester_pass']];
pR.schedule = [[P1 - 0.5, 'peer_rev_idle'], [ask.lands, 'peer_rev_work'], [findings.k[0] - 0.3, 'peer_rev_findings'],
  [reask.lands, 'peer_rev_stale'], [approve.k[0] - 0.3, 'peer_rev_fresh']];
pB.schedule = [[P2 - 0.5, 'peer_build_idle'], [run.lands, 'peer_build_fail'], [P4 - 0.3, 'peer_build_pass']];

pC.type('peer_coder_typed', 'Fix the invisible-formatting gap', 3, P + 0.9, P + 2.4)
  .highlight('peer_coder_asks', '@reviewer is available and suited', 2, P1 + 0.3, ask.k[3])
  .highlight('peer_coder_extends', 'Edited src/domain.rs', 1, P3 + 0.3, reask.k[0] + 0.2);
pT.type('peer_tester_typed', 'Add tests to tests/personas.rs', 3, P + 1.1, P + 2.6)
  .highlight('peer_tester_tells', 'The new test failed on @build', 1, result.lands + 0.3, P3 - 0.2);
pR.highlight('peer_rev_findings', 'I sent the review back to @coder', 1, findings.k[0], findings.k[3])
  .highlight('peer_rev_stale', 'The first review is now stale', 1, reask.lands + 0.3, approve.k[0] - 0.3)
  .highlight('peer_rev_fresh', 'approved, with nothing blocking', 1, approve.k[0], P4 + 0.5);
pB.highlight('peer_build_fail', 'test result: FAILED', 1, run.lands + 0.3, P3 - 0.2);

// ---- S9: see every agent at a glance ----
ui.schedule = [[S, 'ui_status'], [S + 4.3, 'ui_status_scrolled']];
const pop = ui.spot('ui_status', 'Agent status', S + 1.3);
ui.spot('ui_status_scrolled', 'Agent status', S + 1.3);
const ZOOM = 1.3;
ui.pose = [
  [S, { x: 0, y: -760, z: -700, rx: 30, s: 0.84, o: 0, blur: 10 }],
  [S + 1.2, { y: -150, z: 0, rx: 0, o: 1, blur: 0 }, 'expo'],
  [S + 2.4, {}],
  [S + 3.3, { x: -pop.cx * ZOOM, y: -30 - pop.cy * ZOOM, s: ZOOM }],
  [S + 5.4, {}],
  [S + 6.1, { x: 0, y: -150, s: 0.84 }],
  [S + 6.3, {}],
  [S + 7.0, { z: -900, o: 0, blur: 14 }],
];

const tags = [
  plate(nameRev, '@reviewer', 'claude'), plate(nameCoder, '@coder', 'codex'),
  new Tag(lead, [[A + 3.0, 'waiting on 2 replies', 'wait'], [A + 6.3, '1 of 2 replies in', 'wait'], [A + 7.1, 'merged into one answer', 'done']]),
  new Tag(rev, [[A + 3.4, 'working', 'work'], [A + 6.0, 'replied', 'done']]),
  new Tag(tst, [[A + 3.6, 'working', 'work'], [A + 5.2, 'replied', 'done']]),
  plate(pC, '@coder', 'codex'), plate(pT, '@tester', 'claude'), plate(pR, '@reviewer', 'claude'), plate(pB, '@build', 'shell'),
];

const flow = (from, to) => [RUNTIME[from.runtime][2], RUNTIME[to.runtime][2]];
const beams = [
  // act 1: lead's right edge to each worker's left edge; the link stays up until that reply
  new Beam(lead, rev, A + 2.6, '@reviewer · correctness',
    { dur: 1.8, from: [lead.w / 2, 40], to: [-rev.w / 2, 0], lift: 70, holdUntil: A + 5.9 }),
  new Beam(lead, tst, A + 2.8, '@tester · test gaps',
    { dur: 1.8, from: [lead.w / 2, -60], to: [-tst.w / 2, 0], lift: 20, holdUntil: A + 5.1 }),
  // back: @tester finished first in the real run, then @reviewer
  new Beam(tst, lead, A + 5.1, 'reply', { dur: 1.3, from: [-tst.w / 2, 0], to: [lead.w / 2, -60], lift: 20 }),
  new Beam(rev, lead, A + 5.9, 'reply', { dur: 1.3, from: [-rev.w / 2, 0], to: [lead.w / 2, 40], lift: 70 }),
  // the whole peer web, softly flowing: both conversations and @tester's report to @coder
  new Beam(pC, pR, P4 + 0.9, '', { dur: 1.2, from: [pC.w / 2, 0], to: [-pR.w / 2, 0], lift: 70, colors: flow(pC, pR), holdUntil: S - 0.3 }),
  new Beam(pT, pB, P4 + 1.1, '', { dur: 1.2, from: [pT.w / 2, 0], to: [-pB.w / 2, 0], lift: 70, colors: flow(pT, pB), holdUntil: S - 0.3 }),
  new Beam(pT, pC, P4 + 1.3, '', { dur: 1.2, from: [-pT.w / 2, 30], to: [-pC.w / 2, -30], bend: [-120, 0, 120],
    colors: flow(pT, pC), holdUntil: S - 0.3 }),
  ...cards.map((c) => c.beam).filter(Boolean),
];

// ---------- captions ----------
const hud = (id) => document.getElementById(id);
function captionAt(el, t, t0, t1) {
  const a = clamp((t - t0) / 0.7), b = clamp((t - (t1 - 0.5)) / 0.5);
  const o = ease.out(a) * (1 - ease.in(b));
  el.style.opacity = o;
  el.style.transform = `translateY(${(1 - ease.out(a)) * 26 - ease.in(b) * 18}px)`;
  const blur = (1 - ease.out(a)) * 10 + ease.in(b) * 8;
  el.style.filter = blur > 0.05 ? `blur(${blur}px)` : 'none';
  el.style.visibility = o > 0.001 ? 'visible' : 'hidden';
}
function heroAt(el, t, t0, t1) {
  const a = clamp((t - t0) / 1.1), b = clamp((t - (t1 - 0.6)) / 0.6);
  const o = ease.out(a) * (1 - ease.in(b));
  el.style.opacity = o;
  el.style.transform = `scale(${0.94 + 0.06 * ease.out(a) + 0.04 * ease.in(b)})`;
  const blur = (1 - ease.out(a)) * 18 + ease.in(b) * 10;
  el.style.filter = blur > 0.05 ? `blur(${blur}px)` : 'none';
  el.style.visibility = o > 0.001 ? 'visible' : 'hidden';
}
const CAPTIONS = [
  ['capName', N + 0.1, N + 3.3], ['capAsk', A + 0.1, A + 7.0], ['capOne', A + 7.1, A + 10.8],
  ['capPeers', P + 0.1, P1], ['capCross', P1 + 0.1, P2], ['capDirect', P2 + 0.1, P3], ['capFresh', P3 + 0.1, P4],
  // the Agent status caption leaves before the zoom, which fills the frame
  ['capHome', P4 + 0.1, S], ['capStatus', S + 0.2, S + 2.8],
];

function renderAt(t) {
  bgMat.uniforms.t.value = STILL ? 0 : t;
  camera.position.set(0, 0, D);
  camera.lookAt(0, 0, 0);
  camera.updateMatrixWorld();
  wins.forEach((w) => w.update(t));
  beams.forEach((b) => b.update(t));
  tags.forEach((g) => g.update(t));
  cards.forEach((c) => c.update(t));
  heroAt(hud('title'), t, 0.15, N - 0.05);
  for (const [id, t0, t1] of CAPTIONS) captionAt(hud(id), t, t0, t1);
  heroAt(hud('end'), t, T_END, DURATION);
  gl.clear();
  gl.render(bgScene, bgCam);
  gl.render(scene, camera);
  css.render(cssScene, camera);
}

window.renderAt = renderAt;
window.DURATION = DURATION;
// stills.mjs: Agent status once it has settled with its spotlight, just before the zoom; the end card for social
window.STILLS = { status: S + 2.2, social: T_END + 1.9 };
window.sceneReady = document.fonts.ready.then(() => true);
if (!params.has('capture')) {
  const start = performance.now() - (Number(params.get('t')) || 0) * 1000;
  const loop = () => { renderAt(((performance.now() - start) / 1000) % DURATION); requestAnimationFrame(loop); };
  loop();
} else {
  renderAt(Number(params.get('t')) || 0);
}
