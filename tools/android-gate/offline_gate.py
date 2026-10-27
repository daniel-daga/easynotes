#!/usr/bin/env python3
"""Run the offline half of the two-device sharing gate on two Android devices.

The manual steps are the offline half of the sharing gate: two devices on one
shared list, both offline, each making a different edit, one force-stopped and
relaunched while still offline, then reconnected one at a time, and finally the
same item edited on both while offline. The list has to converge with every
change, no duplicates, and the same value for the conflicted item.

This drives both devices through `adb` and uiautomator (`adb_ui.py`), against
a relay the caller has already started. It assumes the two devices are
already signed in to the shared list named by `--title`; making accounts,
sharing and joining are one-time setup by hand, because a recovery phrase
needs a person to write it down.

    python3 tools/android-gate/offline_gate.py \
        --a emulator-5554 --b emulator-5556 --title Gate --host 10.0.2.2

The devices must have an accessibility service enabled, as
`docs/android-release.md` explains: without one Flutter publishes no
semantics and uiautomator sees a single opaque canvas.

Everything asserted here is read from the list's card on the grid, not from
the note's text fields. A field that received a remote edit can report only
its first character through semantics for a while (observed on 2026-10-26:
"bread" read back as "b"); the card draws the whole row and is what a person
would check.

Evidence (screenshots, metadata, a log) goes to
`target/android-gate/<timestamp>/`, which is not committed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from adb_ui import Device  # noqa: E402

# The row the two devices conflict over in the last phase.
CONFLICT_ITEM = "apples"
CONFLICT_A = "apples red"
CONFLICT_B = "apples green"


class Run:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        stamp = time.strftime("%Y-%m-%d-%H%M%S")
        self.out = os.path.join("target", "android-gate", stamp)
        os.makedirs(self.out, exist_ok=True)
        self.log_path = os.path.join(self.out, "run.log")
        self.failures: list[str] = []
        self.a = Device(args.a, "A", log=self.log)
        self.b = Device(args.b, "B", log=self.log)
        self.step_number = 0
        self.title = re.escape(args.title)

    def log(self, line: str) -> None:
        print(line, flush=True)
        with open(self.log_path, "a") as log:
            log.write(line + "\n")

    def step(self, what: str) -> None:
        self.step_number += 1
        self.log(f"\n-- {self.step_number}. {what} ({time.strftime('%H:%M:%S')})")

    def check(self, ok: bool, what: str) -> bool:
        self.log(f"{'ok  ' if ok else 'FAIL'}  {what}")
        if not ok:
            self.failures.append(what)
        return ok

    def shot(self, device: Device, name: str) -> None:
        path = os.path.join(self.out, f"{device.name}-{self.step_number:02d}-{name}.png")
        device.screenshot(path)
        self.log(f"     screenshot {path}")

    # --- screen helpers -----------------------------------------------------

    def on_note(self, d: Device) -> bool:
        return d.has("^Checklist item$", exact=True, timeout=3)

    def open_note(self, d: Device) -> None:
        if self.on_note(d):
            return
        d.tap(rf"^{self.title}\n", timeout=30)
        d.find("^Checklist item$", exact=True, timeout=30)
        time.sleep(2)

    def to_grid(self, d: Device) -> None:
        if self.on_note(d):
            d.hide_keyboard()
            d.tap("^Back$", exact=True)
            d.find(r"^Open navigation menu$", timeout=30)
        time.sleep(1)

    def status(self, d: Device) -> str:
        """The drawer's one-line sync status, read without leaving the grid."""
        self.to_grid(d)
        d.tap("^Open navigation menu$", exact=True)
        value = d.find(r"^Sync & devices", timeout=20).label
        d.tap(r"^Notes", timeout=20)
        time.sleep(1)
        return value

    def wait_status(self, d: Device, wanted: str, timeout: float = 180) -> str:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            last = self.status(d)
            self.log(f"     {d.name} status: {last!r}")
            if wanted in last:
                return last
            time.sleep(5)
        return last

    def card(self, d: Device, timeout: float = 90) -> str:
        """The titled list's card as the grid shows it."""
        self.to_grid(d)
        deadline = time.monotonic() + timeout
        seen = ""
        while time.monotonic() < deadline:
            for node in d.dump():
                if re.match(rf"^{self.title}\n", node.label):
                    return node.label
            time.sleep(2)
        raise TimeoutError(f"{d.name}: no card titled {self.args.title!r} ({seen!r})")

    def note_rows(self, d: Device) -> list[tuple[str, bool]]:
        """Every item with its tick, including the crossed-off section.

        The note hides ticked items under a "Crossed off items (n)" button
        until it is opened, and a card on the grid does not show them at all.
        A field that received a remote edit can also report only its first
        character while the note was open, so this always reopens the note
        from the grid before reading it.
        """
        self.to_grid(d)
        self.open_note(d)
        try:
            section = d.find(r"^Crossed off items \(\d+\)$", timeout=2)
        except LookupError:
            section = None
        if section:
            boxes = [n for n in d.dump() if n.cls.endswith("CheckBox")]
            if not any(box.checked for box in boxes):
                d.tap_xy(*section.centre)
                time.sleep(1)
        nodes = d.dump()
        boxes = [n for n in nodes if n.cls.endswith("CheckBox")]
        rows = []
        for node in nodes:
            if not node.cls.endswith("EditText") or not node.text:
                continue
            if node.text == self.args.title:
                continue
            box = min(
                boxes,
                key=lambda b: abs(b.centre[1] - node.centre[1]),
                default=None,
            )
            rows.append((node.text, bool(box and box.checked)))
        return sorted(rows)

    def begin_offline(self, d: Device) -> None:
        d.airplane_mode(True)
        if d.reachable(self.args.host):
            raise RuntimeError(f"{d.name}: still reaches {self.args.host} in airplane mode")

    def end_offline(self, d: Device) -> None:
        d.airplane_mode(False)
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            if d.reachable(self.args.host):
                return
            time.sleep(2)
        raise RuntimeError(f"{d.name}: {self.args.host} still unreachable after airplane mode off")

    def append_to_row(self, d: Device, starts_with: str, suffix: str):
        """Tap an item's text and append to it, wherever the cursor lands.

        The row's text field may report only its first character through
        semantics after a remote edit, so the row is found by the prefix it
        does show and the cursor is moved to the end before typing.
        """
        row = d.find(rf"^{starts_with}", timeout=20)
        d.tap_xy(*row.centre)
        time.sleep(1.2)
        d.shell("input keyevent 123")  # move to end
        d.text(suffix)
        time.sleep(1)
        d.hide_keyboard()
        return row

    # --- the run ------------------------------------------------------------

    def execute(self) -> int:
        a, b = self.a, self.b
        for d in (a, b):
            d.dismiss_system_dialogs()
        self.metadata()

        self.step("both devices online, on the same list, both up to date")
        self.open_note(a)
        self.open_note(b)
        self.check("Up to date" in self.wait_status(a, "Up to date"), "A is up to date")
        self.check("Up to date" in self.wait_status(b, "Up to date"), "B is up to date")
        start_a = self.note_rows(a)
        start_b = self.note_rows(b)
        self.check(start_a == start_b, f"both start on the same list: {start_a}")
        self.check(
            start_a == [("bread", False), ("milk", False)],
            f"two items, nothing ticked: {start_a}",
        )
        self.shot(a, "online")
        self.shot(b, "online")

        self.step("both go into airplane mode")
        self.begin_offline(a)
        self.begin_offline(b)

        self.step("A adds an item and ticks one; B edits another and ticks it")
        a.tap("^Checklist item$", exact=True)
        time.sleep(1.5)
        a.type_all("apples")
        a.hide_keyboard()
        a.tick("milk")
        # Append rather than clear: emptying a row removes its item, and the
        # field left holding the removed item throws `unknown item` (seen on
        # 2026-10-26). A person adding a word does not empty the row either.
        row = self.append_to_row(b, "b", " rolls")
        b.tick_beside(row)
        time.sleep(2)
        apart_a = self.note_rows(a)
        apart_b = self.note_rows(b)
        self.check(
            apart_a == [("apples", False), ("bread", False), ("milk", True)],
            f"A has its own offline edit and tick, and not B's: {apart_a}",
        )
        self.check(
            apart_b == [("bread rolls", True), ("milk", False)],
            f"B has its own offline edit and tick, and not A's: {apart_b}",
        )
        self.check(
            self.card(a).endswith("1 of 3 done"),
            f"A's card counts one tick of three: {self.card(a)!r}",
        )
        self.check(
            self.card(b).endswith("1 of 2 done"),
            f"B's card counts one tick of its two: {self.card(b)!r}",
        )
        self.open_note(a)
        self.open_note(b)
        self.shot(a, "offline-edits")
        self.shot(b, "offline-edits")

        self.step("A is force-stopped and relaunched while still offline")
        a.force_stop("eu.fraisl.easynotes")
        a.launch("eu.fraisl.easynotes")
        time.sleep(5)
        survived = self.note_rows(a)
        self.check(
            survived == [("apples", False), ("bread", False), ("milk", True)],
            f"A's offline list survived the relaunch: {survived}",
        )
        self.check(
            self.card(a).endswith("1 of 3 done"),
            f"A's tick is still counted: {self.card(a)!r}",
        )
        self.shot(a, "relaunched-offline")

        self.step("A reconnects, settles, then B reconnects")
        self.end_offline(a)
        self.check(
            "Up to date" in self.wait_status(a, "Up to date"),
            "A is back up to date after reconnecting",
        )
        self.end_offline(b)
        self.check(
            "Up to date" in self.wait_status(b, "Up to date"),
            "B is back up to date after reconnecting",
        )

        self.step("both devices show every change, with no duplicates or losses")
        want = [("apples", False), ("bread rolls", True), ("milk", True)]
        deadline = time.monotonic() + 150
        while True:
            met_a = self.note_rows(a)
            met_b = self.note_rows(b)
            if met_a == met_b == want or time.monotonic() > deadline:
                break
            time.sleep(5)
        self.check(met_a == want, f"A has every change: {met_a}")
        self.check(met_b == want, f"B has every change: {met_b}")
        self.check(met_a == met_b, "both devices show the same list")
        self.check(len(met_a) == 3, f"no duplicated rows: {met_a}")
        self.check(
            self.card(a).endswith("2 of 3 done"),
            f"both ticks are counted: {self.card(a)!r}",
        )
        self.open_note(a)
        self.open_note(b)
        self.shot(a, "converged")
        self.shot(b, "converged")

        self.step("both offline again; the same item is edited on both")
        self.begin_offline(a)
        self.begin_offline(b)
        self.append_to_row(a, CONFLICT_ITEM, " red")
        self.append_to_row(b, CONFLICT_ITEM[0], " green")
        side_a = self.note_rows(a)
        side_b = self.note_rows(b)
        self.check(
            side_a == [(CONFLICT_A, False), ("bread rolls", True), ("milk", True)],
            f"A's side of the conflict: {side_a}",
        )
        self.check(
            side_b == [(CONFLICT_B, False), ("bread rolls", True), ("milk", True)],
            f"B's side of the conflict: {side_b}",
        )
        self.shot(a, "conflict")
        self.shot(b, "conflict")

        self.step("A reconnects, settles, then B; the conflict settles the same way")
        self.end_offline(a)
        self.check(
            "Up to date" in self.wait_status(a, "Up to date"),
            "A is back up to date after the conflict",
        )
        self.end_offline(b)
        self.check(
            "Up to date" in self.wait_status(b, "Up to date"),
            "B is back up to date after the conflict",
        )
        deadline = time.monotonic() + 150
        while True:
            final_a = self.note_rows(a)
            final_b = self.note_rows(b)
            named = {text for text, _ in final_a}
            settled = {CONFLICT_A, CONFLICT_B} & named
            if (
                final_a == final_b
                and len(settled) == 1
                and len(final_a) == 3
            ) or time.monotonic() > deadline:
                break
            time.sleep(5)
        self.check(final_a == final_b, f"the conflict settled the same on both: {final_a}")
        self.check(len(settled) == 1, f"exactly one side of the conflict won: {settled}")
        self.check(len(final_a) == 3, f"still three items: {final_a}")
        self.check(
            ("milk", True) in final_a and ("bread rolls", True) in final_a,
            f"both ticks survived: {final_a}",
        )
        self.check(
            self.card(a).endswith("2 of 3 done"),
            f"both ticks are still counted: {self.card(a)!r}",
        )
        self.shot(a, "conflict-settled")
        self.shot(b, "conflict-settled")

        self.log("")
        if self.failures:
            self.log(f"{len(self.failures)} check(s) failed")
            for failure in self.failures:
                self.log(f"  - {failure}")
        else:
            self.log("all checks passed")
        self.log(f"evidence: {self.out}")
        return 1 if self.failures else 0

    def metadata(self) -> None:
        """What the run doc has to record: device, OS, build, relay."""
        info = {"relay_host": self.args.host, "git": git_head()}
        apk = os.path.join(
            "app", "build", "app", "outputs", "flutter-apk", "app-debug.apk"
        )
        if os.path.exists(apk):
            info["apk_sha256"] = sha256(apk)
        for d in (self.a, self.b):
            info[d.name] = {
                key: d.shell(f"getprop {key}").strip()
                for key in (
                    "ro.product.model",
                    "ro.build.version.release",
                    "ro.build.version.sdk",
                    "ro.build.fingerprint",
                    "ro.build.display.id",
                )
            }
        with open(os.path.join(self.out, "metadata.json"), "w") as out:
            json.dump(info, out, indent=2)
        self.log(json.dumps(info, indent=2))


def git_head() -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"], capture_output=True, text=True
    ).stdout.strip()


def sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--a", default="emulator-5554")
    parser.add_argument("--b", default="emulator-5556")
    parser.add_argument("--title", default="Gate", help="the shared list's title")
    parser.add_argument(
        "--host",
        default="10.0.2.2",
        help="the relay's address as the devices see it, for the reachability check",
    )
    return Run(parser.parse_args()).execute()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (LookupError, TimeoutError, RuntimeError) as error:
        print(f"\nstopped: {error}", file=sys.stderr)
        raise SystemExit(2)
