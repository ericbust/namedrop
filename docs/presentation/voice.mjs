import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { CHAPTERS, TEMPO, VOICE } from './script.mjs';

const RATE = VOICE.sampleRate, LEAD = 0.6, GAP = 0.6, TAIL = 1.4;

const SPOKEN = [
  [/@([a-z0-9-]+)/g, (_, name) => `at ${name.replaceAll('-', ' ')}`],
  [/\bAt-mentions\b/g, 'At mentions'],
  [/\bHerdr\b/g, 'Herder'],
  [/\bnamedrop\b/gi, 'Name drop'],
];
const spoken = (text) => SPOKEN.reduce((s, [re, to]) => s.replace(re, to), text);

const sha = (data) => createHash('sha256').update(data).digest('hex');
const renderer = sha(['narrate.py', VOICE.reference].map((f) => sha(readFileSync(f))).join());

function job(line) {
  const settings = { text: spoken(line.say), seed: line.seed ?? VOICE.seed };
  const { description, ...voice } = VOICE;
  const key = sha(JSON.stringify({ renderer, voice, ...settings })).slice(0, 16);
  return { key, beat: line.beat, ...settings, cached: existsSync(`.cache/${key}.f32`) };
}

function wav(samples) {
  const pcm = Buffer.alloc(samples.length * 2);
  samples.forEach((v, i) => pcm.writeInt16LE(Math.round(Math.max(-1, Math.min(1, v)) * 32767), i * 2));
  const h = Buffer.alloc(44);
  h.write('RIFF', 0); h.writeUInt32LE(36 + pcm.length, 4); h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16); h.writeUInt16LE(1, 20); h.writeUInt16LE(1, 22);
  h.writeUInt32LE(RATE, 24); h.writeUInt32LE(RATE * 2, 28); h.writeUInt16LE(2, 32); h.writeUInt16LE(16, 34);
  h.write('data', 36); h.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([h, pcm]);
}

mkdirSync('.cache', { recursive: true });
mkdirSync('voice', { recursive: true });
const jobs = CHAPTERS.map((c) => c.lines.map(job));
const flat = jobs.flat();
writeFileSync('.cache/jobs.json', JSON.stringify({ voice: VOICE, jobs: flat }, null, 1));
const missing = flat.filter((j) => !j.cached).length;
if (missing) {
  if (!process.env.NARRATOR_PYTHON) {
    console.error(`${missing} lines need rendering. Set NARRATOR_PYTHON to the Python of the Qwen3-TTS venv (see README.md).`);
    process.exit(1);
  }
  execFileSync(process.env.NARRATOR_PYTHON, ['narrate.py', '.cache/jobs.json'], { stdio: 'inherit' });
}
const audioOf = (j) => {
  const out = execFileSync('ffmpeg', ['-loglevel', 'error', '-f', 'f32le', '-ar', String(RATE), '-ac', '1',
    '-i', `.cache/${j.key}.f32`, '-af', `atempo=${TEMPO}`, '-f', 'f32le', '-'], { maxBuffer: 1 << 28 });
  return new Float32Array(out.buffer.slice(out.byteOffset, out.byteOffset + out.length));
};

for (const f of readdirSync('voice')) if (!CHAPTERS.some((c) => `${c.id}.mp3` === f)) rmSync(`voice/${f}`);
const timeline = { voice: `${VOICE.model}, ${VOICE.reference}`, chapters: [] };
for (const [ci, chapter] of CHAPTERS.entries()) {
  const parts = [new Float32Array(LEAD * RATE)];
  let at = LEAD;
  const lines = [];
  for (const [i, line] of chapter.lines.entries()) {
    const j = jobs[ci][i];
    const audio = audioOf(j);
    const start = at, end = at + audio.length / RATE;
    const rest = i === chapter.lines.length - 1 ? TAIL : GAP;
    lines.push({ ...line, ...(j.text !== line.say && { spoken: j.text }), start: +start.toFixed(3), end: +end.toFixed(3) });
    parts.push(audio, new Float32Array(rest * RATE));
    at = end + rest;
  }
  const samples = new Float32Array(parts.reduce((n, p) => n + p.length, 0));
  parts.reduce((o, p) => (samples.set(p, o), o + p.length), 0);
  writeFileSync('.cache/chapter.wav', wav(samples));
  execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-i', '.cache/chapter.wav',
    '-af', 'loudnorm=I=-16:TP=-1.5:LRA=11', '-ar', String(RATE), '-ac', '1',
    '-c:a', 'libmp3lame', '-b:a', '96k', `voice/${chapter.id}.mp3`]);
  timeline.chapters.push({ id: chapter.id, title: chapter.title, audio: `voice/${chapter.id}.mp3`, duration: +at.toFixed(3), lines });
  console.log(`${chapter.id.padEnd(11)} ${at.toFixed(1)}s`);
}
writeFileSync('timeline.json', JSON.stringify(timeline, null, 1) + '\n');
console.log(`total ${timeline.chapters.reduce((n, c) => n + c.duration, 0).toFixed(1)}s`);
