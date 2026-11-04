# Transient blank paint while Codex streams

This is a local draft for Herdr maintainers. It has not been posted upstream.

Herdr 0.9.1 published a wholly blank terminal projection during a streamed Codex answer, then restored content in the next paint. Namedrop forwarded the two payloads unchanged. The capture does not establish whether the original Codex output or Herdr's transaction handling produced the blank state.

The capture used Codex 0.164.0 with GPT-6-Sol high and `--no-daemon`, and Namedrop 1.5.0-exp.10 at `28c44a93a0442111bfbb52f6778541738a0d69d9`. All applications ran in an isolated `test-` profile.

To reproduce the workload:

1. Start real Codex in an isolated Herdr profile and record `herdr terminal session observe <terminal-id>`.
2. Ask for approximately 150 numbered prose lines about prime numbers, with a full sentence per line and no tools.
3. Follow with three plan steps and explicit plan updates around read-only shell commands. Finish another turn with 40 numbered prose lines.
4. Observe the source projection while the prose streams and completed output enters history. In the Namedrop reproduction, browser widths cycled through 820, 1700, 960 and 1440 pixels, with Expand and Restore. These browser changes do not control the source pane.

The captured observer projection was 120 columns by 40 rows. Paint 736 was a 754-byte delta that replaced the remaining characters with spaces. Replaying that completed paint in xterm 6.0.0 produced a wholly blank grid. Paint 737 was a 1,062-byte delta that restored content. Their observer timestamps were 59.059 ms apart. Browser DOM samples were blank and then populated 22 ms apart. These spacings are not exact physical display durations.

Both paints had `full: false` and one matching DEC mode 2026 begin/end pair. No reset or resize occurred near the browser blank. The nearest fit call was approximately 1.7 seconds later.

[The paired paint bytes](paired-deltas.json) preserve the original base64 payloads and metadata. Their decoded SHA-256 hashes are:

- Paint 736. `ed426454cdd4d33dd3650f79d80784c8d8975daffc92950a0a972447088e18b3`.
- Paint 737. `36122dcd6a0047391dfa9fab1f577625783ee450234fa628eea66ca45b185635`.

The open question is whether Codex exposed a clear outside its original 2026 transaction, or Herdr published an intermediate state while that transaction remained open. Herdr's synthesized paint markers cannot answer that. A later raw PTY recording did not reproduce the blank and changed terminal geometry, so it does not resolve the original transaction boundary.
