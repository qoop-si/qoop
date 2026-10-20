# Detach, stop and restore

One daemon per machine holds everything: its spaces, its terminals and their history. Spaces
group the tabs, usually one per project; there are no separately named sessions.

```
qoop                    open it, starting the daemon when there is none
qoop --space api        the interface with that one space in it
qoop ls                 every space, and whether it is awake, asleep or stopped
qoop stop               end everything: the tabs come back on the next open
```

An older `qoop --session NAME` (or `QOOP_SESSION`) is refused, and the message says where that
session's files are and how to use them instead.

## Detach and reattach

`ctrl-q` (or `ctrl-a d`) detaches. The daemon and every program in it keep running. `qoop`
attaches again, from the same terminal or another one.

## Stop and come back

`qoop stop` ends the processes. Running `qoop` again brings back the spaces and tabs, each in
the directory it was last in; a tab that was asleep stays asleep. An agent that reported its
conversation id (through its hook, see [Hooks and skills](#hooks)) is started again on that
same conversation.

## Sleep

A tab or a whole space can sleep and wake as itself, by hand or after it sits idle. See
[Sleep](#sleep).
