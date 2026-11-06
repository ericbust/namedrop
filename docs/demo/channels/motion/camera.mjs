export const W = 1920, H = 1080;
export const PICTURE = { top: 110, height: 970 };
const CENTER_Y = PICTURE.top + PICTURE.height / 2;

export const ease = x => x * x * (3 - 2 * x);
export const clamp = x => Math.max(0, Math.min(1, x));

export function shotIndexAt(timeline, t) {
  const index = timeline.shots.findIndex(shot => t >= shot.start && t < shot.end);
  return index < 0 ? timeline.shots.length - 1 : index;
}

function bracket(keys, t) {
  let i = 0;
  while (i < keys.length - 2 && t >= keys[i + 1][0]) i++;
  const [t0, a] = keys[i], [t1, b] = keys[Math.min(i + 1, keys.length - 1)];
  return [a, b, t1 > t0 ? clamp((t - t0) / (t1 - t0)) : 1];
}

export function sourceAt(shot, t) {
  const [a, b, p] = bracket(shot.source, t);
  return a + (b - a) * p;
}

export function spotAt(shot, t) {
  const [a, b, p] = bracket(shot.spot, t);
  return a + (b - a) * ease(p);
}

const origin = ([cx, cy, zoom]) => [W / 2 - cx * zoom, CENTER_Y - cy * zoom, zoom];

// A push-in scales about the one world point that keeps its screen position, so the
// picture grows from that point instead of sliding while it scales.
export function viewAt(shot, t) {
  const [a, b, p] = bracket(shot.camera, t);
  const [ax, ay, az] = origin(a), [bx, by, bz] = origin(b), e = ease(p);
  if (Math.abs(az - bz) < 1e-9) return { x: ax + (bx - ax) * e, y: ay + (by - ay) * e, zoom: az };
  const zoom = az * (bz / az) ** e;
  const fx = (bx - ax) / (az - bz), fy = (by - ay) / (az - bz);
  return { x: ax + fx * (az - zoom), y: ay + fy * (az - zoom), zoom };
}

export function captionsAt(timeline, t, fade = 0.25) {
  return timeline.captions
    .filter(caption => t >= caption.start && t < caption.end)
    .map(caption => ({ text: caption.text, alpha: Math.min(clamp((t - caption.start) / fade), clamp((caption.end - t) / fade)) }));
}
