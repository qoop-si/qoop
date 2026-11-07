# Security

## Reporting a vulnerability

Report it privately, never in a public issue or pull request:

- on GitHub, through "Report a vulnerability" on this repository's Security tab, or
- by email to hey@qoop.si.

Include the qoop version (`qoop --version`), your operating system and its version, and the
steps that reproduce it. We reply within 7 days.

## Scope

- The code in this repository: the terminal, the daemon, the interface, agent detection,
  hooks, skills and the install script.
- The released binary from https://qoop.si/releases, including its closed parts (the phone
  bridge, the hub and the account features).

Only the latest release is supported: check that the problem still happens there before you
report it.
