# The command line surface

Owned by `qoro-cli` and grown with it from milestone 1 onward, so that the enumeration
exists before a gate needs it rather than being invented when one does.

Gate G9.1 reads this file: with the control-plane host blackholed, every subcommand here
produces the same result as when it is reachable. Four rows are listed but not run by it:
`qoro`, `qoro attach` and `qoro daemon` do not return, and `qoro relay` starts a daemon and then prints
whatever that daemon says to a client rather than a result of its own. They are still
required to be here, and the gate asserts that this table and `qoro --help` name the same
set, which is what stopped `relay` from staying undocumented after milestone 6 added it.

An optional control plane is the whole claim here, and a claim that is only made in prose is
a claim nobody has to keep.

## Subcommands

| Command | What it does | Needs the network |
|---|---|---|
| `qoro` | open the interface, starting the daemon if there is not one | no |
| `qoro attach` | attach to one terminal, with no interface around it | no |
| `qoro daemon` | run the daemon in the foreground | no |
| `qoro reset` | restore a terminal left in raw mode by a killed client | no |
| `qoro relay` | pipe stdin and stdout to this machine's daemon, which is what `ssh` starts on the far side | no |
| `qoro setup show <file>` | what a Setup will run, its content hash and its reproducibility level | no |
| `qoro setup trust <file>` | confirm a Setup on this machine | no |
| `qoro setup check <file>` | whether a Setup may run here | no |
| `qoro --hello` | what a remote bootstrap asks for: version, codec generation, platform | no |
| `qoro --help` | this surface | no |
| `qoro --version` | the version | no |

Every row says no, and that is the design rather than a coincidence: there is no control
plane in the product, and if one is ever added it is an addition to this table with a yes,
plus a row in the gate saying what happens when it is unreachable.

## What "the same result" means

Byte for byte on stdout, and the same exit status. Not "still works": a subcommand that
printed a warning about an unreachable host when the host is unreachable would be a
subcommand whose output depends on the network, which is the thing being ruled out.

## Adding a subcommand

Add it here in the same change. The gate enumerates this file, so a subcommand that is not
here is a subcommand nobody checks, and a row here with no subcommand fails the gate too.
