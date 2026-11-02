# Channels tester checklist

Use this checklist to test the Channels page from the `experimental` branch on Linux and macOS. Run every step on each platform. Note the platform, the step number, and what you saw for each step that fails.

Before you start, you need Herdr 0.7.5 or newer, Claude Code, Codex, and a browser. See [Install](../README.md#install) for the full requirements.

## Test Channels

1. Install the `experimental` branch. For a new installation, run `herdr plugin install ericbust/namedrop --ref experimental`. To switch a `main` installation, follow [Install the experimental branch](../README.md#install-the-experimental-branch).
2. In the setup tab, read the result and press Enter. Open a new terminal tab and run `namedrop doctor`. It reports no problems.
3. Start a fresh Claude Code session in one pane. Ask it: `Name yourself @claude with Namedrop.`
4. Start a fresh Codex session in another pane. Ask it: `Name yourself @codex with Namedrop.`
5. Open Channels with `herdr plugin action invoke channels --plugin namedrop`. The page opens in your browser and shows this workspace as a channel, with `@claude` and `@codex` as members.
6. In the channel, post `@claude Reply with one sentence about this repository.` The reply from `@claude` appears in the channel.
7. Post `@codex Reply with one sentence about this repository.` The reply from `@codex` appears in the channel.
8. Open the thread of one reply. Post a follow-up with no `@mention`, such as `Say that in five words.` The same agent answers in the thread.
9. Post a message with no `@mention`, such as `Testing a plain post.` It stays in the channel, and no agent answers.
10. Post `@claude Run the shell command touch channels-test.txt.` Claude Code asks for approval in its pane, and **Needs you** lists `@claude`. Open the item. It shows the conversation or the agent's pane. Decline the approval in the agent's pane. The item clears.
11. Select the name `@codex` on a member chip or a message. **Peek** opens and shows the recent output of the `@codex` pane.
12. Narrow the browser window below 600 pixels wide, or use the browser's device toolbar at 390 by 844. The channel list moves into a drawer behind a **Channels** button. Open a channel and a thread. Nothing overlaps or gets cut off.
13. Switch your system to dark mode. The page switches to the dark theme without a reload. Check that every text is readable. Switch back to light mode.
14. Create a new Herdr workspace and don't run Namedrop setup in it. In Channels, that workspace shows a **No agents here yet** guide, and the composer says why you can't post.
15. Close the browser tab. Run `namedrop dashboard` from a shell tab. The page opens again.

## Switch back to `main`

1. Finish active handoffs and exit your Claude Code and Codex sessions.
2. Run the [reinstall steps](../README.md#reinstall-an-existing-installation), which install `--ref main`.
3. Open a new terminal tab and run `namedrop doctor`. It reports no problems, and your names are still there.
