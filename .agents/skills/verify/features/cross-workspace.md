# Cross-workspace mentions

A person in one Herdr workspace mentions an agent that lives in another. A `#label` scope before the leading mentions names the workspace, and `#all` names every activated workspace. An unqualified mention stays local when the name exists here, crosses when exactly one other workspace has it, and asks the person when two or more do. A forwarded envelope's sender carries its workspace, and a reply to a human relay returns to the pane the person typed in.

## Sub-features

- `xws-label-scope` sends `#beta-space @lead hi` from alpha to the workspace Herdr labels `Beta Space`, and to no other pane.
- `xws-sender-workspace` gives the forwarded envelope a sender with `workspace` `alpha`. A local envelope keeps a sender without `workspace`.
- `xws-relay-reply` returns the recipient's bound reply to alpha's lead as `report_to_human`, and sets the `namedrop-relay` token on alpha's lead pane only.
- `xws-local-first` keeps an unqualified `@lead` from alpha in alpha.
- `xws-single-remote` sends an unqualified `@reviewer` to the one other workspace that has it.
- `xws-all` delivers `#all @lead status` to every workspace's lead.
- `xws-repo-scope` sends `#stack @builder` to the one linked worktree of the `stack` repository that has the name, though the workspace labelled `stack` is that repository's inactive main checkout.
- `xws-repo-picker` answers `#stack @twin`, a name in two `stack` worktrees, with a picker that names the repository and offers `#wt-one` and `#wt-two`, and persists nothing.
- `xws-recipient-picker` answers `#stack run the tests` and `#wt-two run the tests`, which name a scope and no one in it, with a picker of that scope's `#label @name` tokens, even when there is only one, and persists nothing.
- `xws-picker` answers an unqualified name found in two other workspaces with roster-marked `additionalContext` that lists them, and persists nothing.
- `xws-wrapper` passes the block and the picker through the installed generated `user-prompt-submit` wrappers.

## How to get to it (user POV)

- Open two or more Herdr workspaces and activate Namedrop in each.
- Name agents in each workspace. The same name can exist in several workspaces.
- At an agent's prompt, type `#<label> @<name> <message>`. The label is the Herdr workspace label folded to lowercase, with each run of other characters replaced by one `-`, so `#beta-space` names `Beta Space`.
- Type `@<name> <message>` without a scope. When only other workspaces have that name, the agent either sends it there or asks which workspace you meant.

## Driving it with control-namedrop

