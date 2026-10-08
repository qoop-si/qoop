# ADR-004: Local protocol and framing

Status: accepted
Date: 2026-09-09

## Context
A comparable multiplexer runs two protocols on two sockets: a private bincode
protocol whose version must match exactly, and a stable "endpoint" protocol that
negotiates named codecs inside a frozen envelope and ignores unknown fields. The second pattern is the better one and the first is only
tolerable because client and server always ship together.

## Decision
One Unix domain socket. The complete first-generation grammar, specified before it is
frozen rather than after:

```text
frame  := len:u32le  header  payload
header := channel:u8  flags:u8  stream:u32le  seq:u32le
len    := byte count of header + payload, at most MAX_FRAME (65536)

channel 0  handshake      payload is always JSON, fixed for all time, never negotiated
channel 1  control        codec selected by negotiation
channel 2  event          codec selected by negotiation
channel 3  terminal       codec selected by negotiation, bidirectional. Daemon to client
                          carries the ADR-013 frame. Client to daemon carries input,
                          resize requests and standalone acknowledgements for the terminal
                          named by `stream`. An earlier version defined only the daemon to
                          client direction, which left client input with no channel at all
                          in a grammar that called itself complete
channel 4  blob           chunked bulk transfer, stream identifies the transfer
5..255     reserved, a receiver closes the connection on an unknown channel

flags bit 0  END      last chunk of a logical message
flags bit 1  ACK      header carries an acknowledged revision in the payload prefix
flags 2..7   reserved, must be zero, a receiver closes the connection otherwise
```

`stream` is what makes many terminals and a multiplexed remote client routable. A single
channel byte is not enough and the earlier draft was wrong to imply it was.

`stream` is defined for every channel, not only for the two that obviously use it:

| Channel | `stream` means |
|---|---|
| 0 handshake | always 0 |
| 1 control | a request id, so two chunked responses can be in flight at once |
| 2 event | a subscription id |
| 3 terminal | the terminal id |
| 4 blob | the transfer id |

`END` is set on a single-frame message. A receiver that treated a missing `END` as "keep
accumulating" and a sender that omitted it on a single frame would produce a protocol that
hangs on the first message, and both readings were consistent with the earlier text.

The `ACK` flag means the payload is prefixed by a `u64` little-endian acknowledged revision,
before the codec-encoded body, and `ACK` may ride only on the **first** frame of a chunk
sequence. A standalone acknowledgement, which ADR-013 requires from a client that sends
nothing of its own, is an `ACK`-flagged frame on channel 3 with the terminal's stream and an
empty body.

Stream ids are per connection, allocated monotonically, and never reused for the life of
that connection. That is what prevents a frame still in flight for a closed terminal from
being applied to whatever took its number. A frame for an unknown or already-closed stream
is dropped and counted, never treated as a protocol violation, because it is a normal
consequence of a close racing an in-flight frame.

`seq` is per stream, starts at 0, increments per frame on that stream, and wraps. It is
**assigned at the moment a frame is handed to the writer**, after any render-lane
replacement has already happened.

That allocation point is the whole content of the field. If `seq` were assigned when a
frame is queued, every lane replacement would consume a number that is never transmitted,
and since replacement is the normal case for any client slower than its terminal, the
receiver would see a gap on essentially every burst and request a full resync each time,
turning the cheap capacity-one lane into repeated full-snapshot transfers on the slowest
link in the system. Assigned at hand-off, a gap means transport corruption and nothing
else, which is the only thing a receiver can usefully act on.

It is not the acknowledgement mechanism: acknowledgement carries a revision, which is
ADR-013's concept, not a frame counter.

An earlier draft left `seq` frozen with no stated meaning. Four permanently reserved bytes
with no definition is worse than no field, so it now has one.

`MAX_FRAME` is 64 KiB, deliberately small. This is what prevents head-of-line blocking:
a large history transfer is many small `blob` frames, so control and terminal frames
interleave between them. A single socket with per-lane queues gives isolation only if no
single frame can monopolise the write for long, and this cap is that guarantee. It is
measured: focused input latency during a sustained bulk transfer is a benchmark scenario.

### One version axis, not two

`codec_generation` is the only negotiated version number. An earlier draft also carried
`proto`, which had nothing left to govern once the envelope was frozen for all time and
every payload became a named negotiated codec, and a field with no decision to make gets
moved eventually for a reason nobody can check. It is removed.

Adding a channel is a generation bump. A receiver closes on an unknown channel, so an old
receiver cannot tolerate a new one; the earlier claim that a second transport could be added
later "because the envelope carries a channel byte" was not reachable under the ADR's own
rules, and the rules win.

### Codec generation

The handshake carries `codec_generation`, a single integer that names the compatibility
floor for the whole negotiated codec set. It moves only when a codec's first generation
changes shape, never when a codec is added and never on an application release. This is
the value ADR-010 compares to decide whether a remote daemon must be restarted; without a
named field there, the path of least resistance is comparing version strings, which is the
behaviour ADR-010 exists to forbid.

