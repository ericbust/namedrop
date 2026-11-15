# U14 operator check: the palette in the Rex app

This is the operator half of [UI.md](UI.md)'s U14, the live check for step 6 of the [plan](PLAN.md). The automated half ran on the testbed's Rex lane ([PORT.md](PORT.md#step-6-commits)). The Rex app attaches only to its account's default server, so no driver can show its palette. You run this half by hand.

You need:

- A Mac on Apple silicon with the Rex app installed in `/Applications`.
- Your usual account, with Git and Cargo, to build the archive.
- A disposable macOS account. Its default Rex server is a test server, and it has no other Rex config: no `~/.config/rex`, no `REX_*` variable in its shell files, and no `rex hosts` entry. Never run this check in an account whose Rex or agents you use.

Write each result in the [evidence table](#evidence) as you go. Copy text exactly, and write "nothing" when a step shows nothing.

## 1. Build the archive (your usual account)

1. Clone your Namedrop checkout into a new directory, and check out the commit under test there. A local clone needs no network and leaves your checkout as it is:

   ```sh
   git -C <your checkout> fetch origin
   git clone --quiet <your checkout> /tmp/nd-u14-src
   cd /tmp/nd-u14-src
   git checkout --detach <commit under test>
   ```

2. Build the archive and its checksum into a directory that the disposable account can read:

   ```sh
   sh scripts/package-release.sh --output /Users/Shared/nd-u14
   chmod -R a+rX /Users/Shared/nd-u14
   ```

   The last two lines it prints are the archive, `namedrop-VERSION-aarch64-apple-darwin.tar.gz`, and its `.sha256` file. A release build can take several minutes.

3. Delete the build directory: `rm -rf /tmp/nd-u14-src`.

## 2. Open the Rex app (disposable account)

1. Log in to the disposable account.
2. Confirm that it has no Rex config. `ls ~/.config/rex` must fail with "No such file or directory".
3. Open the Rex app from `/Applications`. Wait until a terminal pane shows a shell prompt. The app has now started the account's default Rex server.
4. In that pane, run `command -v rex` and `rex version`. Write both in the table.

## 3. Install from the archive (in the Rex pane)

1. Copy the archive, check it, and unpack it:

   ```sh
   mkdir ~/nd-u14 && cd ~/nd-u14
   cp /Users/Shared/nd-u14/namedrop-*.tar.gz /Users/Shared/nd-u14/namedrop-*.tar.gz.sha256 .
   shasum -a 256 -c namedrop-*.tar.gz.sha256
   tar -xzf namedrop-*.tar.gz
   cd namedrop-*-aarch64-apple-darwin
   ```

   `shasum` must print `OK`.

2. If `command -v rex` found nothing in step 2, set `REX_BIN_DIR` to the directory of the `rex` that the app's server runs:

   ```sh
   ps -o command= -p "$(pgrep -U "$(id -u)" -f 'rex server run' | head -1)"
   export REX_BIN_DIR="<the directory of the rex in that command>"
   ```

3. Run `sh scripts/install.sh`. It prints the state directory, the launcher and the Rex server it will use, lists what it will write, and asks `Install Namedrop for Rex? [y/N]`. Answer `y`.
4. Write the report's `Rex server:`, `State directory:` and `Config:` lines and its `Keys:` lines in the table.

## 4. Run the actions from the palette and by key

Each step names a palette action by its title. Open the app's command palette and type the title. Write in the table which key or menu item opened the palette.

1. Open the palette and type `Namedrop`. Count the Namedrop actions it lists, and write their titles. The module defines eleven: Activate Namedrop, Name this pane, Manage Namedrop names and agents, Namedrop Doctor, Agent status, Namedrop tour, Open Namedrop Channels, Pause Namedrop, Resume Namedrop, Deactivate Namedrop, and About Namedrop.
2. Run **About Namedrop** from the palette. Write exactly what the app shows: a result, an error, or nothing.
3. Run **Activate Namedrop** from the palette. Write whether exactly one floating layer opened and what its first line says. Press Esc and write whether the layer closed. Activate Namedrop has no default key.
4. Run **Name this pane** from the palette. Write whether the name screen opened. Type `u14-palette`, press Enter, answer `terminal` for the pane type, and press Enter. Write the pane's label afterwards.
5. Split the window to get a second pane. In it, press **Option-Shift-Command-I** (`alt+shift+cmd+i`). Write whether the name screen opened. If it did, name the pane `u14-key` as in step 4.
6. Run **Namedrop Doctor** from the palette, or press Option-Shift-Command-H. Find the check "The Rex module is confirmed." or "The Rex module is not confirmed." Write its whole text in the table, and copy the `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR` and `REX_SERVER` values from its "Server environment:" part. A variable that the sentence leaves out is unset in the server. If the screen cuts the text off, run `~/.local/bin/namedrop doctor --json` in a Rex pane, and copy the `diagnostic.env` of the issue `ND-REX-001`.

## 5. Clean up

1. In a Rex pane of the disposable account, run `~/.local/bin/namedrop install --remove --input-json '{"confirmed":true}'`. Write its last line in the table.
2. Quit the Rex app, and log out of the disposable account.
3. In your usual account, run `rm -rf /Users/Shared/nd-u14`. Delete the disposable account when you no longer need it.

## Evidence

Date: ______ Commit under test: ______ Archive name: ______

| What | Result |
| --- | --- |
| macOS version | |
| Rex app version (Rex, About Rex) | |
| `command -v rex` and `rex version` in the app's pane | |
| `REX_BIN_DIR` set for the install (yes, and its value, or no) | |
| `shasum -c` result | |
| install.sh `Rex server:` line | |
| install.sh `Config:` line | |
| install.sh `State directory:` line | |
| install.sh `Keys:` lines | |
| Key or menu item that opened the palette | |
| Number of Namedrop actions the palette lists | |
| Their titles, as the palette shows them | |
| About Namedrop from the palette: what the app shows | |
| Activate Namedrop from the palette: layers opened, and the first line | |
| Esc closed the activation layer (yes or no) | |
| Name this pane from the palette: the name screen opened (yes or no) | |
| Label of the first pane afterwards | |
| Option-Shift-Command-I: the name screen opened (yes or no) | |
| Label of the second pane afterwards | |
| Doctor's Rex module check, whole text | |
| Server `HOME` | |
| Server `XDG_CONFIG_HOME` | |
| Server `XDG_STATE_HOME` | |
| Server `XDG_RUNTIME_DIR` | |
| Server `REX_SERVER` | |
| `install --remove` result | |
| Anything else that looked wrong | |

Send the filled table to the maintainer, who records it in PLAN.md's step 6 and PORT.md's step 6 record.
