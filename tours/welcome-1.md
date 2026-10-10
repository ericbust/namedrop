id: welcome-1
title: Welcome to Namedrop
---
# Welcome to Namedrop

Namedrop lets agents work together across panes. Give an agent a
name, mention it from another pane, and the reply comes back where
you asked.

Names belong to this Herdr workspace. Use lowercase letters or
digits, separated by single hyphens: @reviewer, @test-runner, @build.

You can also ask an agent in its own pane:

  Name yourself @reviewer with Namedrop.

Press N to name this pane now.
action: name
---
# Mention an agent

Put a name inside a prompt and your agent delegates that part while
it keeps working toward your goal:

  Ask @reviewer to check the retry logic.

Start the prompt with the name to hand off the whole prompt instead:

  @reviewer Check the retry logic and report any bugs.

Either way, the reply returns to the pane that asked. Ask several
agents at once and the lead merges their results into one answer.
---
# Terminals, groups and status

In a shell pane, name it as a terminal:

  namedrop name @build --type terminal

Then send this from an agent pane:

  @build cargo test

It runs without an LLM and returns stdout, stderr and the exit status.

Save a group once, then mention it like a name:

  Create the Namedrop group @review-team with @reviewer and @tester.

Press Prefix, then Shift-S to see what each agent is doing.
---
# When something is off

Press Prefix, then Shift-H to open Namedrop Doctor. It explains
setup problems and blocked deliveries. In a shell, run:

  namedrop doctor

Press Prefix, then Shift-M to manage names: rename, attach or
detach a name, turn delivery on or off, or remove it.

Press Prefix, then Shift-O to open this tour again.
