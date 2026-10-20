# FAQ and troubleshooting

## Does closing the window stop my agents?

No. The window is only a view. The daemon keeps every terminal running; `qoop` in any other
window brings the same tabs back. `qoop stop` is what ends them.

## What happens after a reboot?

The tabs, spaces and panes come back with their history the next time you run `qoop`. The
programs inside them do not survive a reboot: see [Detach, stop and restore](#sessions) for
what comes back and how an agent resumes its conversation.

## `qoop read` says "nothing is awake"

No tab is awake, so no daemon is running: `qoop` or `qoop wake` starts it. `qoop ls` lists
every space and whether it is awake, asleep or stopped. Commands that act on a terminal never
start a daemon themselves.

## An agent shows the wrong state

`qoop agents explain 3` says what qoop saw: the state, where it came from (the agent's own
hook, or its screen), and which rule matched. Installing the agent's hook
(`qoop hooks install <agent>`) makes the state come from the agent itself. A rule can also
be overridden without a release: see [Supported agents](#agents).

## The keys do nothing inside tmux or screen

Both take their own prefix first. Give qoop another one in
[Configuration](#configuration), for example `prefix = "ctrl-b"` inside `screen`, or keep
qoop's `ctrl-a` and tmux's `ctrl-b` apart.

## No desktop notification appears

`qoop settings` says where notifications go from that terminal (its desktop row), or why
nowhere. The usual fixes are in [Notifications](#notifications).

## Colors look wrong on a light terminal

qoop asks the terminal for its background and follows it. A terminal that does not answer
is taken as dark; set `theme = "light"` in [Configuration](#configuration).

## A killed qoop left my terminal in a mess

Run `qoop reset`: it puts the terminal back from raw mode.

## Which systems does it run on?

macOS (Apple silicon and Intel) and Linux with glibc 2.17 or newer (x86_64 and arm64), so
CentOS 7 and newer, Debian, Ubuntu and Fedora. Linux with musl (Alpine) is not supported
yet: the install script says so rather than installing a binary that cannot run.
