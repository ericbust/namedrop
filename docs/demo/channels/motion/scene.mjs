import { W, H, PICTURE, ease, clamp, shotIndexAt, sourceAt, spotAt, viewAt, captionsAt } from './camera.mjs';
const canvas = document.querySelector('canvas');
canvas.width = Math.round(W * devicePixelRatio);
canvas.height = Math.round(H * devicePixelRatio);
const ctx = canvas.getContext('2d');
ctx.scale(devicePixelRatio, devicePixelRatio);
const params = new URLSearchParams(location.search);
const recordingBase = params.get('recording') || 'recording';
const STAGE = '#0b0c10', RING = '255, 190, 72';
const images = new Map();
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

function roundedRect(target, x, y, width, height, radius) {
  target.moveTo(x + radius, y);
  target.arcTo(x + width, y, x + width, y + height, radius);
  target.arcTo(x + width, y + height, x, y + height, radius);
  target.arcTo(x, y + height, x, y, radius);
  target.arcTo(x, y, x + width, y, radius);
  target.closePath();
}

// The spotlight only darkens app pixels outside the focal ring and outlines the ring.
function spotlight(target, ring, view, alpha) {
  const x = view.x + ring.x * view.zoom, y = view.y + ring.y * view.zoom;
  const width = ring.width * view.zoom, height = ring.height * view.zoom, radius = 14;
  target.save();
  target.beginPath();
  target.rect(0, PICTURE.top, W, PICTURE.height);
  roundedRect(target, x, y, width, height, radius);
  target.fillStyle = `rgba(9, 10, 14, ${0.55 * alpha})`;
  target.fill('evenodd');
  target.beginPath();
  roundedRect(target, x, y, width, height, radius);
  target.lineWidth = 4;
  target.strokeStyle = `rgba(${RING}, ${alpha})`;
  target.shadowColor = `rgba(${RING}, ${0.55 * alpha})`;
  target.shadowBlur = 24;
  target.stroke();
  target.restore();
}

function label(target, text, alpha) {
  target.save();
  target.globalAlpha = alpha;
  target.font = '600 38px Inter, Arial, sans-serif';
  const width = target.measureText(text).width + 64, x = (W - width) / 2, y = H - 104;
  target.beginPath();
  roundedRect(target, x, y, width, 66, 33);
  target.fillStyle = 'rgba(15, 16, 22, 0.92)';
  target.fill();
  target.strokeStyle = `rgba(${RING}, 0.9)`;
  target.lineWidth = 2;
  target.stroke();
  target.fillStyle = '#f4f5f8';
  target.textAlign = 'center';
  target.textBaseline = 'middle';
  target.fillText(text, W / 2, y + 34);
  target.restore();
}

function endCard(alpha) {
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.fillStyle = STAGE;
  ctx.fillRect(0, 0, W, H);
  ctx.textAlign = 'center';
  ctx.fillStyle = '#f2f3f6';
  ctx.font = '600 96px Inter, Arial, sans-serif';
  ctx.fillText('Channels (Beta)', W / 2, 430);
  ctx.fillStyle = '#b4b8c4';
  ctx.font = '40px Inter, Arial, sans-serif';
  ctx.fillText('Included in Namedrop 1.5.0', W / 2, 520);
  ctx.fillStyle = '#8ce2d9';
  ctx.font = '44px "DejaVu Sans Mono", monospace';
  ctx.fillText('namedrop channels', W / 2, 660);
  ctx.restore();
}

const offscreen = document.createElement('canvas');
offscreen.width = canvas.width;
offscreen.height = canvas.height;
const offscreenCtx = offscreen.getContext('2d');
offscreenCtx.scale(devicePixelRatio, devicePixelRatio);

window.sceneReady = (async () => {
  [recording, timeline] = await Promise.all([
    fetch(recordingBase + '/recording.json').then(response => response.json()),
    fetch(params.get('timeline') || 'timeline.json').then(response => response.json()),
  ]);
  if (!recording.frames.length) throw new Error('Recording has no source frames');
  window.DURATION = timeline.duration;
  await frame(recording.frames[0].file);
})();

async function drawShot(target, shot, t) {
  const image = await frame(nearest(recording.frames, sourceAt(shot, t)).file);
  const view = viewAt(shot, t);
  target.fillStyle = STAGE;
  target.fillRect(0, 0, W, H);
  target.save();
  target.beginPath();
  target.rect(0, PICTURE.top, W, PICTURE.height);
  target.clip();
  target.translate(view.x, view.y);
  target.scale(view.zoom, view.zoom);
  target.drawImage(image, 0, 0, recording.viewport.width, recording.viewport.height);
  target.restore();
  const alpha = shot.ring ? spotAt(shot, t) : 0;
  if (alpha > 0.001) spotlight(target, shot.ring, view, alpha);
  if (shot.label) label(target, shot.label, clamp((t - shot.start - shot.fade) / 0.35) * clamp((shot.end - t) / 0.2));
}

window.renderAt = async t => {
  await window.sceneReady;
  const index = shotIndexAt(timeline, t);
  const shot = timeline.shots[index], previous = timeline.shots[index - 1];
  const blend = previous && shot.fade ? ease(clamp((t - shot.start) / shot.fade)) : 1;
  if (shot.endCard) {
    await drawShot(ctx, previous, previous.end);
    endCard(blend);
    return;
  }
  if (blend < 1) {
    await drawShot(ctx, previous, previous.end);
    await drawShot(offscreenCtx, shot, t);
    ctx.save();
    ctx.globalAlpha = blend;
    ctx.drawImage(offscreen, 0, 0, W, H);
    ctx.restore();
  } else {
    await drawShot(ctx, shot, t);
  }
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  ctx.font = '600 54px Inter, Arial, sans-serif';
  for (const caption of captionsAt(timeline, t)) {
    ctx.globalAlpha = caption.alpha;
    ctx.fillStyle = '#f4f5f8';
    ctx.fillText(caption.text, W / 2, 75);
  }
  ctx.globalAlpha = 1;
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
