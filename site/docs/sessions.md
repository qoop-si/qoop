# Sessions and restore

A session is a daemon and everything it holds: its spaces, its terminals and their history.
Sessions are named, and they do not see each other.

```
qoop --session work     open one, or make it
qoop ls                 what there is, and what is running
qoop stop work          end it: the tabs come back on the next open
```

`--session` wins over `QOOP_SESSION`, which wins over the session last opened, which wins
over `default`: a bare `qoop` reopens whatever you opened last.

## Detach and reattach

`ctrl-q` (or `ctrl-a d`) detaches. The daemon and every program in it keep running. `qoop`
attaches again, from the same terminal or another one.

## Stop and come back

`qoop stop` ends the processes. Opening the session again brings back its spaces and tabs,
each in the directory it was last in. An agent that reported its conversation id (through
its hook, see [Hooks and skills](#hooks)) is started again on that same conversation.

## Idle terminals

Set `QOOP_IDLE_TIMEOUT_MINUTES` (for example `30`) in the environment the daemon starts
from, and terminals idle for that long with nothing running in the background are closed
the same way as closing the tab. They stay listed as `sleeping` until dismissed, and the
agent's conversation is still in the agent's own store. An agent waiting on another
terminal stays awake while that wait is alive.
