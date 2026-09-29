# ADR-019: Upgrading the daemon without killing what it holds

Status: accepted
Date: 2026-09-10
Evidence: `crates/qoro-runtime/src/daemon.rs` and `crates/qoro-remote/src/lib.rs`
Reproduce: `./scripts/replay-check.sh`

## Context

This was blocked behind the input ownership tests, and they exist now: gate G1.7 pins that a
non-owner cannot resize, G2.14 pins that a silent owner does not lose the geometry, and both
are asserted as absences after a fresh marker rather than as the presence of an unchanged
value. That matters here because the question an upgrade asks is exactly "who owns the
things this process is holding", and answering it before those tests existed would have been
answering it about behaviour nobody had checked.

The thing an upgrade must not do is kill a person's agents. A coding agent that has been
working for twenty minutes and is about to answer is not a process anybody accepts losing to
a routine update, and "restart your terminals after upgrading" is how a tool becomes
something people avoid upgrading.

## Decision

### The daemon does not hand over live terminals, and says so

There is no fd passing, no re-exec that inherits the pty masters, and no second daemon that
adopts the first one's children. Every one of those is buildable and every one of them turns
the upgrade path into the most complicated code in the product, exercised least often, on
the day somebody is already annoyed.

What happens instead is that an upgrade is refused while terminals are live, with what is
live named, and the person decides. That is a worse experience than a seamless handover and
a much better one than a handover that works nine times out of ten.

### The remote case is the one that matters, and it is already decided

ADR-010 gates version skew on the codec generation rather than on the version string, which
means a client update does not restart a remote daemon whose codec is compatible. That is
the case where a restart is most expensive, because the agents are on the far side of a
network and the person is not there to notice, and it is handled by not needing an upgrade
at all.

The application version is free to differ. A remote three releases behind, still speaking
the codec, keeps its terminals through every one of those releases.

### What survives a deliberate restart

Everything that is metadata: workspaces, terminals, layout, split trees, sleeping spaces,
the idempotency ledger, history, the confirmation ledger. The event log is not in that list
because the daemon no longer writes it; see ADR-005. What does not survive is what cannot: the pty, the engine state and the
child processes. A restart that reconstructs a terminal mints a new generation, because it
cannot prove the process is the same one, which is the rule `GenerationId` already carries.

A restart after an unclean shutdown used to append a gap event covering the interval
nobody watched. It no longer does, because the daemon writes no events; the ledger row a
command wrote before a restart is still there, which is the only continuity a reader of this
store gets.

### The upgrade is not the daemon's job

The daemon does not download anything, does not check for versions, and does not restart
itself. It answers `--hello` with its codec generation, refuses to stop while terminals are
live unless told to, and that is the whole of its participation. Anything that upgrades it
is a package manager, which is a thing that already exists and that people already trust
with this.

## Consequences

- No fd passing code, and no class of bug that only appears during an upgrade.
- A person with live terminals is told to close them, which is annoying and predictable.
- A remote daemon keeps its agents through client updates for as long as the codec holds,
  which is the case where a restart costs the most.
- The upgrade path is whatever installed the binary, and the daemon has no opinion about it.
- For the prebuilt macOS arm64 binary that path is `qoro update`: explicit, refused
  while a session runs, checksum verified, and installed over the file while every
  running process keeps serving from its old image. `qoro update --check` only leaves
  a notice the sidebar reads, so the interface advertises an update without ever
  touching the network.

## Rollback

If seamless handover ever becomes worth its cost, the shape is a second daemon that accepts
pty masters over the existing socket, and the refusal above becomes the fallback for when it
fails. Nothing here has to be undone for that: what is decided now is that the fallback ships
first.
