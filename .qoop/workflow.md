# How work ships in qoop

1. Read the real code and logs first; check how other multiplexers handle it where it applies.
2. Work in a fresh worktree from current `origin/main`, never in the plain checkout; rebase onto
   `origin/main` at each phase and before committing.
3. Write the plan to `plans/<date>-<slug>.md` (never committed) with a done/not-done table.
   Review it with sub-agents until every reviewer is satisfied; no Codex review in this repo.
4. Implement in phases; each phase gets a code review and its fixes before the next starts.
5. Review the tests: they fail when the change is reverted, and cover what the plan promised.
6. Run `cargo fmt --check`, `cargo clippy --workspace --all-targets`,
   `uv run python scripts/check-prose.py` and the tests you touched, then
   `cargo test --workspace`. There is no CI: these are the gate.
   A test starts `qoop` only through `qoop_testkit::sandboxed`, which gives it a home of its own:
   the binary moves the home's old directories on its first command, and a build under a `target`
   directory does not unless it has that mark (see `legacy.rs`).
7. Run it for real and keep the output: the CLI in a qoop or tmux terminal, the app on a
   simulator or device, a screenshot for anything visual. Never on the owner's live daemon,
   agents or qass: use an isolated `QOOP_RUNTIME_DIR`.
8. Commit through `/commit`, push the branch, open a PR (`ref` an issue, never `closes`), and
   rebase-and-merge it. Pre-authorized here by `AGENTS.md`; never push to `main` directly.
9. Pull `main` in the plain checkout, take a fresh release build (the qoop app build, never
   this repository alone on a paired machine), install it to
   `~/.local/bin/qoop`, and run `qoop bridge reload` after replacing the binary.
10. Clean up: remove the worktree, delete the local and remote branch, drop the worktree's
    `target/`.

## Done means

The PR is merged (`gh pr view <n> --json state,mergedAt`), its commit is on `origin/main`, the
installed `~/.local/bin/qoop` is newer than the merge, the worktree is gone (`git worktree
list`), and the task evidence names the real run and its output.

## Loop limit

At most 8 review rounds per task, plan, code and test reviews together; a round is a change of
the task's stage to `reviewing`. At the limit, stop and ask the owner.

## Ask the owner first

A new dependency (`cargo add`, `bun add`, `uv add`), a tag, a version or a changelog edit
(release-please owns those), a design or naming pick, anything external (email, a post, an issue
comment, publishing), and the repo going public.

## Verify where it runs

- where: `installed ~/.local/bin/qoop`
- check: its mtime is after the merge and `~/.local/bin/qoop --version` runs; `launchctl list |
  grep si.qoop.bridge` shows a PID. Then the command the task changed, against an isolated
  stack only: `mktemp -d /tmp/qv.XXXXXX` prints `<s>`, and every qoop command is written out
  as `env QOOP_RUNTIME_DIR=<s> XDG_CONFIG_HOME=<s>/c XDG_DATA_HOME=<s>/d ~/.local/bin/qoop
  <args>` (start with `ensure-awake`, end with `stop`), its output kept; then `rm -rf <s>`.
- never: a qoop command without those three variables, the owner's live daemon, agents,
  spaces or qass, `launchctl` writes, the owner's phone.
