# qoop

A portable runtime for human and coding-agent workspaces.

```
cd my-project
qoop
```

That reaches a real shell, with an index of your terminals beside it. No account, no wizard,
no manifest. Press `ctrl-q` and the shell keeps running; run `qoop` again and you are back in
the same one.

## What it is for

A terminal multiplexer is a good place to keep a coding agent, and a bad one to keep several.
The things that go wrong are the same every time: the agent finished twenty minutes ago and
nobody noticed, the session came back with the right command line and the wrong
conversation, a background job outlived the window that started it, and an update restarted
everything.

So the parts that usually get bolted on are the parts qoop is built out of:

- **Every terminal knows its agent.** The index says which agent is in which terminal and
  what it is doing: working, blocked on a question, done, failed or idle. An agent that
  needs you lights its row, rings the bell and sends a desktop notification.
- **Sessions come back as themselves.** Stop a session, open it again, and each tab returns
  to its directory and, for agents that report it, to the same conversation.
- **What scrolls off goes to disk.** Scrollback is kept as rows on disk, searchable from copy
  mode across the whole history.
- **Closing a terminal contains what it started.** Jobs that escaped the shell's process
  group are found by their terminal and ended with it.
- **Nothing polls.** Eight terminals open and nobody typing costs microseconds of CPU a
  minute; a screen is read only once it stops changing.

## Where it runs

macOS (Apple silicon and Intel) and Linux (x86_64 and arm64, glibc 2.17 or newer). On
Windows, under WSL.

## Next

- [Install](#install)
- [Quick start](#quick-start)
- [Supported agents](#agents)
