# Test Namedrop with real agents

Run this test before you merge a Namedrop pull request that changes runtime
behavior. It is step 3 of [Publish](https://github.com/ericbust/namedrop/blob/main/docs/releasing.md#publish)
in Namedrop's releasing guide.

## Run it with one command

Replace `27` with your pull request number, or use `--ref BRANCH` or
`--link PATH`:

```sh
./testbed test --pr 27 --fresh --yes
```

Allow 10 to 20 minutes. The command does every step below by itself. It
exits with 0 on a pass. Paste its last line, the one that starts with `- **`,
under "What has been tested" in Namedrop's `docs/compatibility.md`. On a
failure it prints no such line, keeps the profile, and names the evidence
directory for the step that failed. Do not record a failed run as tested.

## Run it by hand

Use these steps when `./testbed test` cannot run here: on a machine where it
has not been run yet, such as macOS, or for Namedrop older than 1.1.0. Allow
about 15 minutes, most of it for the first build.

Run `./testbed attach` from a plain terminal outside Herdr. Every other
command also works from a Herdr pane. Run them in this repository. Replace `27` with your pull request number. To test
a branch instead, use `--ref BRANCH` in step 2.

1. Check the tools:

   ```sh
   ./testbed check
   ```

   Look for `check passed` on the last line.
2. Create a fresh profile with the pull request installed:

   ```sh
   ./testbed up --pr 27 --fresh
   ```

   `--fresh` deletes an older profile of the same name first, so the test
   starts from nothing. Type the profile name when asked, or pass `--yes` to
   skip that. Look for `Ready:` with the Namedrop version you expect.
3. Open the profile's Herdr:

   ```sh
   ./testbed attach
   ```

   Look for three panes: `@shell` on the left, `@claude` on the right, and
   `@codex` below it.
4. In `@claude`, finish the first-run prompts yourself. Claude asks for a
   theme and asks you to trust the folder. Read each prompt before you accept it.
5. In `@codex`, finish the first-run prompts yourself. Trust the folder, then
   run `/hooks` and review the Herdr and Namedrop hooks before you approve them.
   When an agent exits, its pane keeps a shell with the error on screen, and
   `./testbed up` starts the agent again.
6. In a second plain terminal, check the names and Namedrop's health:

   ```sh
   ./testbed status
   ```

   Look for `@shell`, `@claude` and `@codex` under `namedrop address list`.
   Look for `Namedrop checks passed` under `namedrop doctor`. If Doctor reports
   an issue, follow its repair step before you continue.
7. Send a mention from Claude to Codex. In `@claude`, submit:

   ```text
   Ask @codex to reply with the single word pong.
   ```

   Look for these results:
   - `@codex` receives the request one time.
   - `@claude` receives the reply one time and reports `pong`.
8. Send a mention from Codex to Claude. In `@codex`, submit:

   ```text
   Ask @claude to reply with the single word ping.
   ```

   Look for these results:
   - `@claude` receives the request one time.
   - `@codex` receives the reply one time and reports `ping`.
9. Optional: test a terminal recipient. In `@claude`, submit:

   ```text
   Run `echo hello` on @shell and tell me its output and exit code.
   ```

   Look for `hello` and exit code `0` in Claude's answer.
10. Open Agent status with **Prefix**, then **Shift-S**. Look for no messages
    that await progress. Close the panel with **Esc**.
11. If a step failed, write down what you saw and stop here. Do not record a
    failed run as tested. To try again from nothing, run
    `./testbed up --fresh --yes`, then start at step 3.
12. Print the compatibility line:

    ```sh
    ./testbed report
    ```

13. Paste that line under "What has been tested" in Namedrop's
    `docs/compatibility.md`. Add the models you used after each agent's
    version, such as `Codex 0.162.1 with gpt-5.5`. End the line with what you
    saw, for example: "Mentions went both ways between Claude and Codex, and
    each reply arrived once."
14. Close the terminal window that shows Herdr, then stop the profile's server:

    ```sh
    ./testbed stop
    ```

    To delete the profile, run `./testbed destroy`.
