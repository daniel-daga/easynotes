# The offline gate, on two Android devices

`offline_gate.py` drives launch gate item 2's offline half — issue
[#60](https://github.com/daniel-daga/easynotes/issues/60) — on two Android
devices. It is the scripted form of the seven manual steps:

1. Both devices online, on one shared list, both **Up to date**.
2. Both into airplane mode.
3. A adds an item and ticks one; B edits a different item and ticks it.
4. A is force-stopped and relaunched while still offline.
5. A reconnects and settles, then B reconnects.
6. Both must show every change, with no duplicate, lost or reverted item.
7. The same item is edited on both while offline, and must settle to the same
   value on both.

The devices can be two emulators (as in
[the 2026-10-26 run](../../docs/offline-gate-2026-10-26.md)) or two phones on
`adb`. Emulators are evidence for the offline behaviour; **they are not gate
item 2**, which asks for two physical Android phones. That gap is #63.

## Before the script

The script starts from a list that already exists on both devices. Doing the
account and invite half by hand is deliberate: a recovery phrase needs a
person to write it down. The one-time setup is:

- Two devices or emulators with an accessibility service enabled. Without one
  Flutter publishes no semantics and `uiautomator` sees an opaque canvas;
  `docs/android-on-windows.md` has the commands.
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

Two Flutter semantics quirks shape the script; both were found while building
this:

- A text field that received a **remote** edit can report only its first
  character through semantics until the note is reopened ("bread" read back as
  "b", 2026-10-26). Item text is therefore read after reopening the note from
  the grid, where it is whole.
- Ticked items are hidden under a **"Crossed off items (n)"** button, and the
  grid's card shows only the unticked ones. The script opens that section
  before listing items and their ticks.

Deleting all of an item's text removes the row; if a field is emptied and then
typed into, the app can log `Unhandled Exception: unknown item <id>`. The
script appends to existing text instead of clearing it, which is also what a
person adding a word does.

## Evidence from 2026-10-26

Two Android 17 (API 37) emulators, debug build of `e51bd1d`, one list titled
`Gate3`, a local plain-HTTP relay: all checks passed, including the
force-stop/relaunch and the same-item conflict (`apples red` won over
`apples green` on both devices). The result and its caveats are in
[`docs/offline-gate-2026-10-26.md`](../../docs/offline-gate-2026-10-26.md).
