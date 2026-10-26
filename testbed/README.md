# Namedrop testbed

Test a fresh Namedrop install before you merge a runtime change. One command
gives you a Herdr profile of its own, its own Herdr server, Namedrop installed
from the branch, pull request or checkout you name, and three named panes:
`@shell`, `@claude` and `@codex`. Your normal Herdr, Claude Code and Codex
settings are never read or changed.

Namedrop's [releasing guide](https://github.com/ericbust/namedrop/blob/main/docs/releasing.md#publish)
asks for this test in step 3. [CHECKLIST.md](CHECKLIST.md) is that test as
numbered steps.

## Requirements

Both platforms need Python 3.9 or newer, Git, Herdr, Claude Code, Codex, and
Rust 1.97 or newer, because Herdr builds Namedrop from source. Sign into Claude
Code and Codex once in your normal setup. The testbed links your login files
into each profile and never copies them.

- **Linux.** x86_64 or aarch64 with glibc. A C compiler (`cc`) must be on PATH.
- **macOS.** Apple silicon. Install the command-line developer tools with
  `xcode-select --install`. Homebrew tools in `/opt/homebrew/bin` are found
  automatically.

Run the check first. It lists each tool with its version and runs a self-test
that needs no network:

```sh
./testbed check
```

## Test a change with real agents

```sh
./testbed test --pr 27 --fresh --yes
```

`test` does CHECKLIST.md for you, with no prompts to answer. It builds the
profile `test-pr-27` the way `up` does, starts Claude Code and Codex, gets
through their first-run screens, sends the mentions, and checks every result
in Namedrop's own records and in the agents' transcripts. Allow 10 to 20
minutes. The first build takes most of that.

| Option | What it does |
| --- | --- |
| `--ref BRANCH`, `--pr N` or `--link PATH` | The source, as for `up` |
| `--fresh --yes` | Rebuild from nothing, with no typed confirmation |
| `--codex plain` or `--codex no-daemon` | How Codex starts. The default is `plain` on Namedrop 1.2.0 and later, and `no-daemon` before that |
| `--keep` | Keep the profile after a pass |

A pass prints the compatibility line to paste into `docs/compatibility.md`
and deletes the profile. The report itself stays in
`~/.namedrop-testbed/reports/`. A failure keeps the profile and its server
running, so you can look at it with `./testbed attach --profile test-pr-27`.
The evidence for every step is in the directory the report names. Running
`test` again on the same profile keeps the build and starts new panes and new
agent sessions. The exit code is 0 on a pass and 1 otherwise.

`test` uses only profiles whose names start with `test-`, so it never drives
or deletes a profile you work in. It answers the first-run screens it
recognizes and stops with the screen text on any other screen. It never
approves an agent's tool prompt: a pane that waits on one for 30 seconds
fails the step. With `--codex plain`, the run also checks Namedrop's
shared app-server warning (`ND-SESSION-003`), runs the two commands that
Doctor gives as the fix, and checks that the warning clears.

## Test a branch

The commands are the same on Linux and macOS. `attach` needs a plain
terminal outside Herdr. Every other command also works from a Herdr pane.

1. Create the profile and install Namedrop from the branch:

   ```sh
   ./testbed up --ref fix/token-cost
   ```

   The first run builds Namedrop and takes several minutes.
2. Open the profile's Herdr:

   ```sh
   ./testbed attach
   ```

3. Finish the first-run prompts yourself. Claude asks for a theme and asks
   you to trust the folder. Codex asks you to trust the folder and to review
   new hooks in `/hooks`. Read each prompt before you accept it.
4. Follow [CHECKLIST.md](CHECKLIST.md).
5. Close the terminal window that shows Herdr, then stop the profile's server:

   ```sh
   ./testbed stop
   ```

6. When you no longer need the profile, delete it:

   ```sh
   ./testbed destroy
   ```

   `destroy` asks you to type the profile name. Pass `--yes` to skip that.

## Start over

To throw the profile away and build it again from the same source, run:

```sh
./testbed up --fresh --yes
```

Then run `./testbed attach` again. When an agent exits, its pane keeps a shell
with the error on screen. `./testbed up` starts that agent again and recreates
a pane that was closed.

## Pick the source

`up` takes one source. A profile remembers its source, so later commands need
only the profile name.

| Source | Command | Default profile name | How Namedrop gets installed |
| --- | --- | --- | --- |
| Branch or tag | `./testbed up --ref BRANCH` | `ref-BRANCH`, with `/` as `-` | `herdr plugin install ericbust/namedrop --ref BRANCH`, the path users take |
| Pull request | `./testbed up --pr 27` | `pr-27` | From the PR's branch like `--ref`. A PR from a fork is cloned, fetched as `pull/27/head`, built and linked |
| Local checkout | `./testbed up --link ~/Projects/namedrop` | `link-namedrop` | Cloned into the profile, built and linked. Your checkout gets no build files |

`--repo OWNER/REPO` changes the GitHub repository for `--ref` and `--pr`. The
default is `ericbust/namedrop`. With no source and no `--profile`, `up`
resumes the only profile, or installs `--ref main` when no profile exists.

