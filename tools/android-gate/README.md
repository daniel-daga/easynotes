# Offline sync checks on two Android devices

`offline_gate.py` drives the offline half of the two-device sharing gate on two
Android devices. It is the scripted form of the seven manual steps:

1. Both devices online, on one shared list, both **Up to date**.
2. Both into airplane mode.
3. A adds an item and ticks one; B edits a different item and ticks it.
4. A is force-stopped and relaunched while still offline.
5. A reconnects and settles, then B reconnects.
6. Both must show every change, with no duplicate, lost or reverted item.
7. The same item is edited on both while offline, and must settle to the same
   value on both.

The devices can be two emulators or two phones connected through `adb`.
Emulator checks do not replace validation on physical devices with a signed
release build and a TLS relay. See the
[Android release checklist](../../docs/android-release.md#validate-before-publishing).

## Before the script

The script starts from a list that already exists on both devices. Doing the
account and invite half by hand is deliberate: a recovery phrase needs a
person to write it down. The one-time setup is:

- Two devices or emulators with an accessibility service enabled. Without one
  Flutter may expose an opaque canvas to `uiautomator`. In Android Settings,
  open **Accessibility** and enable an installed service such as TalkBack on
  each test device; turn it off afterwards if it was enabled only for this run.
- A relay both devices can reach, and both devices pointed at it in Relay
  settings. A debug build may use plain `http://`; a release build may not.
  On an emulator the host is `10.0.2.2`.
- An account on each device, a checklist on A, and A's invite link opened on
  B. The list's title is what the script follows, so give it a distinct one.
- The list's starting state: exactly two unticked items, `milk` and `bread`.
  That is what the checks expect; rename them in the script if you use
  different ones.

Then, from the repository root, with the relay already running:

```sh
python3 tools/android-gate/offline_gate.py \
    --a emulator-5554 --b emulator-5556 --title Gate --host 10.0.2.2
```

`--title` is the shared list's title; `--host` is the relay as the devices see
it, used to prove airplane mode really cut the connection. `ADB` can name an
`adb` outside `PATH`.

The run writes screenshots, `metadata.json` (device model, Android release and
API level, build fingerprint, APK hash, git commit) and `run.log` under
`target/android-gate/<timestamp>/`. It prints `ok`/`FAIL` per check and exits
non-zero if any check fails.

## Why it reads the card and reopens the note

The script accounts for these UI/semantics behaviours:

- Remote field edits have previously exposed incomplete text through semantics.
  The script reopens the note from the grid before checking text, so it does
  not independently verify that an already-open editor updates correctly.
- Ticked items are hidden under a **"Crossed off items (n)"** button, and the
  grid's card shows only the unticked ones. The script opens that section
  before listing items and their ticks.

The script appends to existing text rather than clearing and replacing it.
It therefore does not cover deleting the last character or continuing to edit
an emptied row; test those interactions separately. Historical failures in
that path are not evidence that the current build still has the same bug.