Preconditions: the same as [stuck wake](stuck-wake.md). The helper links the candidate as a disposable plugin and runs every agent as a simulated Codex session.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature cross-workspace --binary <candidate>`.
- Setup. The helper labels its first workspace `alpha`, then creates `Beta Space` and `gamma`, each on its own Git directory. It also creates a `stack` repository, opens a workspace labelled `stack` on its main checkout, and opens the linked worktrees `wt-one` and `wt-two` through `herdr worktree open`. It activates Namedrop in alpha, Beta Space, gamma, wt-one and wt-two through `scripts/setup.sh activate --json`, with that workspace's pane context, and leaves `stack` inactive. Eight simulated sessions are named through the CLI: `lead` and `helper` in alpha, `lead` and `reviewer` in Beta Space, `lead` in gamma, `builder` and `twin` in wt-one, and `twin` in wt-two. Each session registers through one working-then-idle flip.
- Each prompt goes to the stable launcher as `hook user-prompt-submit --state-directory <state>`. The helper writes a Codex `UserPromptSubmit` JSON on stdin and sets the pane's `HERDR_PANE_ID`, `HERDR_WORKSPACE_ID` and `CODEX_SESSION_ID`. A direct hook call never drains, so the helper then runs `hook mailbox-wake`, the command the wrapper runs after a block.
- g. `herdr workspace list` must report the labels `alpha`, `Beta Space`, `gamma`, `stack`, `wt-one` and `wt-two`, and `worktree.repo_name` `stack` on both linked worktrees. After step a, `bridged_messages` must hold one row whose workspace is Beta Space's and whose origin is alpha's.
- a. From alpha's lead, `#beta-space @lead hi` must print exactly `{"decision":"block","reason":"Collaboration message sent."}`. Only Beta Space's lead receives an envelope. Its `control.sender.workspace` must be `alpha`, Beta Space's lead's `delivery inbox` must hold it, and alpha's lead's inbox must be empty.
- b. The bound `reply` prefills `@lead <reply body>`. The helper replaces the placeholder and runs the operation with the bound argv, input and environment. The receipt's `relay` must show `notification_shown` with no `metadata_error`. Alpha's lead, and no one else, receives `Beta lead here.` with `reply_mode` `report_to_human`. `herdr pane get` must show the `namedrop-relay` token `@lead: Beta lead here.` on alpha's lead pane and no such token on Beta Space's lead pane.
- h, send. From alpha's lead, the installed Codex wrapper runs `#beta-space @lead hi from the wrapper` and must print the same block. No manual wake follows. The wrapper's background mailbox wake must deliver the envelope to Beta Space's lead.
- c. From alpha's helper, `@lead local check` reaches only alpha's lead, with a sender that has no `workspace`.
- d. From alpha's helper, `@reviewer please review` reaches only Beta Space's reviewer, with sender workspace `alpha`.
- f. From alpha's helper, `#all @lead status` reaches the leads of alpha, Beta Space and gamma, and no one else.
- i. From alpha's helper, `#stack @builder build it` must print the block, and only wt-one's builder receives it, with sender workspace `alpha`.
- j. From alpha's helper, `#stack @twin which one?` must return roster-marked `additionalContext` that contains `@twin is named in more than one stack worktree: #wt-one, #wt-two.` and `Nothing was sent`. After a mailbox wake, no pane receives an envelope, and the message row counts do not change.
- k. From alpha's helper, `#stack run the tests` must return roster-marked `additionalContext` that contains `It names #stack but no agent there. Its agents: #wt-one @builder, #wt-one @twin, #wt-two @twin. Nothing was sent`. `#wt-two run the tests` must ask the same way with `#wt-two @twin` alone. After each, a mailbox wake delivers nothing and the message row counts do not change.
- e. The helper removes alpha's `lead` name with `address remove`. From alpha's helper, `@lead who should take this?` must return `additionalContext` that starts with `Namedrop workspace roster.` and contains `@lead is not named in this workspace. It is named in these workspaces: #beta-space, #gamma.` and `Nothing was sent`. After a mailbox wake, no pane receives an envelope. The row counts of `collaboration_messages`, `mailbox_deliveries`, `delivery_attempts`, `bridged_messages` and `space_envoys` must not change.
- h, picker. The installed Codex and Claude wrappers each run the same prompt. Each must print byte-for-byte the hook's output from step e, and the row counts must still not change.
- Every delivery check waits for the expected envelope counts, waits 3 s more, and then requires every simulated pane's count to match. A stray envelope to any pane fails the step. The helper completes each received envelope through its bound `complete.no-reply`.
- Every assertion after setup is a `require` that names its step and the observed value. `summary.json` records `outcome` as `passed` or as the failing requirement. `cross-workspace.json` keeps the panes, workspaces, sessions, registrations, the live `workspace list`, each hook's stdin prompt, exit status, stdout and stderr, the reply receipt, the relay tokens, and every requirement with its observed value. It is saved on every stop. `<role>-envelopes.json` keeps every envelope each simulator received. `codex-user-prompt-submit.sh` and `claude-user-prompt-submit.sh` keep the installed wrappers. `state.sqlite3` is backed up after any stop.

## Limits

- The hook runs as a host process. Herdr vouches for it because the pane's `CODEX_SESSION_ID` runs in that pane. It is not a child of a real agent process, and no Codex or Claude host calls it.
- Herdr cannot list notifications, so the `Sent to #beta-space @lead` banner is not observed. The relay is observed through the reply receipt and the source pane's `namedrop-relay` token.
- No person answers the picker, and no model reads the roster or runs `send` with the chosen `#label`.
- The simulated agents never read their envelopes. The helper runs each bound operation for them.
