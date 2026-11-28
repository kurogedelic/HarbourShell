#!/bin/zsh
# guide_shots.sh: the pictures for docs/GUIDE.md and docs/GUIDE.ja.md (run from the repo root).
#   tools/guide_shots.sh [binary]      headless, mock backend, frozen clock; writes docs/screenshots/guide/{en,ja}-*.png at 3x
BIN=${1:-target/release/harbour-shell}
OUT=docs/screenshots/guide
[[ -x $BIN ]] || { echo "build first: cargo build --release --bin harbour-shell" >&2; exit 2; }
mkdir -p $OUT

shot() { # shot <lang 0|1> <group> <lock style> <script> [lock]
  local L=$1 G=$2 LS=$3 SCRIPT=$4 KIND=$5 P=$([ $1 = 0 ] && echo en || echo ja)
  local S=/tmp/harbour-guide
  rm -rf $S; mkdir -p $S/data $S/t $S/home/.config/harbour $S/home/Documents $S/ime
  echo "{\"harbour_lang\":$L,\"harbour_lock_style\":$LS}" > $S/cfg.json
  env TZ=UTC HOME=$S/home HARBOUR_FROZEN=1 HARBOUR_HEADLESS=1 HARBOUR_MOCK=1 HARBOUR_NO_EVDEV=1 \
      HARBOUR_SCREEN=320x170 HARBOUR_CONFIG=$S/cfg.json HARBOUR_DATA=$S/data HARBOUR_ORDER_FILE=$S/order.txt \
      HARBOUR_IME_DIR=$S/ime HARBOUR_SHOT_DIR=$S/t HARBOUR_SCRIPT="$SCRIPT" \
      ${KIND:+HARBOUR_START_LOCKED=1} timeout 120 $BIN >$S/log 2>&1
  grep -E 'unknown key|no tile|panick' $S/log | head -3
  local f
  for f in $S/t/*.bmp(N); do magick $f -filter point -resize 300% $OUT/$P-$G-${${f:t}%.bmp}.png; done
  rm -rf $S
}

for L in 0 1; do
  shot $L home 0 "wait:400,shot:home,menu,wait:300,shot:menu"
  shot $L list 0 "wait:400,Tab,wait:700,shot:list"
  shot $L settings 0 "goto:Settings,Return,wait:1200,Escape,wait:400,shot:sidebar,Right,wait:400,shot:inside,Left,wait:200,Down,Down,Down,Return,wait:500,shot:network"
  shot $L ime 0 "goto:Text Editor,Return,wait:900,ctrl+Space,wait:300,text:nihongo,wait:300,shot:compose"
  shot $L sheet 0 "goto:Sheet,Return,wait:900,text:12,Return,text:30,Return,text:=A1+A2,Return,wait:300,Up,Up,Up,wait:300,shot:sheet"
  shot $L wifi 0 "goto:Interceptor,Return,wait:1800,Right,wait:1500,shot:wifi"
  shot $L lan 0 "goto:Interceptor,Return,wait:1500,Right,wait:400,Right,wait:900,shot:notice,Return,wait:2500,shot:lan"
  shot $L ping 0 "goto:Interceptor,Return,wait:1500,Right,Right,Right,wait:500,shot:ping"
  shot $L devices 0 "goto:Devices,Return,wait:1500,shot:devices"
  shot $L lock 0 "wait:600,shot:lock" lock
done
ls $OUT | wc -l
