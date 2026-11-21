# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow>=11"]
# ///
"""Copy the docs' screenshots from the headless UI tests' output.

`cargo test -p rho-ui` writes every screenshot to target/ui-shots/; the docs
use the few named in SHOTS, as WebP in docs/public/shots/. Run it after a UI
change the docs show:

    cargo test -j 8 -p rho-ui ui_tests && uv run docs/shots.py
"""

from pathlib import Path

from PIL import Image

SHOTS = [
    "diff-changes",
    "machines-listed",
    "pull-request-pr",
    "settings-open",
    "surface-gallery-inputs-dark",
    "terminal-tabs-split",
    "turn-approval",
    "work-steps",
    "worktree-chosen",
]

root = Path(__file__).resolve().parent.parent
source = root / "target" / "ui-shots"
dest = root / "docs" / "public" / "shots"
dest.mkdir(parents=True, exist_ok=True)
for name in SHOTS:
    image = Image.open(source / f"{name}.png").convert("RGB")
    image.thumbnail((1600, 1600))
    image.save(dest / f"{name}.webp", quality=85, method=6)
    print(f"{name}.webp {image.size[0]}x{image.size[1]}")
