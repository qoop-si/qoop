# Quick start

Installed qoop with the line from [Install](#install)? Sign this computer in, so your account
(and your phone) knows it is yours:

```
qoop login
```

Then open your project in qoop:

```
cd my-project
qoop
```

You are in a shell, with the index of terminals on the left. While no agent is running, the
row above the keys says how to start one.

| Do this | Press |
|---|---|
| a new terminal (a tab) | `ctrl-a c` |
| split side by side, or stacked | `ctrl-a v`, `ctrl-a -` |
| move between panes | `ctrl-a H J K L`, or click |
| the next or previous tab | `ctrl-a n`, `ctrl-a p` |
| find anything: a space, an agent, a tab, or start an agent tab | `ctrl-a space` |
| scroll back and copy with the keyboard | `ctrl-a [` |
| an agent's conversation instead of its terminal, and back | `ctrl-a t` |
| every key | `ctrl-a ?` |
| leave, with everything still running | `ctrl-q` |

Start an agent in any terminal (`claude`, `codex`, `gemini`, ...). Its row in the index shows
what it is doing. When it needs you, the row lights up and you get a notification, even if
you are in another tab or another app.

Come back later with `qoop`. The terminals are where you left them.

The prefix is `ctrl-a` unless you set another in [Configuration](#configuration).

## On your phone

Open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the same account. Your
computers show up by themselves, with the agent that is waiting for you first: you can read
it, answer it, or type into any terminal. Notifications need the qoop app on the phone (the
web page cannot send them): turn them on there to be told when an agent needs you.

