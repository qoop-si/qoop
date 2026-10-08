# Update and uninstall

## Update

```
qoop update --check     note what the newest release is, change nothing
qoop update             install it and move every machine onto it
```

`--check` leaves a note the interface reads, so the index signs itself `0.3.0>0.4.0` while
an update is waiting; the interface itself never touches the network. `ctrl-a U` says how it
installs.

After the install, `qoop update` asks the daemon on this machine, and on each saved remote,
to move onto the new binary. A working agent or a running command is never ended. When both
the running qoop and the new one can hand tabs over, they keep running and nothing waits;
otherwise the daemon moves at the first moment nothing there is working, ends the idle tabs
and restores them, with agents resumed in their own conversations. The first move onto a build
that has this is the second kind.

The download is checked against its sha256, started once to confirm its version, and swapped
in with one atomic rename. On an Apple silicon Mac it installs the Apple silicon build even
when run from a Rosetta shell.

## Uninstall

```
qoop hooks uninstall
qoop skills uninstall
qoop stop                 for each running session (qoop ls lists them)
rm "$(command -v qoop)"
rm -rf ~/.config/qoop
```

Session data (history, the index of what was open) lives under `$XDG_RUNTIME_DIR/qoop`, or
the system temporary directory when that is unset; remove it too for a clean slate.
