# Your phone

Open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the account your
computer signed in with (`qoop login`). Your computers show up by themselves, with the agent
that is waiting for you first. You can answer it, type into any terminal, open or close one,
start an agent in a space, and put a space to sleep or wake it. Notifications need the qoop
app on the phone (the web page cannot send them): turn them on there and the phone tells you
when an agent blocks, finishes or fails.

A phone can also be paired with one computer without an account: run `qoop pair` there,
scan the code it shows with the qoop app, and type the six digits the phone shows into the
computer.

The phone and the computer talk end to end encrypted through a relay that sees neither the
screens nor the keys, and a notification carries a fixed sentence and four numbers, nothing
from a screen. `qoop login`, `qoop pair` and the phone side come with the prebuilt release;
they are not part of this repository, so a build from source has no phone part.

