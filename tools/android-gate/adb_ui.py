"""Drive a Flutter app on an Android device through `adb` and uiautomator.

Flutter paints to a canvas, so there is no view tree to click. An active
accessibility service makes Flutter publish its semantics tree, and
`uiautomator dump` then returns real nodes with text, content descriptions and
bounds. This module is the small part of that a scripted two-device run needs:
find a node by the words a person would see, tap it, type into it, and read
what is on screen.

`docs/android-release.md` has the traps this works around: a stale dump, a
keyboard that eats the first keystrokes into a newly focused field, and an
invite link whose `#` the device's shell would read as a comment.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass


def _adb() -> str:
    """`adb` from PATH, or from the SDK layout Android Studio installs."""
    if found := os.environ.get("ADB"):
        return found
    if found := shutil.which("adb"):
        return found
    for root in (os.environ.get("ANDROID_SDK_ROOT"), os.environ.get("ANDROID_HOME")):
        if root:
            candidate = os.path.join(root, "platform-tools", "adb")
            if os.path.exists(candidate):
                return candidate
    candidate = os.path.expanduser("~/Library/Android/sdk/platform-tools/adb")
    return candidate if os.path.exists(candidate) else "adb"


ADB = _adb()


def adb(serial: str, *args: str, timeout: float = 60, check: bool = True) -> str:
    """Run an adb command against one device and return its stdout."""
    done = subprocess.run(
        [ADB, "-s", serial, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if check and done.returncode != 0:
        raise RuntimeError(
            f"adb {' '.join(args)} failed ({done.returncode}): "
            f"{done.stderr.strip() or done.stdout.strip()}"
        )
    return done.stdout


@dataclass
class Node:
    text: str
    desc: str
    hint: str
    cls: str
    clickable: bool
    checked: bool
    focused: bool
    bounds: tuple[int, int, int, int]

    @property
    def label(self) -> str:
        return self.text or self.desc

    @property
    def searchable(self) -> str:
        """What `find` matches: a field's label lives in its hint."""
        return self.text or self.desc or self.hint

    @property
    def centre(self) -> tuple[int, int]:
        x1, y1, x2, y2 = self.bounds
        return ((x1 + x2) // 2, (y1 + y2) // 2)

    def __str__(self) -> str:
        return f"{self.cls} {self.label!r} at {self.bounds}"


_BOUNDS = re.compile(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]")


class Device:
    def __init__(self, serial: str, name: str, log=print):
        self.serial = serial
        self.name = name
        self.log = log
        self._dumped_at = 0.0

    def run(self, *args: str, **kw) -> str:
        return adb(self.serial, *args, **kw)

    def shell(self, command: str, **kw) -> str:
        return self.run("shell", command, **kw)

    # --- reading the screen -------------------------------------------------

    def dump(self) -> list[Node]:
        """Every node uiautomator currently reports, in drawing order.

        The dump can lag behind a screen that has already closed, so a caller
        that is about to assert something important should confirm it with a
        second look (`wait_for`) rather than a single read.
        """
        self.shell("uiautomator dump /sdcard/window.xml >/dev/null")
        xml = self.run("exec-out", "cat", "/sdcard/window.xml")
        self._dumped_at = time.monotonic()
        root = ET.fromstring(xml)
        nodes = []
        for element in root.iter("node"):
            match = _BOUNDS.fullmatch(element.get("bounds", ""))
            if not match:
                continue
            nodes.append(
                Node(
                    text=element.get("text", ""),
                    desc=element.get("content-desc", ""),
                    hint=element.get("hint", ""),
                    cls=element.get("class", ""),
                    clickable=element.get("clickable") == "true",
                    checked=element.get("checked") == "true",
                    focused=element.get("focused") == "true",
                    bounds=tuple(int(part) for part in match.groups()),
                )
            )
        return nodes

    def texts(self) -> list[str]:
        return [n.searchable for n in self.dump() if n.searchable]

    def find(
        self,
        pattern: str | re.Pattern[str],
        *,
        exact: bool = False,
        timeout: float = 15,
        clickable: bool | None = None,
    ) -> Node:
        """Wait for a node whose text or description matches [pattern]."""
        regex = re.compile(pattern) if isinstance(pattern, str) else pattern
        if exact:
            regex = re.compile(rf"^(?:{regex.pattern})$")
        deadline = time.monotonic() + timeout
        seen: list[str] = []
        while True:
            nodes = self.dump()
            seen = [n.searchable for n in nodes if n.searchable]
            for node in nodes:
                if not regex.search(node.searchable):
                    continue
                if clickable is True and not node.clickable:
                    continue
                if clickable is False and node.clickable:
                    continue
                return node
            if time.monotonic() >= deadline:
                raise LookupError(
                    f"{self.name}: no node matching {pattern!r} in "
                    f"{_summarise(seen)}"
                )
            time.sleep(0.4)

    def wait_for(
        self,
        pattern: str | re.Pattern[str],
        *,
        timeout: float = 60,
        gone: bool = False,
        exact: bool = False,
    ) -> bool:
        """Wait until a node matching [pattern] is (or is not) on screen."""
        try:
            self.find(pattern, timeout=timeout, exact=exact)
            present = True
        except LookupError:
            present = False
        return present != gone

    def has(
        self,
        pattern: str | re.Pattern[str],
        exact: bool = False,
        timeout: float = 0,
    ) -> bool:
        return self.wait_for(pattern, timeout=timeout, exact=exact)

    # --- acting on it ------------------------------------------------------

    def tap(
        self,
        pattern: str | re.Pattern[str],
        *,
        exact: bool = False,
        timeout: float = 20,
        clickable: bool | None = None,
    ) -> Node:
        node = self.find(pattern, exact=exact, timeout=timeout, clickable=clickable)
        x, y = node.centre
        self.log(f"{self.name}: tap {node.label!r} at ({x}, {y})")
        self.shell(f"input tap {x} {y}")
        return node

    def tap_node(self, node: Node) -> None:
        x, y = node.centre
        self.log(f"{self.name}: tap {node.label!r} at ({x}, {y})")
        self.shell(f"input tap {x} {y}")

    def tap_xy(self, x: int, y: int) -> None:
        self.shell(f"input tap {x} {y}")

    def text(self, value: str, *, enter: bool = False) -> None:
        """Type [value] into whatever has focus.

        Spaces are `%s` because the device shell splits its arguments; the
        whole value is single-quoted inside the remote shell so that `#` in an
        invite link is not read as the start of a comment.
        """
        typed = value.replace(" ", "%s")
        self.shell(f"input text '{typed}'")
        if enter:
            self.shell("input keyevent 66")

    def focused_field(self) -> "Node | None":
        return next(
            (
                node
                for node in self.dump()
                if node.cls.endswith("EditText") and node.focused
            ),
            None,
        )

    def type_all(self, value: str, *, settle: float = 1.5) -> None:
        """Type into the focused field, repairing characters the device drops.

        Under load the emulator loses the last character or two of an
        `input text`, so the field is read back and only the missing tail is
        sent again. Anything else means the field held something other than
        what this is typing, and it is cleared and started over.
        """
        for _ in range(5):
            field = self.focused_field()
            if field is None:
                self.text(value)
            elif field.text == value:
                return
            elif value.startswith(field.text):
                self.text(value[len(field.text) :])
            else:
                self.clear()
                self.text(value)
            time.sleep(settle)
        raise RuntimeError(f"{self.name}: the field never held {value!r}")

    def clear(self) -> None:
        # `input keycombination` is not on every image, and a select-all that
        # silently does nothing appends to the old value instead of replacing
        # it. Moving to the end and deleting past the start always works.
        self.shell("input keyevent 123", timeout=120)  # move to end
        self.shell("input keyevent " + " ".join(["67"] * 30), timeout=120)
        time.sleep(0.3)

    def hide_keyboard(self) -> None:
        shown = self.shell("dumpsys input_method")
        if "mInputShown=true" in shown:
            self.shell("input keyevent 4")
            time.sleep(0.5)

    def back(self) -> None:
        self.shell("input keyevent 4")

    def fill(
        self,
        target: str | re.Pattern[str],
        value: str,
        *,
        timeout: float = 15,
    ) -> None:
        """Replace a field's contents and check the field took all of it.

        Typing into a field that has only just taken focus loses characters on
        a software-rendered emulator, so the result is read back rather than
        trusted. A retry clears whatever did arrive and types again.
        """
        node = self.find(target, timeout=timeout)
        self.tap_xy(*node.centre)
        time.sleep(1.2)
        self.clear()
        self.type_all(value)
        self.hide_keyboard()
        if not self.has(re.escape(value), exact=True, timeout=3):
            raise RuntimeError(f"{self.name}: field never held {value!r}")

    def screenshot(self, path: str) -> None:
        with open(path, "wb") as out:
            subprocess.run(
                [ADB, "-s", self.serial, "exec-out", "screencap", "-p"],
                stdout=out,
                check=True,
                timeout=60,
            )

    # --- network and app lifecycle ----------------------------------------

    def airplane_mode(self, on: bool) -> None:
        state = "enable" if on else "disable"
        self.shell(f"cmd connectivity airplane-mode {state}")
        # The radios take a moment to come down, and a reconnect that starts
        # immediately would be testing the old socket rather than the new one.
        time.sleep(3)
        self.log(f"{self.name}: airplane mode {state}d")

    def reachable(self, host: str) -> bool:
        done = subprocess.run(
            [ADB, "-s", self.serial, "shell", "ping", "-c", "1", "-W", "2", host],
            capture_output=True,
            text=True,
            timeout=20,
        )
        return done.returncode == 0

    def force_stop(self, package: str) -> None:
        self.shell(f"am force-stop {package}")
        time.sleep(1)

    def launch(self, package: str, activity: str = ".MainActivity") -> None:
        self.shell(f"am start -n {package}/{activity}")
        time.sleep(2)

    def checkbox_for(self, item: str) -> Node:
        """The checkbox on the same row as [item]'s text field."""
        row = self.find(item, exact=True)
        boxes = [n for n in self.dump() if n.cls.endswith("CheckBox")]
        if not boxes:
            raise LookupError(f"{self.name}: no checkboxes on screen")
        return min(boxes, key=lambda box: abs(box.centre[1] - row.centre[1]))

    def tick_beside(self, row: Node, on: bool = True) -> None:
        """Tick the checkbox on [row]'s line, for a row whose label is unsure."""
        boxes = [n for n in self.dump() if n.cls.endswith("CheckBox")]
        if not boxes:
            raise LookupError(f"{self.name}: no checkboxes on screen")
        box = min(boxes, key=lambda b: abs(b.centre[1] - row.centre[1]))
        if box.checked == on:
            return
        self.log(f"{self.name}: {'tick' if on else 'untick'} the row at {row.centre}")
        self.tap_xy(*box.centre)
        time.sleep(1)

    def tick(self, item: str, on: bool = True) -> None:
        box = self.checkbox_for(item)
        if box.checked == on:
            self.log(f"{self.name}: {item!r} is already {'ticked' if on else 'clear'}")
            return
        self.log(f"{self.name}: {'tick' if on else 'untick'} {item!r}")
        self.tap_xy(*box.centre)
        time.sleep(1)

    def dismiss_system_dialogs(self) -> None:
        """Clear the one SystemUI ANR that switching accessibility on causes."""
        for _ in range(4):
            try:
                nodes = self.dump()
            except ET.ParseError:
                time.sleep(1)
                continue
            if not any("isn't responding" in n.label for n in nodes):
                return
            wait = next((n for n in nodes if n.label == "Wait"), None)
            if wait is None:
                return
            self.log(f"{self.name}: dismissing system ANR")
            self.tap_node(wait)
            time.sleep(2)


def _summarise(labels: list[str], limit: int = 25) -> str:
    unique = list(dict.fromkeys(label for label in labels if label.strip()))
    shown = ", ".join(repr(label) for label in unique[:limit])
    return f"[{shown}{', …' if len(unique) > limit else ''}]"
