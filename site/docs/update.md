# Update and uninstall

## Update

```
qoro update --check     note what the newest release is, change nothing
qoro update             install it over this binary
```

`--check` leaves a note the interface reads, so the index signs itself `0.3.0>0.4.0` while
an update is waiting; the interface itself never touches the network. `ctrl-a U` says how it
installs.

`qoro update` refuses while any session is running, and names them, so no agent dies to a
routine upgrade: stop them with `qoro stop <name>` first. The download is checked against its
sha256, started once to confirm its version, and swapped in with one atomic rename. On an
Apple silicon Mac it installs the Apple silicon build even when run from a Rosetta shell.

## Uninstall

```
qoro hooks uninstall
qoro skills uninstall
qoro stop                 for each running session (qoro ls lists them)
rm "$(command -v qoro)"
rm -rf ~/.config/qoro
```

Session data (history, the index of what was open) lives under `$XDG_RUNTIME_DIR/qoro`, or
the system temporary directory when that is unset; remove it too for a clean slate.
