# Other machines

A qoop on a server keeps running when you log out, like the one on your computer does. Add it
once and its spaces are listed beside your computer's own, as spaces: there is no separate
machine list.

From the interface, the `+` beside "spaces" offers `connect a computer`: type its ssh address, and
its spaces join the list; the screen moves to them when they arrive.
From a shell:

```
qoop remote add devbox me@devbox      checks the qoop there
qoop                                  devbox:api, devbox:web under your own spaces
qoop --remote devbox                  only that machine
```

It is your system `ssh` with your own `~/.ssh/config`, so jump hosts, keys and agent
forwarding work as they do for `ssh devbox`. Everything in a remote space runs over there: a
new tab, a split, a new space made while you are in one.

If the connection drops, its spaces read as offline within about fifteen seconds and come
back on their own, with the same tabs, at most ten seconds after the network does.

If qoop is not on the other machine, `qoop remote add` offers to install it there, with its
hooks and skill. `qoop agents`, `send` and `read` speak only to the daemon of the computer you
run them on.

## Servers

To keep agents working on a Linux server while your computer is closed, in this order, on the
server:

1. Install qoop: `curl -fsSL https://qoop.si/install.sh | sh` (or let `qoop remote add` do it).
2. Let what runs there outlive your ssh session: `sudo loginctl enable-linger $USER`. Logged in
   as root, it is `loginctl enable-linger root`.
3. Check it: `loginctl show-user $USER -p Linger` prints `Linger=yes`.
4. Run `qoop login` on the server itself.

The server shows on qoop.si/app and on your phone once your computer is signed in, but that
route goes through your computer's qoop, so it ends while your computer sleeps. Signed in on its
own (step 4), the server is reached directly, with your computer asleep or off.

## Updates

A machine whose qoop is older than the one on your computer follows it on its own. When a release of this
version exists for that machine's platform, the interface puts that release's binary in place
of the old one over the same ssh, in the background, and the daemon over there moves onto it
and its tabs keep running through it where both qoops can hand them over. Otherwise it
moves at the first moment nothing there is working: a running command, a busy agent or a
background job holds it, and the interface says once what it is waiting on. Tabs keep their
numbers and agents come back in their own conversations. A build from main between releases
is never sent anywhere, and a machine whose qoop is from before this needs one update by hand there (`qoop stop`,
then the install script or `qoop update`) and follows on its own after that.
