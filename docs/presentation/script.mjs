// The narration, chapter by chapter. voice.mjs speaks it; the page plays it.
// `beat` names the visual state a line cues. `head` is the large on-screen
// headline for that line. `show` is literal text the scene puts on screen
// (a prompt or a command) while that line plays.
//
// A line can set `seed` to replace VOICE.seed. Use it only when a line fails check_voice.py.

// The narrator is synthetic, not a real person. Qwen3-TTS VoiceDesign (Qwen3-TTS-12Hz-1.7B-VoiceDesign)
// rendered narrator.flac from `description` alone. The 1.7B Base model clones that clip for every line.
export const VOICE = {
  model: 'Qwen/Qwen3-TTS-12Hz-1.7B-Base',
  reference: 'narrator.flac',
  referenceText: 'Every good tool starts with a small annoyance. This one started with a lot of copying and pasting, and a quiet wish that it would just stop.',
  description: 'A warm, calm, confident female narrator in her thirties, unhurried, conversational, like a product keynote voice-over.',
  language: 'English',
  sampleRate: 24000,
  seed: 0,
  sampling: { do_sample: true, top_k: 50, top_p: 1.0, temperature: 0.9, repetition_penalty: 1.05 },
};

// Every line is spoken at the model's natural speed, then sped up by this pitch-preserving factor
// (ffmpeg atempo), so the whole film reads slightly faster than normal.
export const TEMPO = 1.08;

export const CHAPTERS = [
  {
    id: 'bus',
    title: 'The message bus',
    lines: [
      { beat: 'panes', say: 'You probably run more than one coding agent now.' },
      { beat: 'runtimes', say: 'Claude Code in one pane. Codex in another. A shell for your tests.' },
      { beat: 'shuttle', head: 'Copy. Paste. Wait.', say: "And you're the one carrying everything between them. Copy. Paste. Wait. Copy it back." },
      { beat: 'you-are-the-bus', head: 'You are the message bus.', say: "You've become the message bus." },
    ],
  },
  {
    id: 'intro',
    title: 'Namedrop',
    lines: [
      { beat: 'logo', head: 'Namedrop', say: 'This is Namedrop.' },
      { beat: 'promise', head: '@mentions for your agents.', say: "At-mentions for your agents. It's a plugin for Herdr that lets every agent in your workspace reach any other, by name." },
    ],
  },
  {
    id: 'mention',
    title: 'Name it. Mention it.',
    lines: [
      { beat: 'type-name', head: 'Give it a name.', say: 'It starts with a name. Tell an agent, name yourself @reviewer.', show: 'Name yourself @reviewer with Namedrop.' },
      { beat: 'type-mention', head: 'Mention it.', say: 'Now, from any other pane, just mention it. Ask @reviewer to check the retry logic.', show: 'Ask @reviewer to check the retry logic. Use their feedback to fix any bugs.' },
      { beat: 'send', say: 'Your agent sends the request, and keeps working on your goal.' },
      { beat: 'reply-returns', head: 'The answer comes back.', say: 'When the review is done, the answer comes back to the pane that asked.' },
      { beat: 'cross-runtime', head: 'Claude Code. Codex. Same names.', say: 'Claude Code or Codex. It works the same either way.' },
    ],
  },
  {
    id: 'fan-out',
    title: 'One answer from many',
    lines: [
      { beat: 'type-fan-out', say: 'Ask two agents at once. @reviewer checks correctness. @tester looks for missing tests.', show: 'Ask @reviewer to inspect correctness and @tester to find missing test cases. Give me one answer with the changes I should make.' },
      { beat: 'assignments', head: 'Tracked.', say: 'The lead tracks each assignment, and gets a nudge if anything stalls.' },
      { beat: 'merge', head: 'One answer.', say: 'Then it reviews what comes back, and hands you one answer.' },
    ],
  },
  {
    id: 'terminals',
    title: 'Terminals, too',
    lines: [
      { beat: 'name-build', head: 'Terminals have names too.', say: 'Even a plain shell can have a name.', show: 'namedrop name @build --type terminal' },
      { beat: 'run-build', head: 'No model. No tokens.', say: '@build, cargo test. It runs once, in that shell. No model. No tokens.', show: '@build cargo test --locked --all-targets' },
      { beat: 'exit-code', say: 'The output and the exit code go straight back to the agent that asked.' },
    ],
  },
  {
    id: 'team',
    title: 'A team, not a hierarchy',
    lines: [
      { beat: 'no-lead', head: 'No boss required.', say: "And you don't need a boss agent at the top." },
      { beat: 'peers', say: 'Give each agent its own job, and they reach each other directly. @tester runs the suite on @build, and tells @coder what failed.', seed: 5 },
      { beat: 'persona', head: 'They pick their own collaborators.', say: 'Describe what each agent is good at, and they pick their own collaborators from the roster.', show: 'Reviews Rust changes for correctness and risk' },
      { beat: 'group', head: '@review-team', say: 'Save a few of them as a group, and mention the group like a name.', show: '@review-team Review the authentication changes before I merge them.' },
    ],
  },
  {
    id: 'trust',
    title: 'Built to be trusted',
    lines: [
      { beat: 'pin', head: 'Pinned to the code it read.', say: 'Every review can be pinned to the exact code it read.' },
      { beat: 'verdicts', head: 'Fresh. Or stale.', say: 'When the code moves, Namedrop tells you whether that review is still fresh, or stale.' },
      { beat: 'status', head: 'See everything.', say: "Press Prefix, then Shift S, and see what every agent is doing, and what you're still waiting on." },
      { beat: 'doctor', say: "If something's stuck, namedrop doctor tells you exactly how to fix it.", show: 'namedrop doctor' },
      { beat: 'guardrails', head: 'Data. Never orders.', say: 'Context you share stays with the session you sent it to. And a message from another agent is always data, never orders.' },
    ],
  },
  {
    id: 'close',
    title: 'Namedrop',
    lines: [
      { beat: 'decisions', say: 'So you stop being the message bus, and get back to the decisions only you can make.' },
      { beat: 'wordmark', head: 'Namedrop', say: 'Namedrop.' },
      { beat: 'tagline', head: 'Name it. Mention it. Get the answer back.', say: 'Give an agent a name. Mention it. Get the answer back where you asked.' },
      { beat: 'install', say: 'Install it in Herdr today.', show: 'herdr plugin install ericbust/namedrop --ref main' },
    ],
  },
];
