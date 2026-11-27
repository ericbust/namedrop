# Run Namedrop in Herdr

This guide installs Namedrop as a [Herdr](https://herdr.dev) plugin, connects your agents, and covers keys, updates, troubleshooting, and removal. The [README](../README.md) describes what Namedrop does once it runs. [Operations and recovery](operations.md) covers every command and recovery step.

## Install

You need:

- **macOS on Apple silicon** or **Linux on x86_64** with glibc.
- **[Herdr](https://herdr.dev) 0.7.5 or newer**.
- **Codex or Claude Code**, and **Python 3.9 or newer** on `PATH`.
- **Git, Rust 1.97 or newer, and a C compiler and linker**, because Herdr builds Namedrop from source. On macOS, Apple's command-line developer tools provide the compiler. A [release archive](#install-from-a-release-archive) doesn't need these.

On Linux, install from a local Herdr workspace. A remote workspace installs Namedrop on the remote host. With Codex on Linux, Agent status summaries and persona suggestions also need Landlock and the official static Codex executable. `namedrop doctor` tells you what's missing. Linux aarch64 builds pass CI but nobody has tested them by hand yet.

### Install the plugin

Open a terminal in the Herdr workspace where you want to use Namedrop. Check the native integrations, then install or update the ones you use:

```sh
herdr integration status
herdr integration install codex
herdr integration install claude
```

Install Namedrop and open its setup tab:

```sh
herdr plugin install ericbust/namedrop --ref main
herdr plugin pane open --plugin namedrop --entrypoint setup
```

Herdr shows an install preview, then builds the executable and manages the checkout. Review the preview before you confirm. The setup tab activates Namedrop, shows any errors and next steps, and waits for Enter. **Keep Herdr running during setup. It doesn't need a restart.**

If you already use a local Namedrop checkout, follow the [migration steps](operations.md#migrate-a-local-link-to-a-managed-installation) first. Herdr won't install over a local link.

### Install from a release archive

Each [release](https://github.com/ericbust/namedrop/releases) has an archive with a prebuilt executable for each platform, so you don't need Git, Rust or a compiler. Download the `.tar.gz` and its `.sha256` file for your platform, then check and extract the archive. On macOS, use `shasum -a 256 -c` instead of `sha256sum -c`.

```sh
sha256sum -c namedrop-<version>-<target>.tar.gz.sha256
tar -xzf namedrop-<version>-<target>.tar.gz
```

Keep the extracted directory. Herdr runs Namedrop from it, so moving or deleting it breaks the installation. Then open a terminal in the Herdr workspace where you want to use Namedrop, and run setup from the extracted directory:

```sh
sh namedrop-<version>-<target>/scripts/setup.sh install
```

Setup checks that the executable runs on your machine, links the directory with Herdr, and activates Namedrop. Then [check the shell command](#check-the-shell-command) and [connect your agents](#connect-your-agents).

An archive installation doesn't use **Update Namedrop**. To update, extract the new release to a new directory and follow the [manual upgrade steps](operations.md#manual-upgrade). To remove it, use `herdr plugin unlink namedrop`, as [Uninstall](#uninstall) describes. The archives aren't signed or notarized, and nobody has tested a downloaded copy on macOS yet.

The archive's `scripts/install.sh` is the installer for Rex, not for Herdr. See [Run Namedrop on Rex](rex.md).

### Check the shell command

Setup installs `namedrop` in `~/.local/bin` and adds that directory to your zsh and bash startup files. **Open a new terminal tab inside Herdr**, then run:

```sh
namedrop doctor
```

Other shells need [manual PATH setup](operations.md#shell-command). Setup refuses to overwrite an unrelated `namedrop` command or a symlinked startup file.

Activation applies to the current workspace only. Open the setup tab in each workspace where you want agents to collaborate. To run setup inline or troubleshoot it, use the [setup helper](operations.md#setup-helper).

### Connect your agents

Follow the instructions that activation prints, then start **fresh agent sessions**:

- **Codex.** When `codex` is on `PATH`, activation installs or refreshes the local Namedrop plugin for you. Start a new session. If Codex asks, review and trust the Namedrop entries in `/hooks`.
- **Claude Code.** Start a new session, or run `/reload-plugins` to load the Namedrop integration.

To confirm the setup works, name two agents as the [README](../README.md) shows at the top. Send a short request from one to the other, check that the reply comes back, and then try the other direction. A clean install or health check alone doesn't prove that the hooks fired.

## Keyboard shortcuts

Setup binds Namedrop's popups to Herdr's prefix key. Press **Prefix**, release it, then press the key:

| Keys | Opens |
|---|---|
| **Prefix**, then **Shift-S** | [Agent status](../README.md#agent-status) for the current pane |
| **Prefix**, then **Shift-I** | Name this pane |
| **Prefix**, then **Shift-M** | [Manage names](../README.md#manage-names) |
| **Prefix**, then **Shift-H** | [Namedrop Doctor](../README.md#troubleshooting) |
| **Prefix**, then **Shift-O** | [Namedrop tour](../README.md#tour) |

Setup keeps any binding you already have for these actions, including a custom key. If another action already uses one of these keys, setup leaves that key alone, binds the rest, and reports the conflict.

To change a key, edit its `namedrop.*` entry in Herdr's configuration and run `herdr server reload-config`. Herdr reads the file at `HERDR_CONFIG_PATH` if you set it. Otherwise it reads `~/.config/herdr/config.toml`, or the same path under `$XDG_CONFIG_HOME`.

To open a popup without a key, run `herdr plugin action invoke <action> --plugin namedrop`. The actions are `agent-status`, `name-agent`, `manage-addresses`, `doctor`, and `tour`. The `channels` action opens [Channels](../README.md#channels-beta) in your browser. [Popups](operations.md#popups) describes each popup.

## Pause, resume, or deactivate Namedrop

Each of these acts on the current workspace and keeps its names and messages:

- **Pause Namedrop** stops all collaboration in the workspace. Messages wait until you resume.
- **Resume Namedrop** starts it again.
- **Deactivate Namedrop** turns Namedrop off in the workspace. Open the setup tab again to turn it back on.

Run them as Herdr actions, or from a shell tab in the workspace:

```sh
herdr plugin action invoke pause --plugin namedrop
herdr plugin action invoke resume --plugin namedrop
namedrop deactivate
```

`namedrop space pause` and `namedrop space resume` do the same as the first two. While a workspace is paused, Doctor shows `ND-SPACE-001`. [Stop collaboration temporarily](operations.md#stop-collaboration-temporarily) covers deactivation and disabling the plugin.

## Update Namedrop

Namedrop checks for a newer version in the background, at most once a day. For GitHub-managed installations that follow a branch or tag, Doctor shows the installed and available versions. A new Claude Code or Codex session also shows one update line, at most once a day across all panes for that version. The line is visible to you and adds no agent context. Checks never download or install an update automatically. Local links, archives and full commit pins make no update requests.

For a GitHub-managed installation, run this from a plain shell tab:

```sh
herdr plugin action invoke update --plugin namedrop
```

The **Update Namedrop** tab checks your installed branch or ref for new commits, asks **Install update now?**, and then installs and activates it. A compatible update keeps your agents running. Panes, workspaces, names, groups, handoffs, and pause settings stay intact. Existing sessions keep the instructions they loaded, and new sessions load the updated ones.

To turn off both background checks and notices, put `{"check_for_updates":false}` in `settings.json` inside the directory printed by `herdr plugin config-dir namedrop`. Create the file and directory if needed. Set it to `true`, or remove that setting, to enable checks again. Invalid settings disable checks, and they also stop Namedrop's own Claude Code calls, described next. Doctor explains the error as `ND-UPDATE-002`, which does not mark Namedrop unhealthy, and as `ND-MODEL-002`, which does. This setting survives updates and `namedrop purge`.

Agent-status summaries, persona suggestions and channel routing run `claude` with the environment of the Herdr server, and Channels' **Add agent** types `claude` into a new pane. To use another Claude Code account for them, set `claude_command` in the same `settings.json` to a program name or an absolute path, such as a launcher that selects another account. Doctor's `ND-MODEL-002` names the program they run. [Choose the Claude Code program](operations.md#choose-the-claude-code-program) gives the rules.

- **Older installations.** An upgrade from an older installation may need one session refresh. The tab lists the affected sessions by workspace, tab, and pane. Choose **Wait here** or **Later**, and refresh them when you're ready. Complete any Codex cache-refresh steps that setup prints.
- **Nothing is forced.** An update keeps your installed source and ref, including a fixed pin. It stops and cancels nothing. If activation fails, Namedrop selects the previous runtime again. Build and setup details go to `last-update.log` in Namedrop's state directory.
- **Other workspaces.** Other workspaces keep their activation. Run `namedrop doctor` in each one, and open setup if Doctor reports a problem.
- **Rex on the same machine.** If you also use Namedrop in Rex on this machine, and `install.sh` adopted this installation, **Update Namedrop** updates Namedrop for Rex too. See [Use Herdr and Rex on one machine](rex.md#use-herdr-and-rex-on-one-machine).

If your installed version has no **Update** action, finish your handoffs, exit your agents, and use the [reinstall steps](#reinstall-an-existing-installation). Local checkouts and archives use the [manual upgrade steps](operations.md#manual-upgrade). Version 1.5.0 upgrades the database in place to schema 7 and keeps names and history. Start new agent sessions after updating to load the reminder commands and updated instructions. Take a backup before upgrading across schemas. The [upgrade guide](operations.md#upgrade) covers migration, backups, and recovery.

### Reinstall an existing installation

These steps remove Herdr's managed checkout and install the latest `main`. Your saved names and message history stay. Before you start, finish active handoffs and exit your Claude Code and Codex sessions. Keep Herdr running. In a shell tab in the workspace you want to activate, run:

```sh
cd "$HOME"
namedrop deactivate
herdr plugin disable namedrop
herdr plugin uninstall namedrop
herdr plugin install ericbust/namedrop --ref main
herdr plugin pane open --plugin namedrop --entrypoint setup
```

The first command moves your shell out of the checkout that the uninstall deletes. If you installed from another owner, repository, or ref, use yours. Read the setup result. Then open a **new terminal tab**, run `namedrop doctor`, and [start fresh agent sessions](#connect-your-agents). For a local checkout or archive, follow the [migration guide](operations.md#migrate-a-local-link-to-a-managed-installation) instead.

## Troubleshooting

Run `namedrop doctor` in the Herdr workspace that has the problem, or press **Prefix**, then **Shift-H** to open the **Namedrop Doctor** panel. [Troubleshooting](../README.md#troubleshooting) in the README explains what Doctor reports and how to read it.

If the `namedrop` command isn't available, open a new terminal tab inside Herdr. If it's still missing, run `sh scripts/setup.sh check` from the installed package directory. The [setup helper](operations.md#setup-helper) shows how to find that directory. [When something does not work](operations.md#when-something-does-not-work) lists common problems and their fixes.

## Uninstall

Finish active handoffs and exit your Claude Code and Codex sessions. Keep Herdr running. If several workspaces use Namedrop, run `namedrop deactivate` in each one before you disable the plugin. Then, in a Herdr shell tab, run:

```sh
cd "$HOME"
namedrop deactivate
herdr plugin disable namedrop
herdr plugin uninstall namedrop
```

For a local checkout or an extracted archive, use `herdr plugin unlink namedrop` instead of `uninstall`. Your local files stay where they are.

**Your names, groups, message history, and configuration remain.** So do the agent integrations, the shell launcher, and the `PATH` entries, so a reinstall can refresh them. To remove those too, follow the [remaining cleanup steps](operations.md#remove-agent-integrations-and-shell-command). Don't remove unrelated skills, shell settings, or Herdr's native integrations.

If you also use Namedrop in Rex on this machine, Herdr's installation owns the Rex install too. [Use Herdr and Rex on one machine](rex.md#use-herdr-and-rex-on-one-machine) explains what that means before you remove it.
