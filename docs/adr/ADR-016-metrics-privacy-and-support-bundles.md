# ADR-016: Metrics, privacy, and what a support bundle contains

Status: accepted
Date: 2026-09-10
Amended: 2026-09-29, recorded that no bundle was ever producible from a running daemon.
Amended by: ADR-023 (2026-10-26), usage counts leave the machine by default.
Evidence: `crates/qoop-support/src/lib.rs` and its tests
Reproduce: `./scripts/bundle-check.sh`

## Who can build one

A bundle is a slice of the daemon's event log, and since 2026-09-29 the daemon writes no
events, so there is nothing for it to slice. `qoop-support` and its tests are here and
`scripts/bundle-check.sh` passes: its tests build a `Vec<Event>` by hand and hand it to
`Bundle::build`, so it is a proof about the format and about nothing else.

To be exact about what this change cost, it cost nothing. `qoop-support` had no callers
before it and has none now, and no `qoop support` command exists. A bundle could not be
produced from a running daemon before this change either. What the log's removal forecloses
is the first one ever being written.

## Context

Gate G4.7 asks for two things at once that pull in opposite directions: a support bundle
that reconstructs "command to state transition to event to notification to response", and a
support bundle that contains no secrets from a session that used them.

The tempting shape is a redaction pass over the bundle on the way out. ADR-015 already
rejects that for the record itself, and the reason applies here with more force: a filter
applied at output time is a filter that has to know every place a secret can be, and every
new field is a new place it does not know about yet. The first field somebody adds without
thinking about it is the leak.

The second tempting shape is a bundle of summaries, which cannot leak because it contains
almost nothing. It also cannot answer the question it was built for. When somebody reports
that their agent was shown as blocked and was not, the bundle has to say what was seen, and
"we concluded blocked" is precisely the half that is not in dispute.

## Decision

### A secret value never enters the record, so a bundle has nothing to filter

This is ADR-015's rule and this ADR only extends it to the event log and the bundle. A
`SecretRef` resolves as late as possible, directly into the environment block handed to a
child. It is never written to the log, never to a projection, and never to a bundle,
because at no point does anything upstream of those hold the value.

The consequence is the property worth having: a bundle is safe because of where secrets are
resolved, not because of what a redactor remembered to strip.

### Evidence is recorded redacted, and the redaction is recorded

An observation carries what it was derived from (ADR-017). A matched screen line may contain
something a secret rule matches, so it is redacted at the point of recording, and the fact
that a redaction happened is recorded with it. The bundle then shows a person that there was
evidence, that it was redacted, and which rule redacted it, which is enough to diagnose a
wrong conclusion without shipping the thing that was hidden.

A redaction that leaves no trace is indistinguishable from an absence of evidence, and the
two lead to opposite conclusions about a bug.

### The bundle is a log slice plus the projections it produces

Not a separate format. The bundle is:

- a contiguous slice of the log, by time range,
- the projections rebuilt from exactly that slice,
- the daemon's own version, platform and configuration,
- counters: what was dropped, what was degraded, what was refused.

Because a projection is a fold over the log with no other input, the two halves can be
checked against each other: rebuild the projections from the slice and compare. A bundle
whose projections do not match its own log is a bundle that has been edited, and that check
is cheap enough to run on every bundle rather than only when somebody is suspicious.

Terminal history is not in the bundle by default. It is the largest thing we hold and the
most likely to contain something private, and it is almost never what a bug needs. A person
may add a named terminal's history deliberately, and the bundle records that they did.

### Metrics are counters, never content

What is counted: terminals, connections, events by kind, drops by reason, degraded
intervals, refused claims, teardown outcomes, budget breaches. What is never counted:
anything derived from what a person typed or an agent produced, including lengths and
hashes. A hash of a command line is a fingerprint of a command line.

Counters are local. Nothing leaves the machine unless a person exports a bundle, and
exporting is an explicit act with an explicit destination. (Amended by ADR-023: a release
build sends a day's usage counts, numbers and fixed names only, unless turned off.)

### Privacy is a property of the default, not of a setting

There is no telemetry to turn off. A setting that defaults to on and can be turned off is a
decision made by whoever did not read the setting. (Amended by ADR-023, which answers this
argument: the usage counts are on by default, shown before anything is counted, printable
with `qoop telemetry show` and stopped by one command.)

## Consequences

- A bundle can be handed to somebody without a review pass, because there is nothing in it
  that a review pass would remove.
- A wrong conclusion is diagnosable: the evidence is there, redacted where it had to be, with
  the redaction visible.
- The bundle's integrity check is the same fold the daemon runs anyway, so it cannot rot
  separately from the projections it checks.
- Terminal history is opt in per terminal, which means some bugs need a second round trip.
  That is the trade, and it is made deliberately in favour of not shipping a person's screen.

## Rollback

If bundles turn out to need history by default, the change is one flag and a louder consent
step, not a format change: the bundle already names what it contains, so a bundle with
history is a bundle whose manifest says so.
