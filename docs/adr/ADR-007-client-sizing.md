# ADR-007: Per-client terminal sizing

Status: accepted
Date: 2026-09-09

## Context
A resize that takes no client identity lets the
last caller win unconditionally, so one server-global foreground client drives the
shared PTY size. A phone attaching at 50 columns therefore
reflows the desktop user's shell, which is one of the reported failures this decision exists to prevent.

## Decision
```rust
pub enum SizePolicy {
    Owner(ClientId),   // default, first writable attacher
    Fixed(PtySize),    // headless and CI
    Negotiated,        // later, explicit opt-in only
}
```
- Only the size owner's resize reaches `ioctl(TIOCSWINSZ)`.
- Read-only observers can never resize under any policy.
- A client smaller than the logical grid renders a viewport over it, cropping or
  scrolling locally.
- Ownership transfer is an explicit command that emits an event, or a person's key in an
  interface (amended 2026-11-20, below). It is never an implicit side effect of focus, of a
  resize request or of attach order after the first.

### Ownership lifecycle, which the first draft omitted

`Owner(ClientId)` with no transfer rule strands a terminal at a dead client's geometry.
The full rules:

| Event | Result |
|---|---|
| owner detaches cleanly | ownership passes to the writable client that was refused a size first, and the size it last asked for applies at once; if none asked, the terminal is unowned and the next writable resize claims it |
| owner connection dies | same as a clean detach, triggered by the connection's close, not by a timeout |
| the owner's link reconnects | its new connection names the link (hello key `replaces`), and the daemon closes the ones that link made before: over ssh a dropped network leaves the far relay holding the old socket, and the reconnect is the proof it is dead. The close is the row above, not a steal |
| a writable client attaches to an unowned terminal | it becomes the owner and may resize |
| a writable client attaches to an owned terminal | it does not become the owner and cannot resize |
| the owner no longer shows the terminal (it unsubscribed), and a writable client that shows it was refused a size | ownership passes to the one of those refused first, and its size applies; the old owner waits with the size it had, and gets it back the same way. An owner that still shows the terminal keeps it |
| a client requests ownership | the current owner is asked; it must actively agree. A silent or slow owner **keeps** ownership |
| an observer requests ownership | rejected, always |
| a person presses a key or pastes in an interface (the TUI or the desktop app) that shows the terminal and was refused a size | ownership passes to that interface and the size it last asked for applies; the old owner waits first in line with the size it had, and takes it back the same way. As tmux's `window-size latest` |

A resize from a non-owner is answered with a typed error, not ignored, so a client can
tell the difference between "you are not the owner" and "your message was lost".

The silent-owner rule matters more than it looks. Every process running as this user has
full protocol authority (ADR-012), so a timeout-based steal means an agent, a plugin or a
wedged mobile client can reflow a human's 180 column shell to 50 columns simply by asking
while the desktop client is briefly busy. Automatic transfer belongs only to the
closed-connection rows above. A genuinely stuck owner is resolved by a human-confirmed
action on the requesting client, never by a timer.

Gate G2.14 asserts this directly: an owner that never answers keeps ownership. An earlier
version of the plan cited the quiesce gate for this property, which tests foreground
process correctness and touches neither sizing nor ownership.

Mouse coordinates from a cropped viewport are translated into logical grid coordinates by
the daemon, using the viewport offset the client declared. A client never sends logical
coordinates it cannot know.

### What the interface sizes

The interface sizes every terminal it lays out to the rectangle it is drawn in: the tiles on
screen, the other tabs and spaces at the whole pane, and a pane hidden by zoom left at its
last size. Another machine's terminals are sized only once shown here, and the tabs off
screen wait for a drag of a divider or the panel to end. So the first interface on a session owns the geometry of what it shows, as one client
controls a tab's geometry in a terminal multiplexer. It sends a terminal a size only when that
size changed, because an equal size is the repaint nudge.

An interface subscribes to exactly the terminals it draws, so "shows" is that subscription.
The row above exists because every interface also sizes what it does not draw: with two
windows open, the one behind sized a split made in the one in front before the front one did,
owned it, and the front window drew the top-left corner of a grid larger than its pane. An
owner that is not showing a terminal has no view for a reflow to break, so yielding it is not
the steal the silent-owner rule forbids: the phone never resizes, and a client that never
asks a size is never refused one.

### Amendment 2026-11-20: a person's key takes the size

The owner reported that a message typed to an agent in the desktop app did not go to the next
line: the TUI owned the size, so the desktop drew a crop, and the line ran past the window's
edge. Typing is where a person is; tmux has sized a window by the client that last had
activity since 3.1. So a key in an interface takes the size, under these limits:

- Only an interface connection (`qoro-tui`: the TUI, the desktop app, a link carrying one)
  takes it, and only with input that holds a key a person pressed (`agents::person_key`) or a
  paste. Focus reports, mouse reports, cursor position reports and a terminal's other replies
  never do.
- Never: the phone app, `qoop attach`, `qoop send`, `qoop prompt`, qass, restore replay, and an
  answer from a question card (it comes through the control channel). None of them asks a
  size except `qoop attach`, which is excluded by kind.
- A TUI run over ssh from a phone is an interface: typed in at 50 columns, it takes the size
  until the desk's next key. The failure in the Context, a phone attaching and reflowing the
  desk, stays prevented: attaching and resizing still never move the size.
- The TUI turns the wheel into arrow keys over a full screen program that did not ask for
  the mouse, so a wheel there takes the size too, as tmux counts the mouse as activity.
- Asking for ownership (`GrantOwnership`, G2.14) is unchanged: a silent owner keeps it.

### Known exception 2026-11-24: a reply in the person's own words

An agent that stopped and waits for words (no menu to pick from) is answered from the desktop's
Inbox by a reply the person types. It is sent on the interface connection as a
paste and an Enter, so it counts as the person's typing and that interface takes the size, even
when the Inbox covers the terminal. An answer picked on a menu (`AnswerMenu`, `AnswerMenuText`)
and a hand to qass still never move the size. The owner accepted this exception for now; the fix
is a reply path on the control channel that does not count as typing, a daemon and protocol
change kept for stage B of the redesign (plans/2026-11-23-redesign-v3-build.md, D1).

## Consequences
- A small client sees a cropped view rather than a reflowed one. This is the correct
  trade: reflowing someone else's working shell is destructive, cropping is not.
- Focus is client-local and carries no sizing authority, which also removes the
  mechanism behind a reported failure where a resize reached a terminal nobody had claimed.
- With two interfaces on one agent, the one a person types in reflows the agent to its width,
  once per switch; the agent redraws on each switch.

## Rollback
`Negotiated` exists as a named variant so a future policy can be added without changing
call sites. What must never return is an implicit "last resize wins". The 2026-11-20
amendment is "last interface typed in wins", which a resize request alone never triggers;
removing it is deleting the `Typed` command the connection sends ahead of a person's input.
