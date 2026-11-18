# Changelog

## [0.19.1](https://github.com/qoop-si/qoop/compare/v0.19.0...v0.19.1) (2026-11-18)


### Fixed

* **tui:** mouse selection keeps every row across a scroll ([700b49f](https://github.com/qoop-si/qoop/commit/700b49f7fff1e0f782df30e65c2ac0ab3f5edc1e))

## [0.19.0](https://github.com/qoop-si/qoop/compare/v0.18.1...v0.19.0) (2026-11-18)


### New

* **desktop:** answer agents from the window, with a hold and qass ([1fcc49d](https://github.com/qoop-si/qoop/commit/1fcc49d8318e7c48fb74db3388b8b3a2bc84d3ef))
* **desktop:** app bundle, --version and the missing qoop line ([0b8df77](https://github.com/qoop-si/qoop/commit/0b8df772aaa6689b66f3b396747efe06e3bab55c))
* **desktop:** app shell, home and New work through the mux ([3da6615](https://github.com/qoop-si/qoop/commit/3da66157d427dc2ab0f93c0873a8b1aba9e0dbfa))
* **desktop:** machine states, remote settings and pets ([52d10c1](https://github.com/qoop-si/qoop/commit/52d10c10758a5509a25c1ea246166d380f4d0537))
* **desktop:** qass page with its terminal, switches and rules ([b3e0bbc](https://github.com/qoop-si/qoop/commit/b3e0bbce1a23de23ec7a0ce5fe0e09d644364b7b))
* **desktop:** schedules page with run history and reports ([bd04369](https://github.com/qoop-si/qoop/commit/bd043696306aa95a4c522a8b08b8885db010fb03))
* **desktop:** session view with panes, task strip and review ([eeac53e](https://github.com/qoop-si/qoop/commit/eeac53e856f04ad5964d42f3dc43e49b61717d3d))
* **desktop:** settings page for This Mac with the file beside it ([0bb6f32](https://github.com/qoop-si/qoop/commit/0bb6f3242f466bdec01a6b0b4750c20a6f937c7c))
* **schedule:** run history per schedule and qoop schedule edit ([489503a](https://github.com/qoop-si/qoop/commit/489503aa4ae6a4f39899ec580540c283254a58c9))


### Fixed

* bounded config lock, ASCII window keys, watch open runs ([cc36e4d](https://github.com/qoop-si/qoop/commit/cc36e4d6d75ade4c658e9e0ba15194bbf61c8268))
* build again with link_reason and the shared focus ring ([b35faff](https://github.com/qoop-si/qoop/commit/b35faff26da7bf8cbf39c4106a7f3c3104a88aad))
* **config:** one writer of config.toml at a time; settings fixes ([ab00698](https://github.com/qoop-si/qoop/commit/ab006983be7e3a5107e3c61443bf422b0ad8be95))
* **desktop:** a down machine reads by name and retries at once ([20e1ec2](https://github.com/qoop-si/qoop/commit/20e1ec254c83dae930581bb95447ec5ae6f4853b))
* **desktop:** a new question starts clean; strip, note and next line ([658580c](https://github.com/qoop-si/qoop/commit/658580cb1adfd5646ac967b92423fc9797dc638f))
* **desktop:** a remote's answer through Via counts as heard ([46fa42e](https://github.com/qoop-si/qoop/commit/46fa42ee28e6dff2c88f9dc5c60a7e04cf4e4ffc))
* **desktop:** a stale card says what it waits for ([d8215a4](https://github.com/qoop-si/qoop/commit/d8215a4772d8541a05d5417f15569368d62ce7f5))
* **desktop:** any traffic from a machine counts as heard ([a24d965](https://github.com/qoop-si/qoop/commit/a24d965db9fc0347e349f8a867506d83de3af4b6))
* **desktop:** build on macOS: banner shrink, run test clock, file view ([5876df5](https://github.com/qoop-si/qoop/commit/5876df593f612c2759a5f42d44d9d5a7988ff3a6))
* **desktop:** card answers on hold, Undo key, refusals as sentences ([5a82eeb](https://github.com/qoop-si/qoop/commit/5a82eebb60cbcd3b3d19461cb0c601662989d94b))
* **desktop:** dark pairs on the terminal ground, labels on the top glow ([1e20f86](https://github.com/qoop-si/qoop/commit/1e20f860acdea45c6d7b472550843dc3fddbd47b))
* **desktop:** dark quiet text reads 4.5:1 on a hovered card ([affa52a](https://github.com/qoop-si/qoop/commit/affa52a7045d453b241d9aafa9f0401f02a01cb2))
* **desktop:** drop late remote settings, stale on a silent daemon ([8ef55b5](https://github.com/qoop-si/qoop/commit/8ef55b554768172d47e2032fa4b07a8ce48da435))
* **desktop:** focus ring, remote reopen, live theme, stale header ([c1ef4d5](https://github.com/qoop-si/qoop/commit/c1ef4d5143278cb7e77d739e90c1955dda3afb53))
* **desktop:** focus scrolls into view, fields framed, one needs count ([d2cd1c8](https://github.com/qoop-si/qoop/commit/d2cd1c820a584b459114e57de11de556c737390d))
* **desktop:** glyph list for branch letters, contrast test grounds ([78b065e](https://github.com/qoop-si/qoop/commit/78b065e84073b680f31b26658a0ab59e54e25a2c))
* **desktop:** hand-off Undo takes back the hand-off, one hold timer ([1d9c7d4](https://github.com/qoop-si/qoop/commit/1d9c7d467350ab22eac031c01ce2da02e1cc9fde))
* **desktop:** keep a remote from before pets live on its refusals ([470d662](https://github.com/qoop-si/qoop/commit/470d6628c0ed42f881b64112e6f328087cecc1a6))
* **desktop:** name the qass page's chips apart from home's ([a9d96f4](https://github.com/qoop-si/qoop/commit/a9d96f453011096d7b2532703dc4098bd971194d))
* **desktop:** name the schedule report's read type ([59302e8](https://github.com/qoop-si/qoop/commit/59302e8fb359505dbeb4381b947a94a3fc283ba6))
* **desktop:** offer [agents] in New work, wrap minutes, stronger ring ([5303670](https://github.com/qoop-si/qoop/commit/5303670ac653906ef88f43eadb3d55ff734afd68))
* **desktop:** outline the current page, tab, row and run ([04a277c](https://github.com/qoop-si/qoop/commit/04a277ca0b086a9a9425ebc85170b721c51446ee))
* **desktop:** P1 review fixes, fish-safe quoting ([a3d0487](https://github.com/qoop-si/qoop/commit/a3d048707adb598548d0f0aab0266f50f52cbe52))
* **desktop:** pair every control state fill with its label ink ([f3abf12](https://github.com/qoop-si/qoop/commit/f3abf12ba2211c9640819a816c451def1f3f5a8b))
* **desktop:** qass cards in the Inbox count, keyboard-only autoscroll ([352db79](https://github.com/qoop-si/qoop/commit/352db791771426a439abd23caeed244872996249))
* **desktop:** session keys, wheel steps and review fixes ([3b63b3b](https://github.com/qoop-si/qoop/commit/3b63b3b78dbb73eea1abea958f3d67ac4008a41d))
* **desktop:** settings refuse a file the writer cannot write ([b304c16](https://github.com/qoop-si/qoop/commit/b304c16678a571eb2d8e970773114b67e405c312))
* **desktop:** sheets keep drafts and focus, picks and changes marked ([ef12c8b](https://github.com/qoop-si/qoop/commit/ef12c8b6106f400a5450b4436cfe7db11e7d3fd0))
* **desktop:** stale question card, late Sent line, remote settings title ([42ef604](https://github.com/qoop-si/qoop/commit/42ef6049a7dee3e6cc812eb589668dffa8d0fa27))
* **desktop:** take a Focus event with its zoom ([9fa5ae8](https://github.com/qoop-si/qoop/commit/9fa5ae818613cb7f29a76e57a9bf6d4941a0482c))
* **mux:** send a down link's reason beside the space name ([7900ba8](https://github.com/qoop-si/qoop/commit/7900ba834cf5f9c8f5b6d863289c77a7309464df))
* **qass:** decide a start under a lock; one parameter for Start ([b557bea](https://github.com/qoop-si/qoop/commit/b557bea3e4f47f423e2766e26091df59c0812163))
* **qass:** one start of qass at a time; qass page review fixes ([75ab390](https://github.com/qoop-si/qoop/commit/75ab39096f8e64685568ab5b70c44d3c8cb0eaef))
* **qass:** take back a board left alone instead of a second ([2e931bf](https://github.com/qoop-si/qoop/commit/2e931bfccf92774d09118ed70128f4262a0c50a8))
* **release:** notes gain the desktop line on a re-run ([ed3de05](https://github.com/qoop-si/qoop/commit/ed3de05488591e4bb17f1b116c2e94d96a9c196d))
* **runtime:** the writable test's strings compile ([f606600](https://github.com/qoop-si/qoop/commit/f606600bd67bc81b82c885572e1305da4a8f94da))
* **schedule:** a due slot waits for an open run as Run now does ([4b163cf](https://github.com/qoop-si/qoop/commit/4b163cfea4da55d7bda600308b3b8deea78f4d0f))
* **schedule:** Run now waits for any open run, in one transaction ([1f54099](https://github.com/qoop-si/qoop/commit/1f54099eb67ca031755b33ef1802cc8d7fd0639b))
* **schedule:** Run now waits for the last run; edits send only changes ([f17843e](https://github.com/qoop-si/qoop/commit/f17843e44a7327f22b8220449477ad8c68b202e6))
* **tui:** say offline on the palette's line for a down space ([d3e4075](https://github.com/qoop-si/qoop/commit/d3e407565e02ae85941231f2dcbfe1cf6114c723))

## [0.18.1](https://github.com/qoop-si/qoop/compare/v0.18.0...v0.18.1) (2026-11-18)


### Fixed

* **detect:** read a Claude draft as weak idle, not weak working ([aaa1d65](https://github.com/qoop-si/qoop/commit/aaa1d657ea7224f85d85d3c08aaff959895340bd))
* **detect:** read Claude's Linux record mark like the macOS one ([47d1884](https://github.com/qoop-si/qoop/commit/47d1884870c7a28bfdf50bbf14bd50462c9afcd1))

## [0.18.0](https://github.com/qoop-si/qoop/compare/v0.17.1...v0.18.0) (2026-11-17)


### New

* **cli:** move panes, keep agent views and pass selections for plugins ([4ebacae](https://github.com/qoop-si/qoop/commit/4ebacae8022ecb861b76eaecb346d68c62e3d77d))
* **tui:** herdr plugin link handlers and agent views in the sidebar ([3e29747](https://github.com/qoop-si/qoop/commit/3e2974746e64ac73cfcea8479db7b8a8f232bfb4))

## [0.17.1](https://github.com/qoop-si/qoop/compare/v0.17.0...v0.17.1) (2026-11-16)


### Fixed

* **cli:** make herdr plugins behave as they do under herdr ([b91356c](https://github.com/qoop-si/qoop/commit/b91356c83a9edfa54a5daf71b99c3f493cc222ed))
* **qass:** keep a handled same rest quiet in the watcher ([b6a48a9](https://github.com/qoop-si/qoop/commit/b6a48a944283ad90f071bc1b66af9bf895174de5))

## [0.17.0](https://github.com/qoop-si/qoop/compare/v0.16.0...v0.17.0) (2026-11-15)


### New

* **cli:** install herdr plugins with consent pinned to the manifest ([1899611](https://github.com/qoop-si/qoop/commit/189961138200016efc89afe812b399b2840a07ee))
* **cli:** keep herdr plugin metadata tokens ([e5db1e0](https://github.com/qoop-si/qoop/commit/e5db1e02e56b9aa043d01d209a180525be155260))
* **cli:** let herdr plugins act on terminals and open panes ([68bfdb3](https://github.com/qoop-si/qoop/commit/68bfdb3dbf870a9ee51654d5fb55bae800d60916))
* **cli:** serve herdr plugins from a plugin host ([9915a36](https://github.com/qoop-si/qoop/commit/9915a368005e1f0f6015697f67147846d7530bc1))
* **cli:** translate qoop rows into herdr plugin shapes ([14b4cbb](https://github.com/qoop-si/qoop/commit/14b4cbbc59ad8bbddc4feaade2da5a8fe3ab8203))
* **tui:** list plugin actions in the palette and report focus ([394a6c2](https://github.com/qoop-si/qoop/commit/394a6c2e93005d42f40a19beae724853ce193526))


### Fixed

* **cli:** quote typed shell words safely for fish ([0225184](https://github.com/qoop-si/qoop/commit/02251842071ace09b348d7a1c464cfc05ed3a829))
* **tui:** keep a status line notice while the focused tab prints ([08aa8da](https://github.com/qoop-si/qoop/commit/08aa8dab86872aded81f35b7451d7a9f00d6da9f))

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
