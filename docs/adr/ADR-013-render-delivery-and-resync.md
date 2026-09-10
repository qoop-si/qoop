# ADR-013: Render frame delivery, dropping and resync

Status: accepted
Date: 2026-09-09

## Context
The plan used a per-client render lane: capacity one,
latest wins, drop on full. A review correctly identified that this is only safe if frames
are *cumulative*. If a frame carries an incremental damage delta and a frame is dropped,
the client's screen is permanently wrong: a dropped frame containing an erase or a mode
change is never re-sent, and after output stops the client sits on a corrupted display
with no event to correct it.

A comparable multiplexer avoids the corruption because it recomputes a diff against
each client's own baseline on every send and resets the baseline to force a full repaint
when it is unknown. That is a real mechanism, not an accident, and the plan omitted it.

## Decision

A render frame is defined relative to a base revision that the daemon knows the client
has, not relative to the previous frame the daemon sent.

```text
Frame {
  terminal_id,
  base_revision,     // the revision this frame updates from
  revision,          // the revision the client is at after applying it
  body: Delta | Full // Full carries a complete snapshot
}
```

Rules:
1. The daemon tracks `acked_revision` per client per terminal. Acknowledgement rides on
   any frame a client already sends, so it is free for an interactive client, but it is
   **not** dependent on the client sending anything: a client that has received frames and
   sent nothing must emit a standalone ack within 2 seconds or after 16 frames, whichever
   comes first.

   This is not a detail. A read-only observer, which is exactly the mobile and
   second-screen case, produces no input at all. Under the earlier rule its
   `acked_revision` never advanced, so it pinned the terminal's retained damage history at
   its bound forever and received a full snapshot for every single update, which is the
   opposite of the intended behaviour on the slowest link in the system.
2. When the render lane is full, the pending frame is **replaced**, not queued. The
   replacement is recomputed from `acked_revision`, so it is cumulative by construction.
   Dropping is therefore always safe: what gets dropped is a stale description of a
   transition that the replacement also describes.

   Replacement takes effect only at a **logical message boundary**. A frame body larger
   than the protocol frame cap is chunked (ADR-004), and a replacement partway through a
   chunk sequence would splice two bodies into one that is neither, with no message id for
   the receiver to detect it. A sequence that has started is finished first, and the
   replacement applies to what would have been sent next.
3. If recomputing a delta from `acked_revision` would be larger than a full snapshot, or
   if `acked_revision` is older than the retained damage history, the frame body is
   `Full`. A client can never be left describing a state it cannot reach.
4. On attach, on resize and after any protocol error, the daemon sends `Full` and resets
   `acked_revision`.
5. Terminal quiescence is not a hazard: because replacement is cumulative rather than
   incremental, the last frame delivered before output stops is always sufficient to
   bring the client fully current.

The client is never the source of truth. It may request a full resync at any time, and a
resync is idempotent.

## Consequences
- The daemon retains damage history per terminal back to the oldest `acked_revision`
  among attached clients, bounded at 256 KiB per terminal. A client that has not acked
  within that window is dropped from the retention calculation, so one stalled client
  cannot make every other client pay: the stalled one starts receiving `Full` frames while
  the current ones keep receiving deltas.
- A client that fails to ack for 30 seconds is disconnected, because at that point it is
  not a slow client, it is a wedged one.
- Three bounds interact and the binding one is stated so nobody has to derive it: the ack
  deadline (2 seconds or 16 frames), the retention budget (256 KiB per terminal) and the
  disconnect deadline (30 seconds). At 1 MiB per second of output, 256 KiB of damage is a
  fraction of a second, so **retention is the binding bound for a slow link**: a merely
  slow client crosses it long before the ack deadline and receives `Full` frames from then
  on. That is the intended behaviour on the mobile case, and it is why the retention number
  and not the ack deadline is the one to tune if mobile delta delivery matters later.
- A "1 frame" lane is one *logical* frame, which may be several protocol frames and may be
  megabytes for a `Full` body. The lane's memory cost is therefore the snapshot size, not
  the protocol frame cap.
- Dropping is genuinely lossless from the client's point of view, which is what makes a
  cheap capacity-one lane safe.

## Verification
- Saturate the render lane, then stop output entirely, then assert the client's rendered
  screen is byte identical to the daemon's snapshot. This is the exact case the review
  said would corrupt.
- Property test: apply a random sequence of writes and random drops, assert the client
  converges to the same grid as the daemon.
- Assert a stalled client's retained history is bounded and that the daemon switches to
  `Full` at the bound.

## Rollback
None. Incremental-relative-to-last-sent framing is the thing being rejected.
