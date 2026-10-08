# ADR-012: Local socket security and daemon singleton election

Status: accepted
Date: 2026-09-09

## Context
The daemon executes arbitrary commands with the user's full privileges. Anything that can
speak to its socket can therefore run code as that user. The plan named a socket path and
nothing else. That is not a security contract.

Separately, the plan's startup sequence used an identity-checked stale-socket removal plus
a readiness pipe. A review pointed out this does not serialise two simultaneous cold
starts: a second launcher can see a bound-but-not-yet-listening socket, classify it as
stale, unlink it and bind its own, leaving two daemons and one unreachable workspace.

## Decision

### Socket location and permissions
- Path: `$XDG_RUNTIME_DIR/qoop/qoopd.sock` when `XDG_RUNTIME_DIR` is set, owned by the
  user with mode `0700` on the directory.
- Fallback when it is not set: `$TMPDIR/qoop-<uid>/qoopd.sock`, where the directory is
  created with mode `0700` and then **verified after creation**: it must be a real
  directory, not a symlink, owned by our uid, with no group or other permission bits. If
  verification fails the daemon refuses to start. It never chmods someone else's
  directory into compliance.
- The socket itself is chmodded `0600` right after `bind`. In between it has default
  permissions, and what protects it then is the verified `0700` directory around it. A
  umask around `bind` would change it for every thread of the process at once.

### Test and debugging override

`QOOP_RUNTIME_DIR` overrides the runtime directory, and every check above applies to the
overridden path exactly as it does to the default one. This is not the optional session
namespace ADR-006 rejects: it exists so that integration tests, which run in parallel, each
get their own daemon and socket, and so a contributor running the suite does not attach to
their own working daemon. With it set, the database and the history stay in that directory
too rather than in the data directory (ADR-005), so a test never writes into a home.

### Authentication
Filesystem permissions are the first gate, not the only one. Every connection is
authenticated by peer credentials before the handshake is read: `SO_PEERCRED` on Linux,
`LOCAL_PEERCRED` or `getpeereid` on macOS. A connection whose peer uid differs from the
daemon's uid is closed immediately with no protocol response at all.

### What this does and does not protect

Stated plainly, because the plan previously implied more than it delivered: **every
process running as the same user has the same authority as the human at the keyboard.**
An agent that we launched can connect to the daemon and drive any workspace. Capability
checks in the collaboration layer are a cooperative product feature that prevents
mistakes and makes actions auditable. They are not a security boundary.

Real isolation between agents requires a different uid, a container or a VM, which is the
`ExecutionBackend` axis and is deliberately out of scope for v1. Documentation and
marketing must not claim otherwise.

### Singleton election
Startup takes an exclusive `flock` on `<runtime dir>/qoopd.lock` and holds it for the
entire sequence: stale socket check, unlink, bind, listen, initialise, signal readiness.
A second launcher blocks on the lock, and when it acquires it, finds a live socket and
simply connects. There is no window in which two daemons can both bind.

Readiness is still signalled over an inherited pipe rather than by polling the socket
path, so a client learns the daemon is ready with zero polling and no race.

Crash of a launcher mid-sequence releases the `flock` automatically because it is tied to
the open file description, so there is no stale lock to clean up.

## Consequences

- The daemon refuses to start rather than repairing an unsafe runtime directory. That is a
  visible failure on a misconfigured machine, which is the correct trade against silently
  running with a world-writable socket.
- Peer-credential checking adds one syscall per connection and closes the class of attack
  that filesystem permissions alone would miss on a shared or misconfigured system.
- The `flock` serialises cold start, so the worst case for a large simultaneous fan-in is a
  short wait, not two daemons.
- Same-uid processes remain fully authorised. Everything above raises the bar against other
  users and against misconfiguration; it does nothing against a hostile process running as
  this user, and the documentation says so in those words.

## Verification
- A test that starts 32 clients simultaneously on a cold machine and asserts exactly one
  daemon process exists and all 32 attach to it.
- A test that creates the fallback directory as a symlink and asserts the daemon refuses
  to start.
- A test that connects from a different uid, where CI permits, and asserts the connection
  is closed before any protocol bytes are exchanged.
- A test that asserts the socket is never accessible to group or other at any point,
  checked by racing a stat loop against startup.

## Rollback
None.
