# ADR-018: What a plugin is allowed to be

Status: accepted
Date: 2026-09-10
Evidence: `crates/qoop-plugin/src/lib.rs` and its tests
Reproduce: `./scripts/plugin-check.sh`

## Context

Milestone 11 is blocked on this because a review surface with an ecosystem is a review
surface that runs somebody else's code, and every terminal multiplexer that grew a plugin
system grew it the same way: a scripting language in the same process, full access to
everything, and a decade of "do not install untrusted plugins" as the security model.

That model has one property worth keeping, which is that plugins are easy to write, and one
that is disqualifying here. This product holds a person's agent sessions, their working
trees, and a socket that can start processes as them. A plugin in the daemon's process is a
plugin that can do all of that, and a plugin that can do all of that is one that nobody can
sensibly install from a list.

## Decision

### A plugin is a process, not a library

It runs as a child of the daemon, over the same protocol a client uses, with a capability
set granted at load and never widened at runtime. There is no in-process plugin API, so
there is no version of "the plugin crashed" that takes a terminal with it, and no version of
"the plugin blocked" that stops a render.

The cost is real and is accepted: a process boundary per plugin, and a protocol call where a
function call would have done. What it buys is that every one of the failures above becomes
a process failure, which the daemon already knows how to survive.

### A plugin sees what it was granted, and the grant is per object

Capabilities are the shape `qoop-collab` already uses: granted to a pair, never held by a
participant alone. "This plugin may read that terminal" is the only form that means
something. A role, a scope or a tier is how something ends up including what nobody
intended, and the person granting it cannot tell what it includes without reading the
implementation.

The set is small and closed, and the absence of a write capability is deliberate in exactly
the way it is in `qoop-collab`: a plugin cannot type into a terminal, because there is no
method that takes a terminal and some bytes.

### A plugin never sees a secret, and never sees a token

Secrets resolve into the environment of the process that needs them (ADR-015), and a plugin
is not that process. The capability token of a terminal is injected by the daemon and
excluded from capture by name; the same exclusion applies here, because a plugin holding a
terminal's token is a plugin that can present itself as that terminal's child.

### A plugin that misbehaves is stopped, and the daemon says so

Three bounds, each with a counter: wall time per call, calls per interval, and total
outstanding calls. Exceeding one stops the plugin and records why. There is no warning
threshold, because a warning is a thing nobody reads, and no automatic restart, because a
plugin that fails its bounds once will fail them again and a restart loop is worse than a
stopped plugin.

## Consequences

- A plugin crash, hang or flood costs one process and one message, and no terminal.
- Writing a plugin is more work than writing a script, and it is work in any language that
  can speak the protocol rather than only in the one that was embedded.
- The daemon's attack surface for a plugin is the protocol, which is the surface it already
  has to defend against a client.
- A plugin cannot do the thing plugins are most often asked to do, which is type into a
  terminal. That is the point, and it is stated here so nobody has to discover it.

## Rollback

If the process boundary turns out to be too expensive for something real, the change is a
capability that a plugin can be granted to run in process, with the prompt that implies, not
a redesign: the protocol surface is the same either way.

## Amendment, 2026-11-15: herdr-compatible plugins

Two sentences above stop being the whole story: "a plugin cannot type into a terminal" and
"a capability set granted at load". They still describe the native model, the `qoop-plugin`
crate, which is unchanged and stays the model for a future qoop-native plugin API. A second
kind of plugin now exists beside it: a herdr TUI plugin, installed with `qoop plugin install`
and run unchanged (`docs/plugins.md`).

A herdr plugin is a program the person chose to install, and its manifest carries build steps
that run arbitrary code. So the boundary is not a capability set. It is consent pinned to
content and rechecked at every run:

- `qoop plugin install` and `link` print every build, action, pane, event and startup command,
  the commit and what a plugin can do, and record the person's yes in the consent ledger
  (`<data>/plugins/consent.json`, the ADR-015 pattern). The ledger pins the manifest text,
  compared byte for byte. It records the commit and does not verify it again, and it does not
  pin the files the commands run: the plugin runs as the person and could rewrite them anyway.
  The docs say exactly this.
- Every `qoop plugin run`, handler, startup, pane open and host start compares the manifest
  with the ledger. A mismatch or a missing ledger refuses (fail closed). An id is installed
  again only after `qoop plugin rm`.
- A plugin runs as the person and can read every terminal and the command lines of their
  processes. It can also type into terminals, but only through the refusals of `qoop send`
  and `qoop prompt`: a person at the terminal, a blocked agent and the trust-prompt and
  sleeping guards turn a plugin's write into an error for the plugin, with no retry and no
  bypass. This reverses the last consequence above for this kind of plugin, on purpose, and
  says so here so nobody has to discover it.
- A plugin never gets a terminal's token. A plugin pane starts through a one-time spec held in
  the host's memory, and `pane-exec` strips qoop's variables before the plugin starts, so an
  agent started inside a plugin pane has no hook authority. A plugin cannot report agent
  state: `pane.report_agent` is an unknown method.
- The plugin host is an observer of the daemon, never keeps it up, and has no token of its
  own. It is started lazily, when a plugin is installed and the interface starts or an
  install runs, and never restarted in a loop: it leaves with the daemon, and when no plugin
  is left. A hook command is stopped by process group after ten minutes, and at most 32 run
  at once.
- Nothing qoop runs for a plugin reads `HERDR_SOCKET_PATH` except the `herdr` shim, which only
  a plugin's own `PATH` finds, so no plugin of this kind can reach a herdr of the person's own.

## Amendment, 2026-11-16: the person grant and the own-pane pass

- Two exceptions loosen the refusal above. A person's key in a plugin's own pane lets that
  plugin write once into its tab within 10 s (never past a draft). Since a line a person already
  sent holds no write any more, only a draft does, this pass lifts nothing of the person's hold. And a plugin's write into a pane it opened, still running its program, is never
  held by a person there (a sidebar closing itself with `ctrl+q`).
- Both are a courtesy against a plugin's mistakes, not a boundary against a process of the same
  user. The host knows who asks, and what runs in a pane, from the starting environment of a
  process (`HERDR_PLUGIN_ID`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ENTRYPOINT_ID`), read only while
  its start time stays the same, never from what a request says. Any process of the same user can
  start a program with those variables set, and so could pass for a plugin. The guard keeps a
  well-behaved plugin from typing over a person; it does not keep out a hostile program that
  already runs as that person.
