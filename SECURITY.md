# Security policy

## Report a vulnerability

Don't open a public issue for a security problem. Report it privately on the repository's **Security** tab with **Report a vulnerability**. GitHub shares that report only with the maintainers.

Include the Namedrop commit, your OS and architecture, the output of `herdr --version`, and the steps that reproduce the problem. Remove credentials and private conversation text before you send it.

You get a reply within seven days. When a fix lands, the advisory credits you unless you ask to stay anonymous.

## Supported versions

Namedrop fixes security problems on `main`. Update to the latest `main` before you report, and check that the problem still happens.

## What counts

Namedrop passes text between agents that run with your permissions, so these are in scope:

- Text from another agent, or from a pane, that grants permissions or runs commands the recipient didn't approve.
- Shared context that a session other than its recipient can read.
- Credential files or recognized secrets that reach shared context.
- A delivery that reaches a pane outside its workspace, or that runs twice.
- A way for the Agent status summary request to escape its sandbox.
- Activation that changes files outside the ones listed in [what installation, activation and packages each cover](docs/operations.md#what-installation-activation-and-packages-each-cover).

Out of scope are problems in Herdr, Codex, or Claude Code themselves, and anything that needs an attacker who already controls your user account. Report those to the project that owns them.
