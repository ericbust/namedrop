// node metrics.mjs <label>=<timeline.json>[,<movie.mp4>] ... prints a Markdown table, one column per cut.
// Camera numbers come from the timeline. Movie numbers come from decoded frames, as a cross-check.
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { W, PICTURE, ease, clamp, shotIndexAt, viewAt, captionsAt } from './camera.mjs';

const RATE = 120, CENTER = [W / 2, PICTURE.top + PICTURE.height / 2];
const MOVING = { zoom: 0.01, pan: 5 };
const TOGETHER = { zoom: 0.15, pan: 150 }, HARD = { zoom: 0.3, pan: 500 };
const ZOOMED_IN = 1.2;

// The 59.5-second cut of a669e6d stored one eased segment per caption, with no cuts.
function legacy(sections) {
  const section = t => sections.find(item => t >= item.start && t < item.end) || sections.at(-1);
  const view = t => {
    const item = section(t), p = clamp((t - item.start) / (item.end - item.start));
    const [cx, cy, zoom, anchorY] = item.from.map((value, i) => value + (item.to[i] - value) * ease(p));
    return { x: W / 2 - cx * zoom, y: anchorY - cy * zoom, zoom };
  };
  return {
    duration: sections.at(-1).end,
    shot: t => (section(t).endCard ? 1 : 0),
    view,
    caption: t => section(t).caption || '',
    overview: t => !section(t).endCard && view(t).zoom > 1,
  };
}

function shots(timeline) {
  return {
    duration: timeline.duration,
    shot: t => shotIndexAt(timeline, t),
    view: t => viewAt(timeline.shots[shotIndexAt(timeline, t)], t),
    caption: t => captionsAt(timeline, t, 0).map(caption => caption.text).join(''),
    overview: () => false,
  };
}

function runs(flags, dt) {
  const out = [];
  flags.forEach((on, i) => {
    if (on && (i === 0 || !flags[i - 1])) out.push([i, i]);
    if (on) out.at(-1)[1] = i + 1;
  });
  return out.map(([a, b]) => (b - a) * dt);
}

function camera(cut) {
  const n = Math.round(cut.duration * RATE), dt = 1 / RATE;
  const zoomSpeed = [], panSpeed = [], moving = [], shot = [], zoom = [], caption = [];
  for (let i = 0; i < n; i++) {
    const t = i * dt, next = Math.min(t + dt, cut.duration - 1e-6);
    const a = cut.view(t), b = cut.view(next);
    const wx = (CENTER[0] - a.x) / a.zoom, wy = (CENTER[1] - a.y) / a.zoom;
    const sameShot = cut.shot(t) === cut.shot(next);
    const z = sameShot ? Math.abs(Math.log(b.zoom / a.zoom)) / dt : 0;
    const p = sameShot ? Math.hypot(b.x + wx * b.zoom - CENTER[0], b.y + wy * b.zoom - CENTER[1]) / dt : 0;
    zoomSpeed.push(z); panSpeed.push(p); zoom.push(a.zoom); shot.push(cut.shot(t)); caption.push(cut.caption(t));
    moving.push(z > MOVING.zoom || p > MOVING.pan);
  }
  const moves = runs(moving, dt);
  const holds = [];
  let held = 0;
  for (let i = 0; i < n; i++) {
    const boundary = i > 0 && shot[i] !== shot[i - 1];
    if ((moving[i] || boundary) && held) { holds.push(held); held = 0; }
    if (!moving[i]) held += dt;
  }
  if (held) holds.push(held);
  const captionRuns = [];
  caption.forEach((text, i) => {
    if (i === 0 || text !== caption[i - 1]) captionRuns.push({ text, seconds: 0 });
    captionRuns.at(-1).seconds += dt;
  });
  const shown = captionRuns.filter(run => run.text);
  const zoomedPan = panSpeed.reduce((sum, speed, i) => sum + (zoom[i] >= ZOOMED_IN ? speed * dt : 0), 0);
  const overlap = ({ zoom, pan }) => zoomSpeed.filter((speed, i) => speed > zoom && panSpeed[i] > pan).length * dt;
  const minutes = cut.duration / 60;
  const pictureHolds = holds.filter(seconds => seconds > 0.05);
  return {
    'Duration (s)': cut.duration.toFixed(1),
    'Camera moves': moves.length,
    'Camera moves per minute': (moves.length / minutes).toFixed(1),
    'Time the camera moves (s)': moves.reduce((a, b) => a + b, 0).toFixed(1),
    'Shortest / longest move (s)': `${Math.min(...moves).toFixed(2)} / ${Math.max(...moves).toFixed(2)}`,
    'Shot changes (cut or dissolve)': new Set(shot).size - 1,
    'Peak zoom speed (ln zoom per s)': Math.max(...zoomSpeed).toFixed(2),
    'Peak pan speed at picture center (px/s of 1920)': Math.max(...panSpeed).toFixed(0),
    [`Pan and zoom together (s, zoom > ${TOGETHER.zoom}/s and pan > ${TOGETHER.pan} px/s)`]: overlap(TOGETHER).toFixed(2),
    [`Hard pan and zoom together (s, zoom > ${HARD.zoom}/s and pan > ${HARD.pan} px/s)`]: overlap(HARD).toFixed(2),
    [`Pan travel while zoomed in (px, zoom >= ${ZOOMED_IN})`]: zoomedPan.toFixed(0),
    'Zoom range': `${Math.min(...zoom).toFixed(2)} to ${Math.max(...zoom).toFixed(2)}`,
    'Shortest camera hold (s)': Math.min(...pictureHolds).toFixed(2),
    'Camera holds under 2 s': pictureHolds.filter(seconds => seconds < 2 - 1e-6).length,
    'Captions shown': shown.length,
    'Shortest caption (s)': Math.min(...shown.map(run => run.seconds)).toFixed(2),
    'Mini-map visible (s)': (zoom.filter((_, i) => cut.overview(i * dt)).length * dt).toFixed(1),
  };
}

