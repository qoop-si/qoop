# Sleep

A tab you are not using can sleep: it ends, nothing of it takes CPU or memory, and waking
it brings back the same number, name, split, directory and scrollback, the last screen
included, with its agent resumed into its own conversation. The rest of its space is left
as it is. A sleeping tab stays in its space, dimmed, on the bar and in the column, and keeps
the name its conversation had. A click on it wakes that tab alone and opens it. Reaching
for it from the palette shows the last screen it drew, dimmed, without waking anything:
Enter or `ctrl-a Z` wakes it once you have seen where it was. A program on the alternate
screen leaves no last screen behind, and its tab says so.

A tab sleeps whole, every pane of its split, or not at all: a command still running, an
agent that is working or has not told its conversation yet, or a job left in the background
keeps it awake, and the refusal says which pane and why. Putting a space to sleep sleeps
each tab that can and says which stayed awake. When nothing is awake and no interface is
attached, the daemon itself leaves ten seconds later, so a machine with everything asleep
runs nothing of qoop; the next `qoop` starts it and wakes the tab that slept last.

A space's row shows `+` and `⋯`; the `⋯` slides out the rest of its controls (sleep or
wake, keep awake, settings, close) and turns into `›` to put them back. With `[sleep]
idle_minutes` set, the cup on an agent's row or in a space's drawer keeps it out of idle
sleep: click it and it fills, click again and idle sleep may take it. A full cup or a
sleeping space's moon stays beside `+` while the drawer is closed. A sleep you ask for still
sleeps it.

```
ctrl-a Z                sleep the tab you are in, or wake the sleeping one you are on
ctrl-a a                the keyboard on the agents column: move, open, search, sleep
qoop sleep --tab 7      the same from a shell, by tab number
qoop sleep api          every tab of a space, by name or id
qoop wake --tab 7       wake one tab; `qoop wake api` every tab of a space
qoop wake               wake the tab that slept last
qoop ls                 every space, awake, asleep, partly asleep or stopped, and its tabs
qoop --space api        the interface with that one space in it; `qoop` shows them all
qoop stop               end everything now: what was awake comes back on the next open
```
