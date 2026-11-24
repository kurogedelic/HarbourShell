# Harbour Shell for Cardputer Zero

A replacement home screen for the M5Stack Cardputer Zero (Raspberry Pi CM0, Debian 13, 320x170 LCD).
It lists and runs the same `.desktop` apps as the official APPLaunch, rebuilds the device settings as a
1-bit, dithered control panel, and adds built-in apps. The official launcher stays installed and one command
switches back. The look is described in [DESIGN.md](DESIGN.md).

Binary: `harbour-shell` (Rust + SDL2). Not part of the web build.

## Install and switch (on the device)

```sh
# 1. user-level install, no sudo (what this repo's tools do)
colima start                                              # Docker must be running, or the build fails
./tools_build_arm64.sh && ./tools_make_bundle.sh          # on the Mac
tar cf - -C dist/bundle . | ssh pi@<device> 'mkdir -p ~/harbour-bundle && tar xf - -C ~/harbour-bundle && sh ~/harbour-bundle/install_user.sh ~/harbour-bundle'
ssh pi@<device> 'systemctl --user restart harbour-shell.service'   # `harbourctl try` does not restart a running Harbour

# 2. switch
harbourctl try        # run Harbour now; next boot still uses the official launcher
harbourctl official   # back to the official launcher now
harbourctl enable     # Harbour at boot        (undo: harbourctl disable)
harbourctl status

# 3. once, with sudo, if Harbour is the boot launcher
sudo sh ~/harbour-bundle/install_system.sh
```

Step 3 matters: the boot splash (`fb_load --keep-open`) holds the LCD until the official APPLaunch or LaunchWizard
starts, so with Harbour alone the logo stays in front of Harbour. `install_system.sh` adds
`harbour-splash-release.service`, which stops `fb_load` as soon as Harbour is running.
The same script also gives the next boot Harbour's own picture (`boot/splash.bmp`, made by `src/bin/boot_splash.rs` from `assets/images/splash.png` plus the version and "for M5Stack Cardputer Zero"): it adds a drop-in `fb_load.service.d/harbour.conf` that passes the BMP to `fb_load`. `sudo sh install_system.sh uninstall-splash` brings the built-in M5 logo back.

System-wide package: `./tools_build_arm64.sh && packaging/build_deb.sh` -> `dist/arm64/harbour-shell_<ver>_arm64.deb`
(`sudo dpkg -i`; it installs the unit and `harbourctl` but enables nothing).

### Safety net
- `harbour-shell.service` has `Conflicts=APPLaunch.service`: starting one launcher stops the other.
- If Harbour fails 3 times within 30 s (or does not stop within `TimeoutStopSec=3`), systemd starts the official launcher (`OnFailure=`).
  Harbour handles SIGTERM, so a normal restart is clean.
- Settings -> System -> Official launcher switches back from the UI.
- A failed boot is always recoverable: the official launcher is still enabled unless you ran `harbourctl enable`.
- Hold **Esc** or Home inside an external app: SIGINT after 1 s, SIGKILL after 2.5 s if it remains hung; then Harbour redraws. This failsafe requires a working evdev keyboard.

### Upgrading from Harbour Launcher (the old name)

Before 0.1 the program was called Harbour Launcher: `~/harbour-launcher`, `harbour-launcher.service`,
`/usr/share/harbour-launcher`, binary `harbour_launcher`. Kept as they were: `harbourctl`, the data in
`~/.local/share/harbour` and `~/.config/harbour` (settings, IME dictionary and learning, Paint files, feeds),
`harbour_*` config keys and `HARBOUR_*` variables.

`install_user.sh` (step 1 above) does the move by itself:
1. notes whether `harbour-launcher.service` starts at boot and is running;
2. installs `~/harbour-shell` and `harbour-shell.service` (your drop-ins in `harbour-launcher.service.d/` are
   copied with the paths renamed);
3. enables `harbour-shell.service` if the old unit was enabled, then disables, stops and deletes the old unit;
4. deletes the bundle files in `~/harbour-launcher` (anything else is kept in `~/harbour-shell/from-harbour-launcher`),
   moves `~/.cache/harbour-launcher` to `~/.cache/harbour-shell`, and starts Harbour Shell if the old one was running;
5. if `install_system.sh` was run before, says to run it once more (`sudo sh ~/harbour-bundle/install_system.sh`):
   the splash helper looks for the process name, which changed from `harbour_launcher` to `harbour-shell`.

