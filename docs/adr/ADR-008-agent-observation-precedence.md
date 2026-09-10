# ADR-008: Agent observation precedence and generation

Status: accepted
Date: 2026-09-09

## Context
A comparable multiplexer separates "which agent" (process identity) from "what
state" (screen scraping or a first-party hook) and that split is the best decision in
its detection subsystem. What it lacks is a generation concept: staleness is guarded
only for hook-reported metadata, by a per-source sequence number, and the screen
detection state machine relies on being incidentally single threaded.

## Decision
Precedence, strongest first:
1. native structured agent protocol
2. official hook or plugin event
3. our own shell or OSC integration
4. process identity and foreground process group
5. terminal screen heuristics

Every observation is:
```rust
struct AgentObservation {
    agent_id: AgentId,
    generation_id: GenerationId,
    state: AgentRuntimeState,
    source: ObservationSource,
    confidence: Confidence,
    sequence: Option<u64>,
}
```
There is no API that publishes a state without a generation. Acceptance is ordered by
`(agent_id, generation_id, source, sequence)`. An observation from an older generation is
dropped, never applied.

A new generation is minted when the terminal's foreground process is replaced. On that
transition, OSC and screen evidence tied to the previous process is cleared and state is
forced to unknown for a bounded startup grace window.

Exception that is kept: strong visible screen evidence of a blocking modal may override
a weaker hook report that still claims working. A hook can lag or under-report a modal;
the live screen cannot.

### Generation provenance is captured when work starts, not when it finishes

Carrying a generation field does not prove an observation belongs to that generation. A
screen scan that started under generation 3 and finished after a restart would otherwise
be stamped generation 4 and accepted.

Rule: an observation records the generation that was current **when its input was
sampled**. A scan captures `(generation, content_sequence)` before it reads the screen and
carries both to the end. On completion, the result is rejected if the terminal's
generation has moved on. The same applies to a hook report, which carries the generation
that was current when the hook fired.

### The first generation, and generations across restart

The first generation is minted when the terminal is created, not when an agent is first
detected. Generations are a monotonically increasing per-terminal counter persisted with
the terminal, so a daemon restart cannot reuse a number. A restart mints a new generation
for every terminal it reconstructs, because it cannot prove the process is the same one.

### Reconciliation rules

The acceptance tuple alone is not a policy. Explicitly:

- Sequence is per source. A source without sequence numbers gets an arrival counter
  assigned by the daemon, scoped per `(terminal, source, generation)` and reset on a new
  generation. A counter that survived a generation bump would let a late arrival from
  generation 3 outrank an early one from generation 4 within the same source, which is the
  class this ADR exists to prevent.
- Across sources within one generation, strength decides, with one exception below.
- Freshness bounds strength: an observation older than its source's freshness window is
  demoted, so a stale high-strength report cannot outrank a current lower-strength one.
- The screen-over-hook exception applies only when the screen observation is **newer**
  than the hook observation and carries `visible_blocker`. An old modal screenshot can
  never override a newer working report.
- When two current sources of comparable strength disagree, the state is
  `Uncertain { candidates }` rather than an arbitrary winner. Uncertain is a real state
  that the UI shows honestly and that never triggers an attention notification on its
  own.

### When process identity is structurally unavailable

An agent run inside a container, a sandbox or another namespace is not visible to us: the
foreground process on our PTY is the wrapper, the agent runs in a pid namespace we cannot
read, and an exec notification fires for the wrapper and never for the agent.

In that case process identity reports `Unavailable`, not the wrapper's identity. Confidence
is capped accordingly, and only a hook or an OSC source can raise an attention item for
that terminal. Reporting "the agent is docker" with normal confidence would be worse than
reporting nothing, because it is a confident wrong answer that the reconciliation rules
would then defend.

## Consequences
- The reported failure where a terminal reads as ready while a trust prompt is on screen, and the stale
  generation class are both structurally prevented rather than patched.
- Detection manifests remain a maintenance cost. That cost is budgeted, not wished away.

## Rollback
None needed. Removing the generation field would be a regression, not a rollback.
