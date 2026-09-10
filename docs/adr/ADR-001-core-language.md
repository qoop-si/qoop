# ADR-001: Core language and edition

Status: accepted
Date: 2026-09-09

## Context
The runtime is a long-lived, concurrent process manager holding raw file descriptors,
child processes and untrusted byte streams. It must ship as a single native binary on
macOS, Linux and Windows.

## Options
1. Rust. Memory safety on a long-lived concurrent process manager, native performance,
   single binary, mature PTY and terminal ecosystem.
2. C or Zig. Lower baseline overhead in principle, no borrow checker friction, but every
   use-after-free in a PTY or client teardown path becomes a security bug in a process
   that runs arbitrary developer tooling.
3. Go. Fast to write, but a garbage collector on a latency-sensitive input path and a
   larger idle runtime footprint work against invariant I11.

## Measurements
None required. The problems this design avoids (fixed timers, global sizing, no durable event log) are architectural, not
language-caused. Rewriting in a lower-level language would fix none of them.

## Decision
Rust, edition 2024, current stable toolchain. MSRV is pinned once CI has data, not
guessed. Targeted `unsafe` or FFI is allowed only after a profile identifies a real hot
spot, and then only with a safety comment, a test and a benchmark.

## Consequences
Zig or C may still enter as a vendored library behind a seam, for example a terminal VT
engine. That is a dependency decision (ADR-003), not a core language change.

## Rollback
Replacing the core language is not a rollback, it is a rewrite. The reversible decision
is per component, which is why every external engine sits behind a trait.