Run from Harbour's own Terminal, the script cannot stop the service it runs in: then the old unit is removed by
the next `harbourctl try` / `harbourctl enable` (the new unit has `Conflicts=harbour-launcher.service`).
`harbourctl status` lists the old unit while it exists. The .deb replaces a `harbour-launcher` package, moves
users' boot links to `harbour-shell.service` and updates the splash helper in its postinst.

## Runtime assets

Shared runtime resources live under `assets/`: `images/`, `atlas/`, `themes/`, and `fonts/`.
Each built-in app is a package in `apps/<id>/` (next to `assets/`): `<id>.desktop` in the official format
(`Name`, `Name[ja]`, `Exec=harbour:<id>`, `Icon=icon.png` relative to the folder, `X-Harbour-Aliases`),
`icon.png`, and `res/` for files only that app uses (loaded as `app/<id>/res/...`). The launcher finds its own apps
through these files (`src/launcher/packages.rs`); `Exec=harbour:<id>` names code in the binary. Without its
package an app is not shown (Settings always is). Both the user bundle and the Debian package ship `assets/` and `apps/`. The Latin M+ font table
is compiled from `src/generated/mplus_f12r.rs`; `assets/fonts/mplus_j12r.bdf` and
`assets/fonts/{ChiKareGo2,Cavs}.fnt` are loaded at runtime. The old top-level `fonts/`
directory is removed during a user-install upgrade.

## What it does

