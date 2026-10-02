# FAQ and troubleshooting

## Does closing the window stop my agents?

No. The window is only a view. The daemon keeps every terminal running; `qoro` in any other
window brings the same tabs back. `qoro stop` is what ends a session.

## What happens after a reboot?

The tabs, spaces and panes come back with their history the next time you run `qoro`. The
programs inside them do not survive a reboot: see [Sessions and restore](#sessions) for
what comes back and how an agent resumes its conversation.

## `qoro read` says "no session running"

Nothing is running in this session yet, or you are asking a different session than the one
you started. `qoro ls` lists the sessions and which of them are running; `--session NAME` or
`QORO_SESSION` picks one. Commands that act on a terminal never start a daemon themselves.

## An agent shows the wrong state

`qoro agents explain 3` says what qoro saw: the state, where it came from (the agent's own
hook, or its screen), and which rule matched. Installing the agent's hook
(`qoro hooks install <agent>`) makes the state come from the agent itself. A rule can also
be overridden without a release: see [Supported agents](#agents).

## The keys do nothing inside tmux or screen

Both take their own prefix first. Give qoro another one in
[Configuration](#configuration), for example `prefix = "ctrl-b"` inside `screen`, or keep
qoro's `ctrl-a` and tmux's `ctrl-b` apart.

## No desktop notification appears

`qoro settings` says where notifications go from that terminal (its desktop row), or why
nowhere. The usual fixes are in [Notifications](#notifications).

## Colors look wrong on a light terminal

qoro asks the terminal for its background and follows it. A terminal that does not answer
is taken as dark; set `theme = "light"` in [Configuration](#configuration).

## A killed qoro left my terminal in a mess

Run `qoro reset`: it puts the terminal back from raw mode.

## Which systems does it run on?

macOS (Apple silicon and Intel) and Linux with glibc 2.17 or newer (x86_64 and arm64), so
CentOS 7 and newer, Debian, Ubuntu and Fedora. Linux with musl (Alpine) is not supported
yet: the install script says so rather than installing a binary that cannot run.
