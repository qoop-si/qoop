# Changelog

## [0.16.1](https://github.com/qoop-si/qoop/compare/v0.16.0...v0.16.1) (2026-11-14)


### Fixed

* **cli:** quote typed shell words safely for fish ([0225184](https://github.com/qoop-si/qoop/commit/02251842071ace09b348d7a1c464cfc05ed3a829))

## [0.16.0](https://github.com/qoop-si/qoop/compare/v0.15.0...v0.16.0) (2026-11-14)


### New

* **cli:** qoop race new and ls, one prompt to many agents ([f08f832](https://github.com/qoop-si/qoop/commit/f08f8324cd8aced126d57419a0da75d9c2bb6733))
* **cli:** qoop race rm and ls --test ([3eede6b](https://github.com/qoop-si/qoop/commit/3eede6b764ef954b7571e382db9efe2bd39c7b09))
* **worktree:** finish a worktree with a proof, a pull request, a fast-forward or a discard ([f77ef20](https://github.com/qoop-si/qoop/commit/f77ef2028f8fe5f51ec0e2125f1569d45b5649a1))
* **worktree:** list stale worktrees with ls --stale ([30027e5](https://github.com/qoop-si/qoop/commit/30027e58ea40d60afed2ed8d512d4498e8d60123))
* **worktree:** open a worktree's tab in its repository's space and claim an index ([7ac3dd7](https://github.com/qoop-si/qoop/commit/7ac3dd709e35303ca563533d499c25bdc4cce349))
* **worktree:** set up a new worktree from its space's config ([e51a3c1](https://github.com/qoop-si/qoop/commit/e51a3c18573c21227413dec462c277cd5b0284e0))


### Fixed

* **cli:** race rm closes only its own tabs and rechecks each racer ([c682371](https://github.com/qoop-si/qoop/commit/c682371119106289baccbe5361630b30a6a83418))
* **cli:** race rm closes only the racer's own tab ([2bd63c6](https://github.com/qoop-si/qoop/commit/2bd63c68efd977795edcf121ff509f84aff7e67f))
* **cli:** race rm finds a pruned racer's tab and judges unknown by row ([df95eb0](https://github.com/qoop-si/qoop/commit/df95eb0ccee2b05b926670c3e0a18f7383eb3e2b))
* **update:** wait out a busy binary when asking its version ([814148c](https://github.com/qoop-si/qoop/commit/814148c146c40b342812df207c928e0bcad31ae5))
* **worktree:** call a worktree with nothing the base lacks merged ([d92716a](https://github.com/qoop-si/qoop/commit/d92716a23d239718d8d344fdee610a43685f69a6))
* **worktree:** pass over the finished branch when picking the default base ([955f208](https://github.com/qoop-si/qoop/commit/955f208fc122bcb71287899044ec774677660819))
* **worktree:** prove before deleting, full ref names, bounded remote calls ([10b9e5e](https://github.com/qoop-si/qoop/commit/10b9e5e64484e0b231ef8ae33bfffc1728314be9))
* **worktree:** read origin/HEAD as its branch and bound the pipe drain ([6f4fa0c](https://github.com/qoop-si/qoop/commit/6f4fa0c010513158bdfca92cfc5a3717b32cf410))
* **worktree:** tighten the worktree setup rules and its copy ([68015c7](https://github.com/qoop-si/qoop/commit/68015c7cbc75d92c5fbae3f0c020fbe0deda6bd7))

## [0.15.0](https://github.com/qoop-si/qoop/compare/v0.14.0...v0.15.0) (2026-11-14)


### New

* **asks:** questions carry options, leave after a day and come back ([4718ccc](https://github.com/qoop-si/qoop/commit/4718ccc141d8eb4f32d46f2a33009d811303937d))
* desktop app core, agent menu reader and question notes ([cb18276](https://github.com/qoop-si/qoop/commit/cb18276cd8344c0cd19054e749ece5b4454e28f1))
* **parsed:** rows carry questions, workflows, labels and idle teammates ([6f78cfb](https://github.com/qoop-si/qoop/commit/6f78cfb225a907189e493f376dd9a651bbf5207e))
* **runtime:** AgentRows answers only the rows a client names ([5b40388](https://github.com/qoop-si/qoop/commit/5b403884ba71a63f5add1d23af4c5f1b5b698d5d))
* **runtime:** quiet_rows tells a phone only the row changes it shows ([a30d581](https://github.com/qoop-si/qoop/commit/a30d5811e118fd2eb3b55c9fbf258f6d81b75cda))


### Fixed

* **board:** a task waiting on its agent's sub-agents is working too ([2a1fed7](https://github.com/qoop-si/qoop/commit/2a1fed7494a32d5bb9d2e2b30bda14319863e464))
* **board:** a task whose turn runs is working, not waits ([fb2b6f1](https://github.com/qoop-si/qoop/commit/fb2b6f1f6e915d0b3b4fd79f5518eb10f0a1e707))
* **hooks:** a Claude session closing is no finish ([1dcd975](https://github.com/qoop-si/qoop/commit/1dcd9755aaad44039b58d3687360f11594271bc5))
* **hooks:** give the turn's git look five seconds, as the other calls get ([6a34dc5](https://github.com/qoop-si/qoop/commit/6a34dc5ce90caa12e19d589fc9fd6ee554d69311))
* **pty:** let one /proc scan answer a settle's group lookups on Linux ([0117d2e](https://github.com/qoop-si/qoop/commit/0117d2e176aaf92e675eac077958bc501119b141))
* **qass:** say why the watch switch could not be saved ([6d560b4](https://github.com/qoop-si/qoop/commit/6d560b4b69c94143805454efbe72a5270135e5a6))
* **release:** hub step as its own job, re-runs that keep the release whole ([2e7252e](https://github.com/qoop-si/qoop/commit/2e7252e56e17345889569cd63423403252f8e993))
* **remote:** tell a fish login shell by its version ([81fcaae](https://github.com/qoop-si/qoop/commit/81fcaaedb085b9cce8ec9d751b48a9875129841c))
* **runtime:** a closed task is news to a quiet phone, and a raced look looks again ([f6eddca](https://github.com/qoop-si/qoop/commit/f6eddca5054f1053827f0e61783fb40ab0821658))
* **runtime:** announce a held blocker once, not every 800 ms ([e2c3522](https://github.com/qoop-si/qoop/commit/e2c352269a5a5b8ef013e9bee18a3d7386ca9e69))
* **runtime:** announce a task's row once, and finish only agents' turns ([8af6bcc](https://github.com/qoop-si/qoop/commit/8af6bcced6971822f1801d3f88a78a9ab3ea5843))
* **runtime:** check a caller's token under one lock, not against a stale read ([6986827](https://github.com/qoop-si/qoop/commit/69868274273effc89807831ff9b988637697a401))
* **runtime:** decide each agent row's status once in the daemon ([a5cb55f](https://github.com/qoop-si/qoop/commit/a5cb55f45c63807ceba4298c4e51c1780b09aef1))
* **runtime:** ring the probe on input only, and wait out a busy exec only where nothing leaks ([2dfe578](https://github.com/qoop-si/qoop/commit/2dfe578f8579348c64803f2324151b556297dfc2))
* **runtime:** settle only the terminals that moved on the probe platform ([4046f9e](https://github.com/qoop-si/qoop/commit/4046f9e3078a8a17e6bc9ede588adc6aab125b68))
* **runtime:** the agent's own spinner starts a turn no hook reported ([a66f30e](https://github.com/qoop-si/qoop/commit/a66f30e392f6b88007a5f5a85b901b7ba9a6a9e8))
* **runtime:** wait out a busy binary before a move gives up ([902fc96](https://github.com/qoop-si/qoop/commit/902fc96ac4e003420eb5e1514aec495b5a4d8e3d))
* **status:** a turn that runs is working, whatever card or stage is open ([626b312](https://github.com/qoop-si/qoop/commit/626b312413494fef7a03802625a0bd0685419b74))
* **tui:** draw the daemon's status in the sidebar and the notch ([c5734b0](https://github.com/qoop-si/qoop/commit/c5734b00f8e1a58b90009494e754f33455e5bd27))
* **tui:** read the status in the last views; one bell a finish ([843d79f](https://github.com/qoop-si/qoop/commit/843d79f3399ebe56b2693f3074fa87710a1a36ec))
* **tui:** take the remote swap lock with the shell's exclusive create ([0dfdf63](https://github.com/qoop-si/qoop/commit/0dfdf631041b5dbb18220f2a5dbcc4e3b3a269ae))
