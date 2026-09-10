# ADR-018: What a plugin is allowed to be

Status: accepted
Date: 2026-09-10
Evidence: `crates/qoro-plugin/src/lib.rs` and its tests
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

Capabilities are the shape `qoro-collab` already uses: granted to a pair, never held by a
participant alone. "This plugin may read that terminal" is the only form that means
something. A role, a scope or a tier is how something ends up including what nobody
intended, and the person granting it cannot tell what it includes without reading the
implementation.

The set is small and closed, and the absence of a write capability is deliberate in exactly
the way it is in `qoro-collab`: a plugin cannot type into a terminal, because there is no
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
