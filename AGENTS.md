# Agent workflow for this repo

1. Never work directly in the plain checkout (`/Users/me/coding/qoro`).
   It is the pristine main. Always start a fresh worktree from current
   `origin/main` and do all implementation, review and test loops there.
2. Commit, push, opening a PR and rebase-and-merge are pre-authorized.
   Never push directly to `main`; land work through a PR with rebase only.
3. After the merge, update the plain checkout (`git pull`) and take a
   fresh build, so the installed binary matches the new main.
4. Then clean up: remove the worktree (`git worktree remove <path>`) and
   delete the merged remote branch (`git push origin --delete <branch>`),
   so no stale worktree or branch is left behind.