### The outbound arithmetic, done rather than asserted

An earlier version of this table exempted a snapshot larger than the per-connection cap
"while it is being written", and bounded only "the number of such frames in flight, which is
one per terminal". Combined with the per-terminal render lane, one connection attached to a
hundred terminals could then hold a hundred concurrent multi-megabyte snapshots, and
ADR-003 measures a scrolled 200 column terminal at 5.7 MiB. A single client reattaching to a
busy workspace, which is an ordinary action rather than an attack, would demand more than
the entire global budget on one connection. The exemption is removed.

The real rule, with the numbers:

- A connection may have at most **one** oversized `Full` body in flight at a time. Other
  terminals' snapshots queue behind it and are recomputed from their own acknowledged
  revision when their turn comes, which costs nothing because the recomputation is
  cumulative by construction (ADR-013).
- The per-connection ceiling is therefore `8 MiB + one snapshot`, and a snapshot is bounded
  by the terminal's own memory ceiling from ADR-003, roughly 5.7 MiB at 200 columns.
- 64 accepted connections against a 64 MiB global budget means the global budget binds
  first for wedged readers, which is what gate G1.21 exercises with 48 of them.
- A single client attaching to many terminals is bounded by the one-snapshot rule rather
  than by the number of terminals, which is the case the earlier version got wrong.

### Bootstrap

Channel 0 is the answer to "what decodes the first frame before a codec is negotiated".
Its payload is JSON, forever, with no negotiation, because something has to be fixed for
negotiation to be expressible.

An earlier version of this ADR said CBOR. Writing the decoder changed the answer, which is
the kind of correction the plan predicted would only surface in code: this frame is sent
once per connection, so compactness buys nothing measurable, and a bootstrap that can never
change is worth being readable in a hexdump and decodable by every language a plugin or a
mobile client might be written in. It carries only:

```text
client hello:  { codec_generation_min, codec_generation_max, client_id,
                 codecs: {control: [...], event: [...], terminal: [...], blob: [...]},
                 client_name, client_version }
               unknown fields are ignored, which is what lets optional behaviour ship
               without a generation bump
server welcome:{ codec_generation, selected: {...}, capabilities: [...], server_version }
server refuse: { reason, server_codec_generation_min, server_codec_generation_max,
                 unsatisfied_category, human_message }
```

Failure behaviour is explicit rather than implied:
- No overlap in any one of the four codec categories: the server sends `refuse` naming
  which category failed and both supported ranges, then closes. It never falls back to a
  default codec, and it never proceeds with a partially negotiated set.
- Every mutating control message carries an idempotency key, and the client presents a
  **stable client identity** in its hello which it re-presents on reconnect.

  That identity is self-asserted and authenticates nothing. It is a dedupe key, not a
  credential, and it is namespaced by the peer uid, which the socket layer does
  authenticate (ADR-012). Two same-uid processes can therefore collide on it, and the worst
  they can do is suppress each other's retries or observe that a key was used, which is the
  same authority they already have over each other. It is deliberately **not** the key that
  ADR-007 uses for resize ownership: ownership is per connection, because it must end when
  the connection does, and a stable identity would survive a crash and strand the geometry.

  Amended 2026-09-30: what is built is narrower. The ledger is keyed by the request id
  alone, not by that identity, and lives in `qoop.db`'s `applied` table with the outcome,
  kept for 30 days (ADR-005). A create writes its row in the same transaction as the
  terminal, so a key already answered cannot open a second tab.

  Scoping this to the connection, which an earlier draft did, defeats the case it was
  written for: a client that has crashed reconnects on a new connection with an empty key
  table, and a daemon that has crashed loses an in-memory table entirely. The stated
  scenario is a mobile double-tap approving a destructive action, which is reachable
  exactly when the network was bad enough to make the user tap twice.
- A first frame on any channel other than 0: connection closed, no response.
- A first frame larger than `MAX_FRAME` or with unknown flags: closed, no response.
- Handshake not completed within 5 seconds: closed. This is what stops a connection from
  reserving buffers indefinitely while withholding its body.

### Global resource budgets

Per-connection bounds do not bound the daemon. All of these are enforced:

| Budget | Value |
|---|---|
| concurrent connections | 64 accepted, plus an admission queue of 64 with a 10 second wait, so a cold-start fan-in of many clients queues rather than failing |
| connections that have not completed the handshake | 48 concurrent, each with a 5 second deadline. Over the limit, a connection waits in the admission queue rather than being rejected. 48 is strictly above the 32-client cold-start fan-in gate G1.10 asserts, with margin for a stray connection in the same window |
| total buffered outbound bytes across all connections | 64 MiB; the victim is chosen by slowest drain rate and oldest unacknowledged frame, never by absolute buffered size, so a legitimate bulk transfer is not evicted in favour of a wedged reader |
| per-connection control lane, outbound | 256 messages **and** 1 MiB, whichever binds first; overflow coalesces duplicate notifications, then disconnects. Coalescing never applies to a command, only to outbound notifications, so it cannot conflict with the no-acknowledged-command-lost rule |
| per-connection render lane, outbound | one pending logical frame **per terminal**, not per connection. ADR-013's argument that dropping is safe is per terminal: a replacement is recomputed from that terminal's acknowledged revision. A single slot shared across terminals would drop one terminal's frame in favour of another's, with nothing to recompute it from until that terminal next changed |
| per-connection total outbound, all lanes | 8 MiB, with **no exemption**. See the arithmetic below |
| decoded object depth and size | capped per codec, enforced before allocation |