// Share of pixels, at 480 px wide, whose luma changes by more than 24 levels between
// consecutive frames. A camera move or dissolve changes many; typing or a terminal row
// changes few. A mean difference would also count H.264 flicker in flat dark areas.
function movie(file) {
  const result = spawnSync('ffmpeg', ['-v', 'error', '-i', file, '-vf',
    'scale=480:-2,format=gray,tblend=all_mode=difference,lut=y=if(gt(val\\,24)\\,255\\,0),signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-',
    '-f', 'null', '-'], { encoding: 'utf8', maxBuffer: 1 << 28 });
  if (result.status !== 0) throw new Error(result.stderr);
  const changed = [...result.stdout.matchAll(/YAVG=([\d.]+)/g)].map(match => (100 * Number(match[1])) / 255);
  const fps = Number(spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v', '-show_entries', 'stream=r_frame_rate',
    '-of', 'csv=p=0', file], { encoding: 'utf8' }).stdout.trim().split('/').reduce((a, b) => a / b));
  const busy = changed.map(value => value > 2), still = changed.map(value => value < 0.1);
  const median = [...changed].sort((a, b) => a - b)[Math.floor(changed.length / 2)];
  return {
    'Movie: frames where over 2% of pixels change': `${busy.filter(Boolean).length} of ${changed.length}`,
    'Movie: seconds of large picture change': (busy.filter(Boolean).length / fps).toFixed(1),
    'Movie: runs of large picture change': runs(busy, 1 / fps).length,
    'Movie: median share of pixels changing per frame': `${median.toFixed(2)}%`,
    'Movie: near-still frames (under 0.1% change)': `${(100 * still.filter(Boolean).length / changed.length).toFixed(0)}%`,
    'Movie: longest near-still run (s)': Math.max(...runs(still, 1 / fps)).toFixed(1),
  };
}

const columns = process.argv.slice(2).map(argument => {
  const [label, files] = argument.split('=');
  const [timelineFile, movieFile] = files.split(',');
  const timeline = JSON.parse(readFileSync(timelineFile, 'utf8'));
  const cut = Array.isArray(timeline) ? legacy(timeline) : shots(timeline);
  return { label, values: { ...camera(cut), ...(movieFile ? movie(movieFile) : {}) } };
});
const keys = [...new Set(columns.flatMap(column => Object.keys(column.values)))];
console.log(`| Metric | ${columns.map(column => column.label).join(' | ')} |`);
console.log(`|---|${columns.map(() => '---:').join('|')}|`);
for (const key of keys) console.log(`| ${key} | ${columns.map(column => column.values[key] ?? '').join(' | ')} |`);
