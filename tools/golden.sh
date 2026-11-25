#!/bin/zsh
# golden.sh: golden-image checks for the drawing code (run from the repo root).
#   golden.sh shoot <binary> <outdir> [en|ja|lock|desk]   headless, frozen-clock shots of every screen (BMP); a 3rd argument shoots just that part
#   golden.sh compare <dirA> <dirB>     differing pixels per image; exit 1 if any differ
# Two shoots of one binary must compare equal. Shoot with the old and the new renderer to see what
# a drawing change did. The shot list follows tools/design.sh (plus Graph and the lock screens).

shoot() {
  local BIN=$1 OUT=$2 ONLY=$3
  [[ -x $BIN ]] || { echo "not an executable: $BIN" >&2; return 2; }
  [[ -z $ONLY ]] && rm -rf $OUT
  mkdir -p $OUT
  local APPS=(Browser Calculator Calendar Camera Clock Files HarbourTalk Images Infoscope Interceptor Monitor Music Paint Radio Settings Sheet Snake Terminal "Text Editor" Timer "Voice Memo" Yacht Graph)
  local L P i k n j s
  for L in 0 1; do
    P=$([ $L = 0 ] && echo en || echo ja)
    [[ -z $ONLY || $ONLY = $P ]] || continue
    run $BIN $OUT $P $L 320x170 0 "wait:400,shot:00home,menu,wait:300,shot:00home-menu,Escape,wait:200,Tab,wait:700,shot:00home-list,Tab,wait:700"
    i=0
    for n in $APPS; do
      i=$((i+1)); k=$(printf %02d $i)${n// /}
      run $BIN $OUT $P $L 320x170 0 "goto:$n,Return,wait:1600,shot:$k,menu,wait:300,shot:$k-menu"
    done
    # Settings pages, one per sidebar row, then the inside of each.
    s="goto:Settings,Return,wait:1200"
    for j in 0 1 2 3 4 5 6 7 8 9; do s="$s,shot:set$j,Down,wait:350"; done
    run $BIN $OUT $P $L 320x170 0 "$s"
    s="goto:Settings,Return,wait:1200"
    for j in 0 1 2 3 4 5 6 7 8 9; do s="$s,Right,wait:300,shot:setin$j,Left,wait:200,Down,wait:300"; done
    run $BIN $OUT $P $L 320x170 0 "$s"
  done
  # The four lock screen styles (English only).
  for j in 0 1 2 3; do
    [[ -z $ONLY || $ONLY = lock ]] || continue
    run $BIN $OUT en 0 320x170 $j "wait:600,shot:lock$j" lock
  done
  # The desk (800x480).
  if [[ -z $ONLY || $ONLY = desk ]]; then
  run $BIN $OUT desk 0 800x480 0 "wait:400,shot:00home,goto:Tools,Return,wait:900,shot:01folder,Escape,wait:700,goto:Files,Return,wait:1000,shot:02files,click:300:440,wait:200,goto:Calculator,Return,wait:1000,shot:03two,menu,wait:300,shot:04menu,Escape,wait:200,click:300:440,wait:200,goto:Settings,Return,wait:1100,shot:05three,click:790:5,wait:400,shot:06clockmenu,Escape" desk
  run $BIN $OUT desk 0 800x480 0 "wait:300,goto:Calendar,Return,wait:1000,shot:10calendar,hold:Escape:1100,wait:800,goto:Paint,Return,wait:1000,shot:11paint,hold:Escape:1100,wait:800,goto:HarbourTalk,Return,wait:1500,shot:12talk,hold:Escape:1100,wait:800,goto:Yacht,Return,wait:1000,shot:13yacht,hold:Escape:1100,wait:800,goto:Music,Return,wait:1200,shot:14music,hold:Escape:1100,wait:800,goto:Text Editor,Return,wait:1000,shot:15editor" desk
  fi
  echo "$(ls $OUT | wc -l | tr -d ' ') images in $OUT"
}

# run <bin> <outdir> <group> <lang 0|1> <size> <lock style> <script> [lock|desk]
# One headless run in a throwaway home; its shots are copied to <outdir>/<group>-<name>.bmp.
run() {
  local BIN=$1 OUT=$2 G=$3 L=$4 SZ=$5 LS=$6 SCRIPT=$7 KIND=$8
  # The path shows up on screen (Files > Info), so it is the same every time: one run at a time.
  local S=${GOLDEN_TMP:-/tmp/harbour-golden}
  rm -rf $S; mkdir -p $S
  mkdir -p $S/data $S/t $S/home/.config/harbour $S/home/Music $S/home/Pictures $S/home/Documents
  echo "{\"harbour_lang\":$L,\"harbour_lock_style\":$LS}" > $S/cfg.json
  [[ $KIND = desk ]] && echo '{"version":1,"folders":[{"name":"Tools","items":["timer","snake","radio"]}]}' > $S/folders.json
  printf 'Harbour notes\nline two with some longer text to see wrapping\n日本語のテキスト\n' > $S/home/Documents/notes.txt
  # Files shows modification times: give everything made so far one fixed date.
  find $S -exec touch -t 202601010000 {} +
  # HARBOUR_FROZEN pins the clock and the mock values; TZ keeps the date the same on any machine.
  env TZ=UTC HOME=$S/home HARBOUR_FROZEN=1 HARBOUR_HEADLESS=1 HARBOUR_MOCK=1 HARBOUR_NO_EVDEV=1 \
      HARBOUR_SCREEN=$SZ HARBOUR_CONFIG=$S/cfg.json HARBOUR_DATA=$S/data HARBOUR_ORDER_FILE=$S/order.txt \
      HARBOUR_AGENT_DIR=$S/agent HARBOUR_IME_DIR=$S/ime HARBOUR_SHOT_DIR=$S/t HARBOUR_SCRIPT="$SCRIPT" \
      ${KIND:+HARBOUR_START_LOCKED=$([[ $KIND = lock ]] && echo 1 || echo 0)} timeout 120 $BIN >$S/log 2>&1
  grep -E 'unknown key|no tile|panick' $S/log | head -3
  local f
  for f in $S/t/*.bmp(N); do cp $f $OUT/$G-${f:t}; done
  rm -rf $S
}

compare() {
  local A=$1 B=$2 bad=0 n=0 f d
  [[ -d $A && -d $B ]] || { echo "usage: golden.sh compare <dirA> <dirB>" >&2; return 2; }
  for f in $(cd $A; ls *.bmp; cd $B; ls *.bmp) ; do echo $f; done | sort -u | while read f; do
    if [[ ! -f $A/$f || ! -f $B/$f ]]; then
      echo "MISSING $f (only in $([[ -f $A/$f ]] && echo A || echo B))"; echo x >> $A/.bad.$$; continue
    fi
    d=$(magick compare -metric AE $A/$f $B/$f null: 2>&1)
    d=${d%% *}   # newer ImageMagick adds "(fraction)" after the count
    [[ $d = 0 ]] || { echo "DIFF $f: $d pixels"; echo x >> $A/.bad.$$; }
    echo y >> $A/.n.$$
  done
  n=$(wc -l < $A/.n.$$ 2>/dev/null | tr -d ' '); [[ -f $A/.bad.$$ ]] && bad=$(wc -l < $A/.bad.$$ | tr -d ' ')
  rm -f $A/.bad.$$ $A/.n.$$
  echo "compared ${n:-0} images, $bad differ"
  [[ $bad = 0 ]]
}

case $1 in
  shoot)   shift; shoot "$@" ;;
  compare) shift; compare "$@" ;;
  *) echo "usage: golden.sh shoot <binary> <outdir> | compare <dirA> <dirB>" >&2; exit 2 ;;
esac
