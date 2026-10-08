# ADR-021: Windows

Status: accepted
Date: 2026-09-10
Evidence: `crates/qoop-pty/src/sys.rs`, `crates/qoop-pty/src/procwatch.rs`
Reproduce: `./scripts/platform-check.sh`

## Context

The plan says no Windows claim may be made until this ADR exists with its own spike. This
ADR exists so that the claim can be made accurately, and the accurate claim is that Windows
is not supported.

Writing it down matters because the alternative is a README that says "cross platform",
which is true of the language and not of the product, and a first Windows user who
discovers that the thing the product is for does not work.

## Decision

### Windows is not supported in version one, and nothing claims otherwise

The unsupported platform is named in the README, in the help output and here. There is no
`#[cfg(windows)]` that compiles into something that does not work, because a build that
succeeds and then fails at runtime is worse than one that does not build.

### What the port actually is

Not a port. The parts of this product that are the product are the parts that have no
Windows equivalent:

**The pty layer.** `posix_openpt`, `grantpt`, `unlockpt`, `TIOCSCTTY`, process groups,
controlling terminals and `SIGHUP` do not exist. ConPTY is the analogue and it is a different
model: there are no process groups, so ADR-014's containment identity has nothing to be. A
Windows containment set would be a job object, which is a better mechanism and an entirely
separate implementation.

**Teardown.** The escalation is `SIGHUP`, `SIGTERM`, `SIGKILL`, and the containment set is
everything on the controlling terminal. Windows has none of those four concepts. A job object
with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` does the same job by a completely different route,
and it is arguably more reliable, which is the point: it is not the same code with different
constants.

**Process observation.** `kqueue` `EVFILT_PROC` on macOS and `/proc` on Linux both have
Windows analogues, and they are a third implementation rather than a variation of either.

**The socket.** A unix domain socket with peer uid checking is ADR-012's whole trust
boundary. Windows has `AF_UNIX` now, and it does not have `SO_PEERCRED`, so the boundary
would be a named pipe with an impersonation check, which is a different security argument
that would need its own review.

### What it would take

A `qoop-pty-windows` crate implementing the same seams, a fourth platform arm in the process
watcher, a named pipe transport with its own trust ADR, and a Windows arm for every gate that
currently asserts against `ps`, `lsof`, `/proc` or `proc_pidinfo`. The gates are the larger
half: they are what make the Unix implementation trustworthy, and a Windows implementation
without them would be a Windows implementation nobody can vouch for.

The realistic first step is not the port. It is running under WSL, where every mechanism
above is the Linux one and already works, and saying so plainly rather than implying a native
port that does not exist.

## Consequences

- The product says it runs on macOS and Linux, and under WSL on Windows.
- No half-working Windows build exists to be reported as broken.
- The seams that would carry a port are already there, because they were needed for macOS
  and Linux to differ, and a future port implements traits rather than adding branches.

## Rollback

There is nothing to roll back. If Windows is done later it is an addition, and this ADR
becomes the record of what it had to include.
