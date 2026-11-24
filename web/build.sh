#!/bin/sh
# Web demo build: Harbour Shell as a page (Emscripten, SDL2 from the Emscripten ports).
#
#   web/build.sh            -> web/dist/ (index.html, harbour-shell.js, harbour_shell.wasm, harbour_shell.data)
#   web/build.sh serve      -> also serves web/dist on http://localhost:8017
#
# Needs: emcc on PATH and `rustup target add wasm32-unknown-emscripten`.
# Apps that need the network, a terminal, a camera or the sound device are left out of the demo,
# and so is the Japanese dictionary (it is never redistributed): kana input only.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/../target-web}
STAGE=$TARGET/web-stage
DIST=$ROOT/web/dist
WEB_APPS="calculator calendar clock files images paint settings sheet snake texteditor timer"

rm -rf "$STAGE" && mkdir -p "$STAGE/apps"
cp -R "$ROOT/assets" "$STAGE/assets"
for a in $WEB_APPS; do cp -R "$ROOT/apps/$a" "$STAGE/apps/$a"; done

LINK="-sUSE_SDL=2 -sUSE_SDL_IMAGE=2 -sSDL2_IMAGE_FORMATS=[\"png\"] -sASYNCIFY -sASYNCIFY_STACK_SIZE=262144 \
-sALLOW_MEMORY_GROWTH=1 -sSTACK_SIZE=8388608 -sENVIRONMENT=web -sEXIT_RUNTIME=0 -lidbfs.js \
-sEXPORTED_RUNTIME_METHODS=[\"ENV\",\"FS\",\"IDBFS\",\"addRunDependency\",\"removeRunDependency\"] \
--pre-js $ROOT/web/pre.js --preload-file $STAGE/assets@/assets --preload-file $STAGE/apps@/apps"
RUSTFLAGS=""
for f in $LINK; do RUSTFLAGS="$RUSTFLAGS -C link-arg=$f"; done
export EMCC_CFLAGS="-sUSE_SDL=2 -sUSE_SDL_IMAGE=2 -sSDL2_IMAGE_FORMATS=[\"png\"]"
export RUSTFLAGS
cd "$ROOT"
touch src/bin/harbour-shell.rs # web/pre.js is linked in; make sure a change to it is picked up
cargo build --release --target wasm32-unknown-emscripten --target-dir "$TARGET" --bin harbour-shell

OUT=$TARGET/wasm32-unknown-emscripten/release
rm -rf "$DIST" && mkdir -p "$DIST"
cp "$ROOT/web/index.html" "$OUT/harbour_shell.wasm" "$OUT/deps/harbour_shell.data" "$DIST/"
# The loader names its data package by the build path; keep only the file name.
DEPS=$(cd "$OUT/deps" && pwd -P)
sed -e "s#$DEPS/##g" -e "s#$OUT/deps/##g" "$OUT/harbour-shell.js" > "$DIST/harbour-shell.js"
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$DIST/"
ls -la "$DIST"

if [ "${1:-}" = serve ]; then
  cd "$DIST" && exec python3 -m http.server 8017
fi