A pull request is looked up through the GitHub API. Without a token, GitHub
allows 60 lookups an hour. If you hit that limit, set `GITHUB_TOKEN`:

```sh
GITHUB_TOKEN=$(gh auth token) ./testbed up --pr 27
```

A `--link` test uses the checkout's last commit. Commit your work first. The
testbed warns when the checkout has uncommitted changes.

## Test several sources at once

Each profile has its own directory, server and socket, so profiles run side by
side. Name the profile on every command when more than one exists:

```sh
./testbed up --pr 27
./testbed up --ref main
./testbed list
./testbed attach --profile pr-27
```

With exactly one profile, every command picks it without `--profile`.

## Commands

| Command | What it does |
| --- | --- |
| `check` | Lists the tools and their versions, checks Python and the socket path, and runs the self-test |
| `up` | Creates or resumes a profile. Safe to run again: it starts what stopped and creates what is missing |
| `test` | Runs CHECKLIST.md against real Claude Code and Codex in a `test-` profile and prints the compatibility line on a pass |
| `attach` | Opens the profile's Herdr. Refuses inside another Herdr pane |
| `status` | Shows the source, server, Namedrop plugin, panes, names and `namedrop doctor` |
| `stop` | Stops the profile's server and keeps its files |
| `reset` | Restores the seed repository and starts new panes and agent sessions with the same install |
| `destroy` | Stops the server and deletes the profile |
| `list` | Lists every profile with its server state, Namedrop version and source |
| `report` | Prints one line for Namedrop's [compatibility page](https://github.com/ericbust/namedrop/blob/main/docs/compatibility.md) |
| `shell` | Opens a Bash shell with only the profile's environment |

Options for `up`:

- `--agents claude,codex` picks the agent panes. The default is both.
- `--no-launch` creates and names the panes but does not start the CLIs. Run
  `up` again without it to start them.
- `--fresh` destroys the profile first and installs the same source again. It
  asks for the same confirmation as `destroy`, which `--yes` skips.

To start an agent with other arguments, set its launch command before `up` or
`reset`:

```sh
TESTBED_CLAUDE_CMD='claude --model haiku' ./testbed reset
```

`TESTBED_CODEX_CMD` does the same for Codex. A changed command takes effect
when `up` or `reset` starts the agent again.

## Where profiles live

Profiles live in `~/.namedrop-testbed/profiles/NAME`. Set `TESTBED_HOME` to
use another directory. They live outside this repository so that `git clean`
here can never delete a running profile.

Each profile holds its own HOME, XDG directories, `CODEX_HOME`, Herdr
configuration and Namedrop state. Every Herdr, Namedrop, Git, Cargo, Claude
and Codex command the testbed runs starts from an empty environment with
those paths. The profile's socket is `/tmp/nd-tb-UID-HASH.sock`.

The workspace opens `repo/` in the profile, a small Git repository copied from
[seed/](seed). Its `AGENTS.md` and `CLAUDE.md` tell agents to reply briefly
and to leave files alone.

Before Codex starts, the testbed adds three settings to the profile's
`codex/config.toml` when they are missing:

- `approvals_reviewer = "auto_review"`: Codex reviews the approval a Namedrop
  reply needs, while the sandbox and hook trust prompts stay on.
- `model = "gpt-6-sol"` and `model_reasoning_effort = "high"`: every run uses
  the same model, whatever Codex's default is. `gpt-6.1-sol` answered only 5
  requests per 10 minutes on a Pro account in October 2026, fewer than one run
  sends, and then left Codex waiting minutes on `429 Too Many Requests`.

To try another model, pass it in the launch command, which wins over the file:
`TESTBED_CODEX_CMD='codex -m gpt-5.6-sol'`.

The testbed starts Codex as `codex --no-daemon`. A plain `codex` hands its work
to a shared background server that runs outside the pane. That server's hooks
cannot see which Herdr pane they serve, so Herdr never learns the Codex
session and mentions to `@codex` stay queued. The server also updates itself
every hour and restarts, and a running Codex then fails to save settings with
`remote app-server worker channel is closed`. Keep `--no-daemon` when you set
`TESTBED_CODEX_CMD`.
`test` builds its own Codex command from `TESTBED_CODEX_CMD`, or `codex`, and
adds or removes `--no-daemon` to match `--codex`.

To run your own command in a profile's environment, for example `codex login`
when `up` warns that a login is missing:

```sh
./testbed shell --profile pr-27
```

Run `exit` to leave. Bash users can also `source ~/.namedrop-testbed/profiles/NAME/env.sh`.

## Which Herdr runs

The testbed runs your installed `herdr` with a fresh profile. To test another
Herdr build, point `HERDR_BIN` at it before `up`:

```sh
HERDR_BIN=~/src/herdr/target/release/herdr ./testbed up --ref main --profile herdr-dev
```

The profile remembers `HERDR_BIN`. For every other tool, `up` looks up the
host's copy again on each run, so a profile follows your upgrades.
