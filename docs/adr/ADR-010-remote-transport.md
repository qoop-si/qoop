# ADR-010: Remote transport

Status: proposed
Date: 2026-09-09

## Context
Reimplementing SSH authentication loses against the long tail of real `~/.ssh/config`
setups: jump hosts, per-host identity files, hardware tokens, agent forwarding. A comparable multiplexer
shells out to the system `ssh` and layers a managed config that
includes the user's own config first, which is the right call.

## Decision
Shell out to the system `ssh`, behind a `RemoteTransport` trait so ssh, a direct
connection and a future control-plane relay never leak into domain logic. Include the
user's config first, guarantee keepalives without overriding an explicit user setting,
and reuse one authenticated connection rather than spawning a new `ssh` per local
connection.

Version skew is gated on the protocol codec generation, not on the application version
string. A compatible remote daemon and its running agents are never restarted by a
routine client update.

## Consequences
Text matching on `ssh` stderr is a best-effort hint layer only and is never load-bearing.

## Rollback
Swapping in a library-based transport is a trait implementation change.
