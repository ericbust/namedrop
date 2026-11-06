const W = 1920, H = 1080;
const canvas = document.querySelector('canvas');
canvas.width = Math.round(W * devicePixelRatio);
canvas.height = Math.round(H * devicePixelRatio);
const ctx = canvas.getContext('2d');
ctx.scale(devicePixelRatio, devicePixelRatio);
const params = new URLSearchParams(location.search);
const recordingBase = params.get('recording') || 'recording';
const images = new Map(), panes = new Map();
const ease = x => x * x * (3 - 2 * x);
const clamp = x => Math.max(0, Math.min(1, x));
const interpolate = (a, b, p) => a.map((value, i) => value + (b[i] - value) * ease(p));
let recording, timeline;

async function frame(file) {
  if (!images.has(file)) {
    const image = new Image();
    image.src = recordingBase + '/' + file;
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

async function paneText(file) {
  if (!file) return [];
  if (!panes.has(file)) {
    const response = await fetch(recordingBase + '/' + file);
    if (!response.ok) throw new Error('Cannot read source pane ' + file);
    let text = (await response.text())
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, '')
      .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
      .replace(/\x1b[@-_]/g, '')
      .replace(/\r/g, '');
    if (recording.profileHome) {
      const home = recording.profileHome.replace(/\/$/, '');
      const parent = home.slice(0, home.lastIndexOf('/'));
      const userHome = home.split('/').slice(0, 3).join('/');
      for (const path of [home, parent, userHome]) {
        const wrapped = [...path].map(char => '\\^$.*+?()[]{}|'.includes(char) ? '\\' + char : char).join('\\s*');
        text = text.replace(new RegExp(wrapped, 'g'), '~');
      }
    }
    const rows = text.split('\n');
    while (rows.length && !rows.at(-1).trim()) rows.pop();
    panes.set(file, rows.slice(-20));
    if (panes.size > 12) panes.delete(panes.keys().next().value);
  }
  return panes.get(file);
}

function pane(rows, name, color, y) {
  const x = 1470, width = 450, height = 435;
  ctx.fillStyle = '#111217';
  ctx.fillRect(x, y, width, height);
  ctx.fillStyle = color;
  ctx.fillRect(x, y, 4, height);
  ctx.font = '600 21px Inter, Arial, sans-serif';
  ctx.textAlign = 'left';
  ctx.fillText(name, x + 22, y + 32);
  ctx.fillStyle = '#8e929f';
  ctx.font = '13px Inter, Arial, sans-serif';
  ctx.textAlign = 'right';
  ctx.fillText('LIVE SESSION', x + width - 18, y + 30);
  ctx.save();
  ctx.beginPath();
  ctx.rect(x + 18, y + 52, width - 36, height - 66);
  ctx.clip();
  ctx.textAlign = 'left';
  ctx.font = '11px "DejaVu Sans Mono", monospace';
  ctx.fillStyle = '#e2e3e8';
  rows.forEach((line, i) => ctx.fillText(line, x + 20, y + 69 + i * 17.3));
  ctx.restore();
}

function endCard(progress) {
  ctx.save();
  ctx.globalAlpha = ease(clamp(progress / 0.18));
  ctx.fillStyle = '#090a0e';
  ctx.fillRect(0, 0, W, H);
  ctx.textAlign = 'center';
  ctx.fillStyle = '#f2f3f6';
  ctx.font = '600 82px Inter, Arial, sans-serif';
  ctx.fillText('Channels (Beta)', W / 2, 384);
  ctx.fillStyle = '#a5a9b6';
  ctx.font = '30px Inter, Arial, sans-serif';
  ctx.fillText('Included in Namedrop 1.5.0', W / 2, 452);
  ctx.fillStyle = '#8ce2d9';
  ctx.font = '31px "DejaVu Sans Mono", monospace';
  ctx.fillText('namedrop channels', W / 2, 581);
  ctx.fillStyle = '#c3c6d0';
  ctx.font = '25px Inter, Arial, sans-serif';
  ctx.fillText('Or choose Open Namedrop Channels in Herdr.', W / 2, 646);
  ctx.restore();
}

window.sceneReady = (async () => {
  [recording, timeline] = await Promise.all([
    fetch(recordingBase + '/recording.json').then(response => response.json()),
    fetch(params.get('timeline') || 'timeline.json').then(response => response.json()),
  ]);
  if (!recording.frames.length) throw new Error('Recording has no source frames');
  if (!recording.throwaway && !recording.panes?.length) throw new Error('Final recording requires real pane captures');
  window.DURATION = timeline.at(-1).end;
  await frame(recording.frames[0].file);
})();

window.renderAt = async t => {
  await window.sceneReady;
  const section = timeline.find(item => t >= item.start && t < item.end) || timeline.at(-1);
  const p = clamp((t - section.start) / (section.end - section.start));
  const sourceTime = section.source[0] + p * (section.source[1] - section.source[0]);
  const sourcePane = recording.panes?.length ? nearest(recording.panes, sourceTime) : {};
  const [image, codex, claude] = await Promise.all([
    frame(nearest(recording.frames, sourceTime).file),
    paneText(sourcePane.codex), paneText(sourcePane.claude),
  ]);
  const [cx, cy, zoom, anchorY] = interpolate(section.from, section.to, p);
  const originX = W / 2 - cx * zoom, originY = anchorY - cy * zoom;
  ctx.fillStyle = '#090a0e';
  ctx.fillRect(0, 0, W, H);
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, 135, W, H - 155);
  ctx.clip();
  ctx.translate(originX, originY);
  ctx.scale(zoom, zoom);
  ctx.drawImage(image, 0, 0, recording.viewport.width, recording.viewport.height);
  pane(codex, 'Codex', '#80ddd5', 0);
  pane(claude, 'Claude', '#dfa78f', 465);
  ctx.restore();

  ctx.fillStyle = '#f2f3f6';
  ctx.textAlign = 'center';
  ctx.font = '600 40px Inter, Arial, sans-serif';
  ctx.fillText(section.caption, W / 2, 79);
  const overviewAlpha = ease(clamp((zoom - 1) / 0.3));
  if (overviewAlpha > 0) {
    ctx.save();
    ctx.globalAlpha = overviewAlpha * 0.94;
    const width = 340, height = width * 900 / 1440, x = W - width - 28, y = 153;
    ctx.fillStyle = '#191b24';
    ctx.fillRect(x - 5, y - 5, width + 10, height + 10);
    ctx.drawImage(image, x, y, width, height);
    const left = Math.max(0, -originX / zoom), top = Math.max(0, (135 - originY) / zoom);
    const right = Math.min(1440, (W - originX) / zoom), bottom = Math.min(900, (H - 20 - originY) / zoom);
    if (right > left && bottom > top) {
      ctx.strokeStyle = '#80ddd5';
      ctx.lineWidth = 2;
      ctx.strokeRect(x + left / 1440 * width, y + top / 900 * height, (right - left) / 1440 * width, (bottom - top) / 900 * height);
    }
    ctx.restore();
  }
  if (section.endCard) endCard(p);
};

await window.sceneReady;
await window.renderAt(Number(params.get('t') || 0));
if (!params.has('capture')) {
  const started = performance.now();
  async function tick() {
    await window.renderAt(((performance.now() - started) / 1000) % window.DURATION);
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}
