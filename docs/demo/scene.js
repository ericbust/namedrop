// Namedrop demo: real captured pane output staged in 3D.
// renderAt(t) draws one deterministic frame; render.mjs steps it for video.
import { D, params, STILL, Win, POSE, camera, cssScene, bgMat, Beam, Tag, RUNTIME, Card, title, plate, hud, captionAt, heroAt, drawStage } from './stage3d.js';

// Section starts, in seconds. Everything in a section is timed from its start.
const T_NAME = 3.0, T_ASK = 6.3, T_PEERS = 17.2;
// the peer section: two jobs, three conversations, then the whole web
const P1 = T_PEERS + 3.4, P2 = P1 + 7.6, P3 = P2 + 7.6, P4 = P3 + 8.0;
const T_STATUS = P4 + 3.4, T_END = T_STATUS + 6.9;
export const DURATION = T_END + 4.6;

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
  drawStage();
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
