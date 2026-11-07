# Channels Beta tester checklist

Test the Channels Beta included in the normal `main` installation on Linux and macOS. Run every step on each platform. Record your platform, Herdr, Namedrop and agent versions, the failed step, and what you saw. Remove private conversation text and credentials before sharing screenshots or logs in an [issue](https://github.com/ericbust/namedrop/issues).

You need Herdr 0.7.5 or newer, Claude Code, Codex, and a browser. See [Install](../README.md#install) for the full requirements. Use a test repository for commands that create files.

## Test Channels

1. Follow [Install the plugin](../README.md#install-the-plugin) for a normal `main` installation. If Namedrop is already installed, follow [Update Namedrop](../README.md#update-namedrop). Channels needs no separate installation.
2. In the setup tab, read the result and press Enter. Open a new terminal tab and run `namedrop doctor`. Resolve reported setup problems before continuing.
3. Start a fresh Claude Code session in one pane. Ask it to `Name yourself @claude with Namedrop.`
4. Start a fresh Codex session in another pane. Ask it to `Name yourself @codex with Namedrop.` Fresh sessions load the reminder commands.
5. Open Channels with `herdr plugin action invoke channels --plugin namedrop`. The browser shows this workspace as a channel with `@claude` and `@codex` as members. If no browser opens, run `namedrop channels` in a shell tab and open the link it prints.
6. Post `@claude Reply with one sentence about this repository.` Its reply appears as a preview under the channel message. Open the thread and check the full reply.
7. Post `@codex Reply with one sentence about this repository.` Open its thread and check the reply.
8. In a thread with only one eligible agent, post `Say that in five words.` without `@`. The same agent answers in the thread. The channel keeps the top message, reply count, and latest reply preview.
9. In a thread, explicitly ask the other agent to join with `@claude` or `@codex`. Once both agents participate, type a plain reply. The hint asks you to pick an agent. Sending keeps the reply unsent until you choose recipients with `@name`.
10. Post `Testing a plain post.` at channel level without a mention. It stays in the channel and no agent answers. Type `@` and check that completion offers the channel's agents.
11. Select `@claude` under **Direct messages**. Send `Reply with one sentence about this repository.` without a mention. Its answer stays in the DM and does not appear in the channel or **All activity**. Repeat with `@codex`. Check the workspace label beside each DM name.
12. In a thread, ask `In 2 minutes, reply here with reminder received.` The agent confirms the schedule and a pending reminder appears. Wait for its reply in that thread. Repeat in a DM and check that the reply stays there.
13. Ask `Every 2 minutes, twice, reply here with scheduled check.` Check that the agent confirms a repeating reminder. Use **Cancel** in that thread or DM. The pending reminder disappears. Check that it does not fire after its due time. In a plain Herdr agent pane, ask for a reminder too and check that the answer returns there.
14. In the Claude Code pane, select manual permission mode. Ask it through Channels to run `touch channels-test.txt`. If it asks for approval, **Needs you** lists the agent. Open the item, inspect the pane, and approve the command there. The item clears after the approval. Delete the test file. If your permission settings already allow this command, use an action that requires approval in your test setup.
15. Select an agent's name on a member chip or a message. **Peek** opens in **Live** mode. Check that output updates. Scroll up, then return to the latest output. Use **Expand** and **Restore**. Switch to **Text snapshot** and use **Refresh**.
16. In Live Peek, open **Type to agent** and submit a harmless prompt such as `Reply with peek received.` Check the agent's pane and reply. This sends input to the real pane. Use Esc or arrow-key controls only when they make sense for the screen you see.
17. Address one agent in a message box and type `/` or `$`. Check its skill suggestions. Repeat in **Type to agent**. Choosing a suggestion inserts syntax for that agent's runtime.
18. Ask an agent to run a short task with a subagent. Check its member chip and Peek header while the subagent runs. The count returns to zero when it finishes.
19. In an agent conversation that reaches the **Automatic Reply Limit**, send a new message yourself. It resumes with a fresh allowance. A manually stopped thread stays stopped.
20. Narrow the browser below 600 pixels, or use a 390 by 844 viewport. The channel list moves into a drawer behind **Channels**. Open a channel and a thread. Check that controls remain visible and usable.
21. Switch your system to dark mode. The page changes theme without a reload. Check the text and controls, then switch back to light mode.
22. Create a Herdr workspace without activating Namedrop. Channels shows **No agents here yet** and explains why you cannot post.
23. Close the browser tab. Run `namedrop channels --no-open` in a shell tab and open the new link. Channels opens again. Keep the link private because it grants access to your local conversations.

## Finish the test

1. Run `namedrop doctor` and save any issue codes. An unanswered or declined request may leave a warning. Read the suggested repair before you act. Do not resend uncertain work.
2. Cancel any remaining test reminders before ending the agent sessions.
3. Remove test files and close the test workspace when you no longer need it. Channels remains part of the normal `main` installation.
