#!/bin/sh
# Builds rho-ui's web build into target/web, which rho-ui-serve serves:
# the wasm and its JS glue (wasm-bindgen, from mise.toml), the page
# (crates/rho-ui/web), GPUI Kit's icons, which the page fetches as each is
# drawn, and the fonts it loads (web.rs), downloaded once into
# target/web-fonts and checked against their SHA-256.
#
#   scripts/build-web.sh          optimized, as you'd use it
#   scripts/build-web.sh --dev    the dev profile: quicker to build
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

profile=release
cargo_profile=--release
# The wasm's function names, 10 of its 42 MB in a release build, only name
# a stack trace's frames: kept for --dev.
bindgen_names=--remove-name-section
case "${1:-}" in
  # ghostty's Zig, built for Debug, logs through a JS import (`env.log`)
  # the page doesn't have; its other modes log nothing. ReleaseSafe keeps
  # its checks.
  --dev) profile=debug cargo_profile= bindgen_names= LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseSafe
    export LIBGHOSTTY_VT_SYS_OPTIMIZE ;;
  "") ;;
  *) echo "usage: scripts/build-web.sh [--dev]" >&2; exit 2 ;;
esac

# wasm_thread, under GPUI's web platform, names a nightly feature even in a
# single-threaded build; this lets that one crate build on stable.
RUSTC_BOOTSTRAP=wasm_thread cargo build $cargo_profile -j 8 \
  --target wasm32-unknown-unknown -p rho-ui

out=target/web
rm -rf "$out"
mkdir -p "$out/fonts" "$out/assets"
wasm-bindgen --target web --no-typescript $bindgen_names --out-dir "$out" --out-name rho-ui \
  "target/wasm32-unknown-unknown/$profile/rho-ui.wasm"
cp crates/rho-ui/web/index.html "$out/"

# GPUI Kit's icons, from the gpui-kit-assets the lock file names.
icons=$(cargo metadata --format-version 1 --locked |
  python3 -c 'import json, sys, os
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "gpui-kit-assets":
        print(os.path.join(os.path.dirname(p["manifest_path"]), "assets", "icons"))')
cp -r "$icons" "$out/assets/icons"

# Noto Sans (notofonts, v2.015) and JetBrains Mono (v2.304), both OFL.
noto=https://raw.githubusercontent.com/notofonts/notofonts.github.io/28b15b4b43b7bed62b5cf6e6b0b5ff5846270535/fonts/NotoSans/unhinted/ttf
mono=https://raw.githubusercontent.com/JetBrains/JetBrainsMono/v2.304/fonts/ttf
fonts=target/web-fonts
mkdir -p "$fonts"
while read -r sum url; do
  file="$fonts/$(basename "$url")"
  [ -f "$file" ] || curl -sSLf -o "$file" "$url"
  echo "$sum  $file" | sha256sum -c --quiet || { rm -f "$file"; exit 1; }
  cp "$file" "$out/fonts/"
done <<EOF
f3961a9cde016d41a4879aecda1474d3a36d6bf54fa0e4643de029cc2248b0e8 $noto/NotoSans-Regular.ttf
87cb2d84472a7d66da659ee47b6cdb9552326e8c128245231f191b6ac72529d9 $noto/NotoSans-Bold.ttf
678288f868807d4d64a6f3b51466871d117d915780381ce9d0ed4b3bcbd06d37 $noto/NotoSans-Italic.ttf
3d367743f371f28671d2764e911a53d7c20ec9b6aa8791d059e7090389fc52a5 $noto/NotoSans-BoldItalic.ttf
a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f $mono/JetBrainsMono-Regular.ttf
5590990c82e097397517f275f430af4546e1c45cff408bde4255dad142479dcb $mono/JetBrainsMono-Bold.ttf
9d0a1f7a708e6af183f1193b7e81d40da294f5c67682c085d8401c60aac8ded4 $mono/JetBrainsMono-Italic.ttf
EOF

echo "built $out; serve it with: cargo run -p rho-ui-serve"