- The outer envelope shape is frozen from the first release and never changes.
- Payload interpretation is selected by negotiated **named codecs**, not by an integer
  version equality check. The client hello lists supported codec names per concern
  (control, event, terminal, blob); the server welcome names exactly which it selected,
  plus a capability list for optional behavior that needs no version bump.
- The terminal channel carries the negotiated terminal codec. Its first generation is the
  ADR-013 frame. It is deliberately not uninterpreted bytes: the daemon owns the only VT
  parser, so a client receives authoritative cell state, never a byte stream it would have
  to parse itself. An earlier draft said the opposite and contradicted ADR-003.
- A frame body larger than `MAX_FRAME` is chunked across frames on the same stream, with
  the END flag set on the last one. A full-screen snapshot at 200x60 exceeds the cap, so
  without this rule the resync path would have had no wire representation at all.
- **A chunk sequence is bounded, on the receiving side only.** Per inbound stream, at most
  8 MiB reassembled and at most 10 seconds from first chunk to `END`. Exceeding either is a
  typed protocol violation that closes the connection.

  This is a receiver rule and it is deliberately not applied to our own sending. A daemon
  sending a large snapshot to a client on a slow link is limited by that link, not by a
  deadline, and applying the receiver's rule to our own transmission would turn the mobile
  case into a disconnect loop. Outbound pacing is flow control and eviction by drain rate,
  which is the row below.

  The 8 MiB cap is per **control, event and terminal** stream. Channel 4 exists for bulk
  transfers precisely so a large artefact does not have to fit inside that cap, and it has
  its own per-transfer limit stated by the blob codec. Without this, any same-uid process, which ADR-012 says is every
  process the daemon launches, completes a handshake and streams individually valid chunks
  with `END` never set until the machine is out of memory, and one daemon serves every
  workspace on the machine.
- **A chunk sequence is atomic once started.** No other logical message may be interleaved
  on that stream until `END`. There is no message id in the envelope, so a receiver cannot
  detect a splice; without atomicity, a render-lane replacement partway through a chunked
  snapshot produces a body that is neither of the two messages and is applied as if it
  were valid, which is exactly the permanently-wrong-screen failure ADR-013 exists to
  prevent, reached through the mechanism ADR-013 added to prevent it. ADR-013 states the
  matching rule on its side: lane replacement takes effect only at a message boundary.
- Where the header `stream` and a body field name the same terminal, the header is
  authoritative and a mismatch is a protocol violation.
- Length is validated against a cap before any allocation.
- After decode, unconsumed trailing bytes are a protocol violation, not a silent ignore.
- Wire tags and representative payload hashes are frozen in tests, so reordering an enum
  cannot silently change the format.
- Frozen fixture files for the first generation may be superseded but never edited.

## Consequences
- One socket, per-connection tasks and per-lane bounded queues give the same isolation
  two sockets would give, with half the path-construction
  code.
- Cross-language clients (mobile, plugins) bind to a stable named codec rather than to
  a Rust serialization format.

## Rollback
Adding a channel is a generation bump, as the Decision section states, because a receiver
closes on an unknown channel. An earlier version of this section claimed a second transport
could be added later without one, which was not reachable under the ADR's own rules.

What is genuinely reversible is a codec: a new codec name can be negotiated at any time
without touching the envelope, which is what the negotiation exists for. Changing the
envelope is not reversible, which is the point of freezing it.

### The normative artifact is the code

`crates/qoop-proto` is the definition; this ADR is the reasoning and the rejected
alternatives. Where the two disagree, the code and its tests are right and this document is
a bug. That is not a dodge: three review rounds each found another decision an implementer
would have had to invent while reading this ADR, which is evidence about prose rather than
about the reviewers.

Writing it settled several things this document had left open or wrong. The header is ten
bytes, so the maximum payload is 65526. `stream` has a defined meaning on every channel
including the ones that do not use it, and a non-zero stream on the handshake channel is a
protocol violation rather than an undefined case. A single-frame message sets `END`. The
acknowledgement is a `u64` little-endian prefix before the codec body, on the first frame of
a sequence only, and it counts against that frame's payload budget. Interleaving is detected
by requiring a continuation's `seq` to be exactly the previous plus one, which is what makes
a chunk sequence atomic without adding a message id to a frozen header. Prose at this level of detail has been
through three review rounds and each one found another decision an implementer would have
had to invent, which is evidence about the medium rather than about the reviewers. The
grammar is frozen when two independent implementations of it agree on the corpus, and the
frozen fixtures are generated from that decoder, not typed.
