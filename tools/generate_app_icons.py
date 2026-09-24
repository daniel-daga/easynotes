"""Regenerate platform icons from the approved logo. Requires Pillow."""
import json
from pathlib import Path

from PIL import Image

APP = Path(__file__).resolve().parents[1] / "app"
source = Image.open(APP / "assets/branding/easynotes-logo-v1.png").convert("RGBA")
outputs = []


def save(relative, size, opaque=False):
    icon = source.resize((size, size), Image.Resampling.LANCZOS)
    if opaque:
        background = Image.new("RGBA", icon.size, "#FFB300")
        icon = Image.alpha_composite(background, icon).convert("RGB")
    path = APP / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    icon.save(path)
    outputs.append((path, size, opaque))


for density, size in {"mdpi": 48, "hdpi": 72, "xhdpi": 96,
                      "xxhdpi": 144, "xxxhdpi": 192}.items():
    save(f"android/app/src/main/res/mipmap-{density}/ic_launcher.png", size)

catalog = APP / "ios/Runner/Assets.xcassets/AppIcon.appiconset"
for entry in json.loads((catalog / "Contents.json").read_text())["images"]:
    size = round(float(entry["size"].split("x")[0]) * float(entry["scale"][:-1]))
    save(f"ios/Runner/Assets.xcassets/AppIcon.appiconset/{entry['filename']}", size, True)

for size in (192, 512):
    save(f"web/icons/Icon-{size}.png", size)
    save(f"web/icons/Icon-maskable-{size}.png", size, True)
save("web/favicon.png", 32)
save("linux/runner/resources/easynotes.png", 256)

for path, size, opaque in outputs:
    with Image.open(path) as icon:
        assert icon.size == (size, size), path
        assert not opaque or icon.mode == "RGB", path
print(f"Generated and verified {len(set(p for p, _, _ in outputs))} app icons.")
