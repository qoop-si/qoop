# Your phone

Open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the account your
computer signed in with (`qoop login`). Your computers show up by themselves, with the agent
that is waiting for you first. You can answer it, type into any terminal, open or close one,
start an agent in a space, and put a space to sleep or wake it. A phone on its own shows
nothing: qoop runs on your computer, and the phone shows that computer's agents.

## Android

Download [qoop-android-arm64.apk](https://qoop.si/releases/latest/qoop-android-arm64.apk)
(about 56 MB) on the phone and open it; Android asks once to let your browser install apps. If
Play Protect says it has not seen an app from this developer, tap More details, then Install
anyway: qoop is not on Google Play yet, it is coming soon.

The app sends notifications: turn them on there and the phone tells you when an agent blocks,
finishes or fails. Each new release then shows up in the app's Settings as Update to qoop and
the version: tap it, and the app downloads it and installs it over itself, keeping your sign-in.
The first time, Android asks you to let qoop install apps; allow it, go back, and tap Update
again.

## iPhone

The iPhone app is not out yet; the App Store app is coming soon. Until then, use
[qoop.si/app](https://qoop.si/app) in the browser. It shows the same agents and lets you answer
them, but a browser cannot send push notifications, so nothing tells you an agent is waiting
until you open the page.

## Pairing without an account

A phone can also be paired with one computer without an account: run `qoop pair` there, scan
the code it shows with the qoop app, and type the six digits the phone shows into the computer.
This needs the Android app for now: a browser cannot pair by code.

## Privacy

The phone and the computer talk end to end encrypted through a relay that sees neither the
screens nor the keys, and a notification carries a fixed sentence and four numbers, nothing
from a screen.