| Area | Behaviour |
| --- | --- |
| Apps | Reads `/usr/share/APPLaunch/applications/*.desktop` and `preinstalled-desktop-apps.tsv`, same rules and order as the official launcher. Rescans every 3 s. `Exec` never goes through a shell; shell metacharacters are refused; bare commands must be in the official allow-list. |
| Home | 4x3 grid of 32 px icon tiles or a 2-column list (Tab). Scroll bar at the right edge. Empty slots are dotted. |
| Folders | Home items are apps **or folders**. Home menu (Alt tapped alone, or right click on the top level): New Folder... (name prompt, default "untitled folder"), Move to Folder... (list of folders + "Top level"), Rename Folder..., Delete Folder (confirm; its apps go back to the home screen), Auto-organize... (confirm; makes Games / Music / Tools from the apps that exist). Enter / double click opens a folder in place; Esc, Backspace, the `..` tile or right click go back; apps opened from a folder return to it. Mouse: drag an app (> 6 px) onto a folder tile; inside a folder drop it on `..` or the strip above the grid to take it out. Tab list view shows folder rows with a trailing triangle. A virtual **Recent** folder (first tile, hidden while empty) shows shortcuts to the last 8 launched items, newest first, from `~/.config/harbour/recent.txt` (one key per line); items stay where they are and nothing is filed in or out of it. Number keys 1-9 jump to that page (they already did) and also work inside folders. An empty folder is drawn as a dotted outline. Stored in `~/.config/harbour/folders.json` (`{"version":1,"folders":[{"name":"Games","items":["snake","nestopia.desktop"]}]}`; item = `.desktop` id for external apps, package id (`apps/<id>/`) for built-in ones, so a language switch changes nothing; files from older versions that name built-ins by their display name are converted on load and saved back, and so is `recent.txt`; written atomically; a missing or corrupt file means no folders). An app is in at most one folder; uninstalled apps drop out on the next scan; hidden apps stay in their folder but are not shown. |
| Visibility / order | Hidden flags use the official `app_desktop_*` keys in `~/.config/cardputerzero/config.json`, so both launchers agree; Harbour's own apps use the same key form with their package id (Settings > Apps lists them after the installed apps; Settings itself cannot be hidden). Order is Harbour's own (`~/.config/harbour/order.txt`, installed apps only). |
| External apps | Own process group, screen and keyboard handed over, output kept in `~/.cache/harbour-shell/last-app.log`. While one starts, an "Opening <app>" screen with a barber-pole bar is shown until the app draws on the LCD. |
| `Terminal=true` | Built-in PTY terminal (53x13 cells, body font (Shinonome / M+), VT100 subset, `TERM=vt100`), "press any key" when `Sysplause` is true. |
| Settings | A control panel: Display (language, brightness, screen off, Japanese dictionary), Theme (theme, invert, status bar), Lock Screen (style, auto lock, lock screen off, date, battery), Sound, Network (Wi-Fi radio/scan/connect with password, Ethernet IP), Bluetooth (power, scan, pair/connect), Devices (Grove 5V, Cap 5V), System (battery and clock, update check, App Store, official launcher, About: device information and credits, restart, power off), Apps. Pop-up buttons; pages change instantly. |
| Help | Harbour menu > Help: the keys of the screen in front (`LauncherApp::help`, host rows for its own screens) and the keys every screen shares. Screens draw no key hints of their own. |
| Install card | `AppAction::Install` (or Settings' `Event::Install`): one card over everything says what and why, asks for the user's password when apt needs root (`sudo -S`, never stored), shows progress and a plain result; a wrong password asks again. Used by Interceptor (i2c-tools, gpiod), Settings > Display > Japanese dictionary (no password) and uninstalling apps. |
| Apps | Home right click > Hide from Home. Settings > Apps: Space ticks show/hide, `[` `]` reorder, Return opens the app's page (show on Home, built-in / installed / your app, owning package, Uninstall... through the install card). Your own apps: drop a `.desktop` file (icon path relative to the folder) into `~/.local/share/harbour/apps` (`HARBOUR_USER_APP_DIR`); it shows on Home like an installed app and Uninstall moves the file to the Trash, no password. |
| Keys | Fn-layer volume and brightness keys work everywhere with a toast. **Alt tapped alone** opens the app menu (Sym, Shift and Fn are hardware toggles, so they cannot serve); Ctrl+Tab and right click also open it; Alt, Ctrl+Tab, Esc or Tab close it. **Tab long** (1 s) locks (not in the Terminal), **Esc long** (0.9 s) leaves an app and closes any menu or the Agent panel (a short Esc never closes an app from its top level, Settings included; unsaved work asks first); both show a gauge while held. **Tab Tab** (300 ms, no other key between) opens and closes the Agent where Tab means nothing to the screen; where it does (Terminal, Text Editor, Browser, Wi-Fi password, tabbed apps) it is two Tabs, and **Alt+Tab** or Harbour menu > Agent opens it (Alt+Tab works everywhere). Tabbed apps switch tabs with **Tab / Shift+Tab**. Every overlay closes with Esc. Full table: DESIGN.md "Keys". F/X/Z/C are arrows without Fn outside text entry. The **Sym** layer types `! @ # $ % ^ & * ( ) ~ \` _ - + = [ ] { } ; : ' " < > \ | , . / ?`. |
| Lock screen | Menu bar as usual, then one of four styles (Settings > Lock Screen > Style, `harbour_lock_style`): a wall clock (analog dial in a dithered bezel hanging from a nail), World Clock (map with the night side, local 7-segment time), a full-screen ASCII-art clock, a big 7-segment clock. Date and battery chips are optional (`harbour_lock_date`, `harbour_lock_battery`). **Auto lock** (`harbour_lock_auto`: off / 1 / 5 / 15 min idle) locks as a long Tab would (not during scripted runs). Scroll (wheel or Up/Down) fills the unlock chip. |
| Screen off | After `dark_time` (Settings > Screen off), or after `harbour_lock_dark` (default 8 s, Settings > Lock Screen > Lock screen off) on the lock screen: the backlight fades, the LCD is painted black, the backlight goes to 0. The first key only wakes it. Backlight 0 alone does not blank this panel; the panel power-down ioctl (`FBIOBLANK`) must not be used (it left the LCD dead after a restart). |
| Mouse | Any USB/BT mouse (evdev, hot-plugged): pointer shows while it moves, left click acts, right click opens the app menu (right click in a Home folder = back), wheel scrolls. Per screen: **Home** click selects, double click opens, drag an app onto a folder, scroll bar (thumb drag, track = page, a few px wider than drawn), wheel scrolls rows. **Settings** sidebar, rows, check boxes, buttons, pop-up choices, sliders (click/drag), close box, always-drawn scroll bar (thumb drag, track = page), wheel moves the selection. **Calculator** every letter key, BS, Ret and a strip of digits / `.` / `+ - * / ^` are buttons (both modes); click the display for the Settings dialog, click a row there to cycle its value, click outside to close. **Timer** tabs, Start/Stop/Lap/Reset, click the upper/lower half of the minutes (+-1 min) or seconds (+-10 s) digits, wheel (over the seconds = 10 s). **Calendar** `<` `>` month arrows, day cell selects, double click = today, wheel = month. **Sheet** click selects a cell, double click edits, wheel scrolls (no scroll bar is drawn). **Files / Music / Images** row click selects, double click opens, scroll bar thumb/track/arrows, wheel. **Interceptor** tabs, list rows (BT/BLE/OSC/IR/MIDI) select, scroll bar on every list, Device tree: click a row, click or double click the `+`/`-` to open/close, wheel, scroll bar. **Browser** links, form fields, address bar, Back/Next/Reload, scroll bar, wheel. **Text Editor** click puts the caret at the nearest character (measured, so proportional Latin works), wheel scrolls 3 lines. Buttons show a hover tint where the widget supports it. |
| Themes | Amber (default), Sky (light blue), Mobile, Terminal (phosphor green), Platinum (light greys), each with an Invert switch (Settings > Theme) that reverses the five tones; generated by `tools/gen_themes.py` into `assets/themes/` (see DESIGN.md). |
| Built-in apps | Terminal, Text Editor (asks to save when closed with unsaved text; Save As panel), Calculator (algebraic and RPN, big display), World Clock, System Monitor, Snake, Files, Timer, Calendar, Music (cover art, cached library), Images, Sheet, Browser (graphical, text-first: curl fetch, HTML/Japanese layout, links, GET/POST forms, dithered pictures, bookmarks; `about:home`, `file://`; menu "Open in Lynx"), Interceptor (ETH, Wi-Fi, BT, BLE, IR, Grove, USB, Serial, I2C, GPIO, 1-Wire, OSC, MIDI, Device; the tab strip scrolls with arrow tabs at both ends). An installed app with the same name replaces the built-in. New ones are one line in `launcher/apps/mod.rs` (`REGISTRY`) plus a package folder `apps/<id>/`. Names, Japanese names and icons come from the packages. |
| Icons | Hand-made 32 px 1-bit icons: a built-in's is `apps/<id>/icon.png`; an installed app whose name (or an `X-Harbour-Aliases` entry, e.g. `cli`, `rec`) matches a package wears that package's icon; Notes, Dictionary and Mail have shared icons in `assets/images/icons/`; every other icon (Fune and the other official apps included: their own `Icon=`) is converted to a 32 px tile with a one-bit glyph. |

## How it talks to the hardware (no sudo needed for any of these on a stock image)
- LCD: SDL software renderer into an off-screen window (`SDL_VIDEODRIVER=dummy`); the frame is read back as RGB565 and handed to a
  `Display` backend (`src/platform/display.rs`):
  - **DRM/KMS (preferred)** (`src/platform/drm.rs`, raw ioctls, no extra crate). Finds the connected `SPI` connector (`/dev/dri/card1`,
    `panel_mipi_dbi`), allocates one RGB565 dumb buffer (XRGB8888 if the plane refuses it), becomes DRM master (a user in `video`
    can: no root needed), scans it out with SETCRTC once and afterwards only calls `DRM_IOCTL_MODE_DIRTYFB` with the bounding box of
    the changed pixels. The driver sends just that window over SPI and the commit is synchronous, so frames are never torn. The
    120 ms "re-push the whole frame" refresh of the fbdev path is not needed and is skipped.
  - **fbdev (fallback)** (`src/platform/fb.rs`): `/dev/fb0` (`HARBOUR_FB`), RGB565, changed rows only. Used when DRM cannot be opened
    (no card, not master, no mode) and, at run time, permanently if any DRM call fails (log: `[display] DRM failed ... falling back`).
  - External apps draw through `/dev/fb0`: while one runs, Harbour hands the CRTC back to the fbdev emulation framebuffer
    (after copying its last frame there, so nothing stale flashes), and re-acquires DRM and redraws everything when the app exits.
    On exit/crash the kernel (`lastclose`) also restores the fbdev console.
  - Screen off is unchanged: black frame, then backlight 0; wake = backlight + full redraw. `FBIOBLANK` is never used.
  - Env: `HARBOUR_DISPLAY=auto|drm|fbdev` (default auto), `HARBOUR_DRM=/dev/dri/cardN` (default: autodetect),
    `HARBOUR_FB_STATS=1` (log mean read_pixels / present time and bytes every 30 pushes). Tools: `harbour_drmprobe` (connector,
    CRTC, plane formats report) and `harbour_drmprobe bench [fbdev]` (times full and partial pushes; stop the service first).
  SDL2 on the device cannot reach the LCD itself (no fbcon; KMSDRM/Wayland crash without a GPU).
- Keyboard: evdev (`/dev/input/by-path/platform-3f804000.i2c-event`), never grabbed. The Fn layer already reports arrows, F-keys, Home/End, Page keys and media keys;
  the Sym layer reports codes 26, 27, 39... while Sym (383) is held (see `/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map`).
- Brightness `/sys/class/backlight/backlight`, 5 V rails `/sys/class/leds/{grove_5v_out,ext_5v_out}`, battery `/sys/class/power_supply/bq27220-0`,
  volume `wpctl`, Wi-Fi `nmcli`, Bluetooth `bluetoothctl`, restart/power off `systemctl` (polkit allows the user service).
- Interceptor: `/sys/class/net`, `nmcli`, `bluetoothctl`, `/sys/bus/usb/devices`, `/dev/lirc*`, serial ports (`stty`, group dialout), `i2cdetect` (i2c-tools, group i2c), `gpioinfo` (gpiod), `/sys/bus/w1`, `/sys/class/leds/{grove_5v_out,grove_fun}`, ALSA raw MIDI, UDP sockets (OSC).

## Develop on a Mac
```sh
HARBOUR_MOCK=1 cargo run --bin harbour-shell                  # window (3x), mock hardware
HARBOUR_SCREEN=800x480 HARBOUR_SCALE=2 HARBOUR_MOCK=1 cargo run --bin harbour-shell   # a desk with windows (docs/DESIGN.md "Desk")
HARBOUR_HEADLESS=1 HARBOUR_MOCK=1 HARBOUR_SHOT_DIR=/tmp/shots \
  HARBOUR_SCRIPT="shot:home,Right,Return,wait:300,shot:settings" cargo run --bin harbour-shell   # scripted BMP frames
cargo test --lib                                                  # 263 tests
harbour_probe [--scan]  /  harbour_probe --apps <dir>             # read-only view of what the real backend sees
```
Script tokens (comma separated): key names, `ctrl+key`, `alt+key`, `shift+key`, `text:abc`, `wait:ms`, `shot:name`, `hold:Key:ms`, `down:Key`, `up:Key`, `menu`,
`click:x:y`, `rclick:x:y`, `move:x:y`, `wheel:n`, `drag:x1:y1:x2:y2` (move to x1,y1, press, move to x2,y2 in 6 steps, release;
`drag:...:hold` keeps the button down for a screenshot and `release` lets go), `expect:k=v&k=v` (checks `screen` (`home`, `settings`,
`editor`, `terminal`, ... or the app name in lower case without spaces), `agent`, `menu`, `modal`, `locked`, `zoom` (0/1); a mismatch is
printed and the run exits with status 3; `has=word` checks the app's own description), `state` (prints them), `rep:Key` (one auto-repeat press). `cargo test --test keymap_headless` runs the key scheme this way.

Environment: `HARBOUR_FB`, `HARBOUR_HEADLESS`, `HARBOUR_MOCK`, `HARBOUR_SCALE`, `HARBOUR_SCREEN=WxH` (a bigger screen: the desk or full mode), `HARBOUR_MODE=desk|full`, `HARBOUR_PANEL=color|gray|mono`,
`HARBOUR_ROTATE=90|180|270`, `HARBOUR_SCRIPT`, `HARBOUR_SHOT_DIR`, `HARBOUR_APP_DIR`, `HARBOUR_ROOT`,
`HARBOUR_DATA`, `HARBOUR_CONFIG`, `HARBOUR_ORDER_FILE`, `HARBOUR_EVDEV`, `HARBOUR_NO_EVDEV`, `HARBOUR_CURSOR=x,y`, `HARBOUR_START_LOCKED=1`,
`HARBOUR_LAUNCH_SHOT=<file.bmp>` (saves the "Opening" screen).
Headless runs also work on the device (real backend, no LCD): `HARBOUR_HEADLESS=1 HARBOUR_ROOT=~/harbour-shell ./bin/harbour-shell`.

## New versions

Once a day Harbour reads the latest release's version from GitHub (`launcher::update`) and compares it with its own. A newer one is announced once with a toast and stays in Settings > System as "Update to x.y.z", which downloads that release's `.deb`, checks it against the release's SHA-256 and installs it with apt (the install card asks for the password); the new version runs after Harbour restarts. "Harbour Shell <version>" on the same page checks now.

- What goes out: one request for the repository's public "latest release" record, with a generic client name and nothing about the device or its user. "Check for new versions" (Settings > System, `harbour_update_check` in config.json) turns it off.
- Not asked in mock runs or the web demo. `HARBOUR_UPDATE_LATEST=1.2.3` stands in for the answer (tests, demos).
- The state (`update.txt` in `~/.local/share/harbour`): when it last looked, the latest version seen, the version already announced.
- A release package leaves out the apps that are not ready (`RELEASE_HIDE` in `packaging/build_deb.sh`, now Yacht); the `+git` builds keep them.

## Web demo

`web/build.sh` builds Harbour Shell as a web page with Emscripten (SDL2 from the Emscripten ports) into `web/dist/`; `web/build.sh serve` also serves it on http://localhost:8017. It needs `emcc` and `rustup target add wasm32-unknown-emscripten`.

- The page picks the picture size from the browser window (a desk when there is room). `?size=320x170` shows the Cardputer Zero's screen, `?scale=N` forces the zoom, `?mode=desk|full` and `?lang=ja` work like `HARBOUR_MODE` and `LANG`, and `?script=...` drives it like `HARBOUR_SCRIPT`.
- The hardware is pretend (`HARBOUR_MOCK`). The home folder is kept in the browser (IndexedDB), so notes, drawings and settings survive a reload.
- Sound: the shell's clicks (and Yacht, when it is built in with `HARBOUR_WEB_APPS`) play through the browser's audio once the page has had a click or a key (browsers start sound only then).
- There are no threads in the page: `rt::sleep` hands each frame back to the browser (Asyncify) and `rt::spawn` runs its work at once.
- Left out: apps that need the network, a terminal, a camera or the sound device (Browser, Infoscope, Radio, Music, Voice Memo, Terminal, Interceptor, Monitor, Camera) and the Agent. The Japanese dictionary is never redistributed, so input is kana only.
- HarbourTalk is in: it talks to the board server through the browser (synchronous XMLHttpRequest), so the server lists the page's address in `ALLOWED_ORIGINS` (`server/harbourtalk/README.md`); the server setting `demo` gives the demo board instead.

## Japanese input (romaji to kana; kanji conversion with an optional, user-installed dictionary)

**Ctrl+Space** switches the input method on and off (the mode letter `A` / `あ` / `ア` / `ｱ` / `Ａ` shows at the right of the menu bar
while the screen takes text). **Ctrl+Shift+Space** cycles あ → ア → ｱ → Ａ. It works in the Agent chat, Text Editor, Terminal (UTF-8), Sheet,
Browser fields, Settings fields and dialogs; committed text arrives as ordinary text input, so those screens needed no changes. On screens
whose letter keys are hotkeys (Camera, Paint...) switch back to `A`.

While composing (underlined line at the bottom of the screen, dotted under romaji still being typed):

| Key | Does |
|---|---|
| a-z | romaji → hiragana (`kka` → っか, `nn` / `n'` → ん, `-` → ー, `, . [ ] / ~ ? !` → `、 。 「 」 ・ ～ ？ ！`; capitals start a Latin run: `Windows`) |
| Space / Shift+Space | with a dictionary: convert (dictionary and learned words first, then hiragana, katakana, half-width katakana, full-width and half-width Latin); without one: the kana / width forms only. Space again = next, Shift+Space = previous |
| 1-5 | choose one of the five candidates on the page shown (the window pages as the selection moves; `n/m` under it) |
| Enter | commit (not sent to the app) |
| Esc | close the candidate window, then cancel the composition (not "back") |
| Backspace / Delete, ← →, Home / End | edit inside the composition |
| F6 / F7 / F8 / F9 / F10 (Fn layer) | hiragana / katakana / half-width katakana / full-width Latin / half-width Latin |
| Ctrl+K / Ctrl+L / Ctrl+U | katakana / full-width Latin / clear (usable without the Fn layer) |
| Tab | commit and stay (the launcher's Tab actions do not fire mid-word) |
| any other key | commit first, then the key goes on |

With nothing being composed, only plain typed characters are taken: Tab, Esc, Alt, Ctrl chords, arrows, F keys and Enter behave as before.
Config keys (`~/.config/cardputerzero/config.json`): `ime` (1 / 0, default 1), `ime_toggle` (default `"ctrl+space"`; e.g. `"ctrl+j"`, `"alt+space"`, `"f10"`),
`ime_wide_punct` (1 = `，．`).

### Kanji conversion: the optional dictionary (Phase 2)

The dictionary is **not in the repository, the bundle or the .deb**. `install_user.sh` asks "日本語入力辞書をダウンロードしますか？" (default **No**;
no terminal = No). `harbourctl ime-install` asks the same later (`ime-install`, `ime-remove`, `ime-forget`, `ime-status`; `packaging/ime_install.sh`).
On "yes" it downloads the ten Mozc OSS dictionary files (about 63 MB, pinned to one Mozc commit, every file sha256-checked), converts them on the
device with `harbour_ime_dict` into `~/.local/share/harbour/ime/harbour-ime.dic` (about 33 MB, about 700,000 readings) and deletes the downloaded
text. The licence text (`Mozc-LICENSE.txt`) is copied beside it. Any failure (offline, checksum, disk, converter) leaves kana-only input, which
is what runs without a dictionary. Remove it any time with `harbourctl ime-remove`.

* Memory: the file is memory-mapped and searched by binary search (about 20 probes per lookup), so only the touched pages are resident (the page
  cache can drop them). Generation builds the file in six ranges of readings, so it needs a few tens of MB, not the whole data set.
* Conversion: the whole composition is looked up exactly, and one lowest-cost segmentation (word costs plus a per-word penalty; no connection
  costs yet) is added: `きょうはいいてんきです` → `今日は良い天気です`. It is a first-phase engine: no phrase boundaries to adjust, and sentences where
  grammar decides (`日本語` vs `日本` + `ご`) can come out wrong. Candidates that the 12x12 font (JIS X 0208) cannot draw are removed when the dictionary is
  generated and again when learned words are loaded.
* Learning: a choice is remembered in `~/.local/share/harbour/ime/learn` (`reading TAB surface TAB count TAB last-used`, at most 5,000 rows) and
  comes first next time. It is written 30 s after the last change and on exit, atomically; it is not recorded in the Terminal. `harbourctl ime-forget` deletes it.
* Not done: SKK-JISYO drop-in (GPL, never bundled), connection costs / bunsetsu editing (phase 3), AI conversion (phase 4).

## Not done / not verified
- On the device: Wi-Fi connect and Bluetooth pairing from Settings, `Terminal=true` apps, PIN-based Bluetooth pairing, the official built-in pages
  (Python, SSH, LoRa, Mesh, Camera), time zone/time setting.
- Interceptor pages beyond mock data (BLE scan, IR decoding, MIDI input) and Music playback of every format; OSC receive was only tried with a test sender.
- Mouse: not mouse-operable yet: Text Editor selection by dragging, Browser address bar caret placement, Terminal (keyboard only), Sheet scroll bar (none is drawn), World Clock / Snake / Monitor / Tone / Osc apps beyond what the host does (World Clock: click left/right half = previous/next city). Settings text pages (Wi-Fi password) are keyboard-driven apart from the OK/Cancel buttons. Hover tint exists only on Timer and Calendar buttons; the Calculator keys only flash while pressed. Verified with scripted headless clicks/drags on the mock backend and unit tests, not on the real device with a physical mouse.
- Key scheme (Tab Tab / Alt+Tab / Esc long, Tab bars): unit tests and headless scripts only. Not tried on the device: Alt+Tab through the evdev keyboard (Alt is not a tap there, so no menu should open), Shift+Tab with the latching Shift, the 300 ms double-tap window by hand.
- Japanese input: scripted headless runs and unit tests only (the dictionary converter was run on the development Mac with the real Mozc data: 33 MB file, 5 test sentences in 47 ms; the on-device install, memory use and cold-cache lookup times were not measured); not tried on the device (Ctrl+Space and the Fn-layer F6-F10 reaching the app, half-width katakana look: the bundled font draws them as full-width glyphs). Half-width katakana in the Terminal depends on the host's terminal program.
- The StickMouse (BT mouse on an M5 Stick) drifting cursor was not reproduced.
