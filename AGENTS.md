# Agent workflow for this repo

1. Never work directly in the plain checkout of this repository.
   It is the pristine main. Always start a fresh worktree from current
   `origin/main` and do all implementation, review and test loops there.
2. Commit, push, opening a PR and rebase-and-merge are pre-authorized.
   Never push directly to `main`; land work through a PR with rebase only. The one exception
   is the version bump that `qoro-private/scripts/release.sh` pushes when the owner asks for a release.
3. After the merge, update the plain checkout (`git pull`) and install a
   fresh release build (the qoro app build), so the installed binary matches the new
   main. Never install a build of this repository alone over
   `~/.local/bin/qoro` on a machine paired with a phone: it has no phone
   part, and the bridge service there would stop.
4. Then clean up: remove the worktree (`git worktree remove <path>`) and
   delete the merged remote branch (`git push origin --delete <branch>`),
   so no stale worktree or branch is left behind.
5. The whole path from a request to a shipped change, its checks and what to ask
   first are in `.qoro/workflow.md`.
