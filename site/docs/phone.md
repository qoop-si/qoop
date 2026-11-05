# Your phone

Open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the account your
computer signed in with (`qoop login`). Your computers show up by themselves, with the agent
that is waiting for you first. You can answer it, type into any terminal, open or close one,
start an agent in a space, and put a space to sleep or wake it. Notifications need the qoop
app on the phone (the web page cannot send them): turn them on there and the phone tells you
when an agent blocks, finishes or fails.

The qoop app on Android: download
[qoop-android-arm64.apk](https://qoop.si/releases/latest/qoop-android-arm64.apk) on the phone
and open it; Android asks once to let your browser install apps. If Play Protect says it has not
seen an app from this developer, tap More details, then Install anyway: qoop is not on Google Play yet. Each new release then shows up
in the app's Settings as Update to qoop and the version: tap it, and Android installs it over
the one you have, keeping your sign-in. The iPhone app is not public yet; use
[qoop.si/app](https://qoop.si/app) there.

A phone can also be paired with one computer without an account: run `qoop pair` there,
scan the code it shows with the qoop app, and type the six digits the phone shows into the
computer.

The phone and the computer talk end to end encrypted through a relay that sees neither the
screens nor the keys, and a notification carries a fixed sentence and four numbers, nothing
from a screen. `qoop login`, `qoop pair` and the phone side come with the prebuilt release;
they are not part of this repository, so a build from source has no phone part.

