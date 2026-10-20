# Other machines

A qoop on a server keeps running when you log out, like the one here does. Add it once and
its spaces are listed beside this machine's, as spaces: there is no separate machine list.

From the interface, the `+` beside "spaces" offers `connect a computer`: type its ssh address, and
its spaces join the list; the screen moves to them when they arrive.
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

If qoop is not on the other machine, `qoop remote add` offers to install it there, with its
hooks and skill. On Linux, the server needs `sudo loginctl enable-linger <user>` once so that
what runs there outlives your ssh session. Once this machine is linked with `qoop login`, the
server also shows on qoop.si/app and on your phone. `qoop agents`, `send` and `read` speak
only to this machine's daemon.

## Updates

A machine whose qoop is older than this one follows it on its own. When a release of this
version exists for that machine's platform, the interface puts that release's binary in place
of the old one over the same ssh, in the background, and the daemon over there moves onto it
and its tabs keep running through it where both qoops can hand them over. Otherwise it
moves at the first moment nothing there is working: a running command, a busy agent or a
background job holds it, and the interface says once what it is waiting on. Tabs keep their
numbers and agents come back in their own conversations. A build from main between releases
is never sent anywhere, and a machine whose qoop is from before this needs one update by hand there (`qoop stop`,
then the install script or `qoop update`) and follows on its own after that.
