# Other machines

A qoop on a server keeps running when you log out, like the one here does. Add it once and
its spaces are listed beside this machine's, as spaces: there is no separate machine list.

From the interface, the `+` beside "spaces" offers `connect a machine`: type its ssh address,
pick one of the sessions already running there or name a new one, and the screen moves to it.
From a shell:

```
qoop remote add devbox me@devbox      checks the qoop there
qoop                                  devbox:api, devbox:web under this machine's spaces
qoop --remote devbox                  only that machine
```

It is your system `ssh` with your own `~/.ssh/config`, so jump hosts, keys and agent
forwarding work as they do for `ssh devbox`. Everything in a remote space runs over there: a
new tab, a split, a new space made while you are in one.

If the connection drops, its spaces read as offline within about fifteen seconds and come
back on their own, with the same tabs, at most ten seconds after the network does.

qoop has to be installed on the other machine already. `qoop agents`, `send` and `read`
speak only to this machine's daemon.
