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
Rust 1.97 or newer, because Herdr builds Namedrop from source. Sign into Codex
once in your normal setup. The testbed links its login file into each profile
and never copies it. Claude Code needs a token file of its own, as
[The Claude Code account](#the-claude-code-account) explains.

- **Linux.** x86_64 or aarch64 with glibc. A C compiler (`cc`) must be on PATH.
- **macOS.** Apple silicon. Install the command-line developer tools with
  `xcode-select --install`. Homebrew tools in `/opt/homebrew/bin` are found
  automatically.

## The Claude Code account

Every Claude Code session the testbed starts uses one long-lived token. The
testbed never uses this machine's default Claude Code login, so test runs bill
the account you chose and not, for example, a team organization you are signed
in to for work. Create the token once, under the account that pays for test
runs:

1. In your browser, sign in to claude.ai with the account to bill.
2. Run `claude setup-token` and approve the request in the browser. It prints
   the token.
3. Save the token with no echo, so it never reaches your shell history or a
   command line:

   ```sh
   mkdir -p ~/.namedrop-testbed
   (umask 077 && read -rs token && printf '%s\n' "$token" > ~/.namedrop-testbed/claude-oauth-token)
   chmod 600 ~/.namedrop-testbed/claude-oauth-token
   ```

   Paste the token at the silent prompt and press Enter.

The token file is `$TESTBED_HOME/claude-oauth-token`. Set
`NAMEDROP_TESTBED_CLAUDE_TOKEN_FILE` to use another file. The testbed refuses
a token file that other users can read.

The profile's `claude` is a small wrapper. It reads the token file when Claude
Code starts and sets `CLAUDE_CODE_OAUTH_TOKEN` for Claude Code and the
commands and hooks it runs. The testbed never puts the token on a command
line, in `env.sh`, in the profile's `state.json`, or in a log or report. An
agent that prints its environment can still show it in its transcript. A token
profile links neither `~/.claude/.credentials.json` nor the login keychain,
and `up` removes those links from a profile an earlier version made. Every
Claude Code start in the profile goes through the wrapper: the Herdr lane,
`--inside-rex`, `rex up` and the `rex` lines it prints, the daily-use and
upgrade lanes, and the verify skill's probe scripts. They start it by its full
path, `PROFILE/bin/claude`, because a pane's login shell can put
`/usr/local/bin` and the system directories ahead of the profile's `bin`,
where another `claude` would start without the token. The profile's
`home/.bashrc` puts `bin` first again, so a `claude` you type in a pane is the
wrapper too.

Before the first prompt, `up`, `test` and `rex up` run `claude auth status
--json` in the profile. For a token, they check that Claude Code signs in with
it (`authMethod` is `oauth_token`). Claude Code shows no plan or organization
for a token, so the testbed asks the OAuth profile endpoint Claude Code reads,
`https://api.anthropic.com/api/oauth/profile`. It sends the token to that
address only and follows no redirect.

A token from `claude setup-token` carries only the inference scope, and the
endpoint refuses it with HTTP 403 and a `permission_error`. So for such a
token, which is the usual case, the account line always reads `plan and
organization unreadable with this token's scope, signed in with the testbed
token`, and the run goes on. Neither Claude Code nor the testbed can name the
account the token belongs to. Confirm it once, out of band: after the first
run, check that its usage shows on your personal account and not on a team
organization.

HTTP 401 means the token was revoked or mistyped. The run stops: create a new
token with `claude setup-token` and save it as above. Any other lookup
failure, a redirect or a 403 without `permission_error` included, stops the
run too. The token file must hold the token alone on one line.

The plan check works only when the plan is readable: for a token that has the
profile scope, and for `--claude-default-login`, whose `claude auth status`
names the plan. A Max or Pro plan runs, with an account line such as `max
plan, personal account, signed in with the testbed token`. A team or
enterprise plan, or an account whose plan is unknown, runs only when
`--claude-org NAME` names its organization, and an account with no
organization never runs. A personal account's organization is named after its
email address, so neither the line nor a refusal shows that name. The account
line heads the `test` report and follows the `Ready` line of `up` and `rex
up`.

A profile has Claude Code only when its run starts Claude Code. A run
without it, such as `up --agents codex`, needs no token and no check, and
leaves no `claude` in the profile's `bin` and no link to the default login.
A profile that has either one is trusted only after its account check
passed, and a refused check removes the default login's links again.
`attach`, `shell` and `reset` refuse such a profile with no passed check.
That includes a profile an older testbed made, which has both even for
`--agents codex`. Run `up` on it first, with `--agents codex` if it needs no
Claude Code. The verify skill's probe scripts refuse any profile with no
passed check. `up`, `test` and `rex up` also refuse to change how a running
profile's Claude Code signs in, because a Claude Code already running keeps
its login. Run `./testbed stop --profile NAME` first, and the next start
launches Claude Code again.

With no token file, the testbed refuses to start Claude Code and prints these
steps. A maintainer who wants this machine's default login passes
`--claude-default-login`. A run that starts Claude Code then links the default
login as before and still checks the account, so a team organization still
needs `--claude-org`. A run without Claude Code links nothing for it.

What the token setup does not cover yet:

- Claude Code passes `CLAUDE_CODE_OAUTH_TOKEN` on to the commands, hooks and
  agents it starts. `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` would remove it there,
  but it also forces the default permission mode, so the lanes would stop at
  permission prompts. The testbed does not set it.
- `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` might keep the token out of every
  environment. It is untested.
- Codex still signs in through a link to this machine's `~/.codex/auth.json`.

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
| `--daily-use` | Run the daily-use lane instead of CHECKLIST.md |
| `--upgrade-check`, optionally with `--upgrade-from REF` | Activate `--link` over the previous release with deliveries open |
| `--inside-rex` | Run the profile's Herdr server in a block of a scratch Rex server, which keeps the block's `REX_*` names in Herdr's environment |
| `--claude-org NAME` | Allow Claude Code to bill this team or enterprise organization. `up` and `rex up` take it too |
| `--claude-default-login` | With no token file, use this machine's default Claude Code login. `up` and `rex up` take it too |

A pass prints the compatibility line to paste into `docs/compatibility.md`
and deletes the profile. The report itself stays in
`~/.namedrop-testbed/reports/`. A failure keeps the profile and its server
running, so you can look at it with `./testbed attach --profile test-pr-27`.
On `--inside-rex`, a failure stops both servers and deletes the profile
instead, and the profile's logs stay in `reports/<profile>-<time>-failed/`.
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

### Daily-use lane

```sh
./testbed test --link . --fresh --yes --daily-use
```

The lane tests what a person's day runs through: Channels posts, thread replies
and DMs to each agent, recaps, warnings, tracked handoffs and nudges, a turn held
on a helper, compaction, exit and resume, a crash, a second workspace, reminders,
and an empty backlog at the end. Each check runs even after an earlier one
failed, so a run on a buggy build reports everything it finds. The exit code is
0 only when every check passed. `--upgrade-check` is a separate run of a
person's update with agents working. The
[verify skill](../.claude/skills/verify/SKILL.md#daily-use-lane) lists what each
step asserts.

To judge another commit, point this kit at a detached worktree of it:

```sh
git worktree add --detach ../nd-old <commit>
./testbed test --link ../nd-old --fresh --yes --daily-use
```

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
| `rex up`, `rex down` | Installs the build with its archive's `install.sh`, with no Herdr, and runs Claude Code and Codex in blocks of a scratch Rex server, in a `rex-` profile. See [The Rex lane](#the-rex-lane) |
| `rex-remote up`, `check`, `down` | Runs the relay between a scratch Rex server here and a Linux Rex server in Docker, over `ssh://`. See [The remote-host lab](#the-remote-host-lab) |

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

The command must start with `claude`, because only the profile's `claude`
sets the token. The testbed refuses any other command for `@claude`. It types
the command as `PROFILE/bin/claude` followed by the other words, each quoted, so
shell syntax such as `;` or `$VAR` reaches Claude Code as plain arguments.
`TESTBED_CODEX_CMD` sets the Codex command. A changed command takes effect
when `up` or `reset` starts the agent again.

### The Rex lane

```sh
./testbed rex up --link /path/to/namedrop --fresh --yes
./testbed rex down --link /path/to/namedrop
```

`rex up` installs Namedrop as a machine with Rex and no Herdr does. The
profile gets no `herdr`: its `bin` links none, and the lane's `PATH` drops each
host directory that holds one, such as Homebrew's. `rex up` prints that `PATH`
and stops if a `herdr` is still on it or beside the lane's `rex`, whose
directory the Rex server puts on every block's `PATH`. A `herdr` in `/usr/bin`,
`/bin`, `/usr/sbin` or `/sbin` stops the lane with an error that says so,
because the build and the install need those directories. Every command the
testbed runs in a `rex-` profile, `env.sh` and `testbed shell` included, gets
the lane's environment. It starts a scratch Rex server under
`env -i`, with the profile's environment minus its `HERDR_*` names and with
`REX_SERVER` and `REX_HOME` naming the server. So a block's only location names are the
`REX_*` the server gives it, and the server's own files stay in the profile's
`HOME`. It then clones the source into the profile, builds the release archive
with the source's own `scripts/package-release.sh`, checks the archive's
checksum, unpacks it, and runs its `scripts/install.sh --yes` with the
lane's `rex` first on `PATH`, so the install takes that `rex` and no other. The install's report follows: it puts
the state in `PROFILE/state/namedrop`, the module and the `require` line in
the scratch server's `init.lua`, and the Claude Code and Codex packages into
the profile with every hook baked to that state directory. A profile installs
once, so a new build needs `--fresh`. A profile that an older testbed
activated in a profile Herdr is refused. The release build and the install
log to `logs/package-release.log` and `logs/install.log`. Claude Code in a
block starts as `PROFILE/bin/claude`, the profile's token wrapper, named in
full because a block's shell can put another `claude` ahead of the profile's
`bin`. A scratch server started by hand needs the same wrapper and its own
`HOME`, with no keychain link, as the probe recipe in
[ADAPTER.md](../experimental/rex/ADAPTER.md#reproduce-the-probes) shows.

The lane runs the first `rex` on `PATH`, and a profile keeps the one it first
ran. To run another Rex build, put its `rex` first on `PATH` and pass `--fresh`.

A login shell runs `/etc`'s `path_helper`, which puts the host's directories,
such as Homebrew's through `/etc/paths.d/homebrew`, back on `PATH`. Codex's and
Claude Code's tool shells are login shells. So `rex up` writes a `PATH` filter
first in the profile `HOME`'s `.zprofile`, `.bash_profile` and `.profile`,
and keeps the rest of each file. It then runs `command -v herdr` in a login
`zsh`, `bash` and `sh` in the lane's environment, and each must find none.

The session `testbed-rex` gets a shell block and one block per agent. The
shell block runs `bash --noprofile` with the profile's `.bashrc`, and `up`
types a `herdr` lookup into it, which must find none. On a repeat `up`, a saved
shell block whose foreground is a program gets nothing typed, and `up` says so.
A saved shell block at its prompt has its typed line cleared first. Claude Code runs as
`PROFILE/bin/claude` and Codex as `codex --no-daemon`, whose model settings are in
the profile's `codex/config.toml`. The lane answers the first-run prompts the
Herdr lane knows, starts Codex again after it trusts its hooks, and sends Codex
the Herdr lane's warm-up prompt, because Codex runs its hooks only from its
first prompt. It is ready once each agent ran a Namedrop hook from a script
that passes the profile's state directory: Codex's hook run is recorded in
`<state>/hook-runs`, and Claude Code's SessionStart hook shows in
`logs/claude-hooks.log`, because Claude Code gives its hooks no `PLUGIN_ROOT`
and Namedrop records no hook run for them. `up` then prints the `rex` lines
that start more of the profile's agents, with Claude Code's wrapper named in
full. `rex down` kills every session, stops the server and
any Codex app server, and keeps the profile. No Herdr runs on this lane.

### The remote-host lab

```sh
./testbed rex-remote up --rex-bin "/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex"
./testbed rex-remote check
./testbed rex-remote down
```

The lab tests Namedrop's relay between two Rex servers over `ssh://`, with no
model and no machine outside this one. A is a scratch Rex server on this
machine. B is a Debian container that runs Superlogical's signed apt Rex
package and an sshd. A reaches B as the host `labb`, with
`rex hosts add labb ssh://nd@127.0.0.1:PORT`. Each side runs its own
Namedrop: a macOS build on A, and a Linux build made in Docker's `rust:1.97.0`
image on B.

It needs Docker, Git, Cargo, and a macOS `rex`. `--rex-bin` names A's `rex`,
otherwise `REX_BIN` or the `rex` on PATH. B runs A's Rex version unless
`--b-rex 0.1.0-preview.19` names another. `--link PATH` names the checkout to
build, by default this kit's own, and the lab builds its last commit.

`up` does these steps. Each step does only what is missing, so `up` on a
running lab changes nothing, and `up` after `down`, after a stopped or paused
container, or with another `--b-rex` brings back only what stopped or changed:

1. Makes the lab's directory and its marker `ndrp-rexlab.json`. `up`, `check`,
   and `down` refuse a directory that holds files and no marker, so `--dir`
   and `--fresh` never write into or delete from another directory.
2. Makes an ed25519 key, an ssh config, and an empty known_hosts in the lab's
   `ssh/` directory, and copies [rex-remote/ssh](rex-remote/ssh) to `bin/ssh`.
   That wrapper runs `/usr/bin/ssh -F ssh/config`, and the
   [config](rex-remote/ssh_config) names only that key and that known_hosts,
   with no agent and no global known_hosts. Rex's ssh transport passes its own
   `ControlMaster` and `ControlPath` on the command line, which win over the
   config: its tunnels pass `ControlMaster=no`, and its first probe of a host
   opens a control socket under `/tmp/rex-UID/<hash>/ssh/` and closes it with
   `-O exit`. None is under `~/.ssh`. Each ssh run is logged to
   `logs/ssh-calls.log`.
3. Builds Namedrop for both sides, and B's image from
   [rex-remote/Dockerfile](rex-remote/Dockerfile). The Dockerfile pins the
   Debian base image by digest, the Rex package version, and the fingerprint
   of Superlogical's signing key. The build stops when the downloaded keyring
   holds another key. The apt source's `Signed-By` names that key, so apt
   installs only packages that key signed. The build also stops when the
   installed `rex` does not name Superlogical as its maintainer. A package
   declares its own maintainer, so that check only tells Superlogical's `rex`
   from Debian's unrelated `rex`.
4. Starts B's container with sshd and Rex ports on `127.0.0.1` only. The
   public key is mounted read-only. The image holds no user key, no client key
   and no authorized key, but it does hold sshd's host keys, made when the
   image is built, so each container of one image has the same host keys. `up`
   starts a stopped container and unpauses a paused one. A new Rex version
   for B replaces the container. It starts B's Rex server as the user `nd`,
   because a container has no systemd.
5. Copies B's Namedrop into the container when B lacks that build. A
   Namedrop running on B is stopped first, because a bridge whose binary was
   replaced answers every relay call `retry`.
6. Starts A's server on `/tmp/nd-tb-rexremote-UID-HASH.sock` with
   `HOME=LAB/home-a`, and lists B as `labb`. When a new container has another
   port, `up` runs `rex hosts edit labb --endpoint`. It names a terminal
   `@lead` on A once, and a terminal `@bterm` on B once per container, opts B
   in to terminal recipients with `relay/allow-terminals`, and starts both
   bridges. A's server restores `@lead`'s session after `down`.
7. When A's relay still pins `labb` to the `host_id` of an earlier container,
   sends one item to `@bterm@labb`. A's relay takes a `host_id` only from a
   live `server.status` in a worker round, so that round records the new B.
   Items still pinned to the old B fail as `labb now answers as another Rex
   server`.

Every `rex` that A runs starts from `env -i` with the lab's `HOME`, the lab's
`bin` first on PATH, `REX_HOME` naming A's socket, `--autostart=false`, and an
explicit `-S`. So no lab call reaches your own Rex server, your Rex hosts list,
or `~/.ssh`.

`check` runs the lab checks that `experimental/rex/PORT.md` lists for PLAN #11
and prints `ok`, `FAIL`, or `known` for each, with the evidence it read. A
`known` result is a behavior that PORT.md records as a caveat. `--only 2,3`
runs some of them. Each check that stops, pauses, or restarts B, or lists
`labb-http`, puts the lab back before it ends, even when it fails. The report
is in `LAB/reports/`.

`down` stops the lab's processes and removes B's container and the tunnel
directory under `/tmp/rex-UID` that A's ssh log names. A process is the lab's
when its command starts with the lab's Namedrop binary or with `/usr/bin/ssh -F
LAB/ssh/config`, or when it is A's server with the pid, start time, and
command line that `up` recorded. `down` never signals itself or its ancestors,
and a process that only names the lab's socket or directory in its arguments
is not the lab's. `down` keeps the lab's directory, so the next `up` reuses
its builds, A's state, and `@lead`. `up --fresh` after `down` starts from a
new key, hosts list, names, and state, and still reuses the builds.
The lab's directory is `TESTBED_HOME/rex-remote` unless `--dir` names
another. The images, `ndrp-rexlab:rex-VERSION`, stay for the next run.

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
