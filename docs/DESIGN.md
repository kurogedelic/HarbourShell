# Harbour design language

One screen: 320x170, 1-bit look drawn with a few tones. Everything follows a few strict
rules: hard outlines, dithered shadows, inverted selection, no
anti-aliasing, no footers.

## Grid (`src/grid.rs`)
* One rule: **whole cells only; what is left over stays empty.** Content is laid out in whole units and rows; the pixels
  that do not fill another cell (the gap right of the last column, the rows below the last whole row) are never used,
  stretched or filled with content.
* Unit = 4 px. Menu strip 16 px; content = 9 rows of 16 px = 144 px (y 16..160).
* **The screen's bottom.** Only the two top corners of the screen are rounded (`chrome::draw_corners`,
  `grid::CORNER_MASK`); the bottom corners are square, so nothing is cut off down there. Content may use the screen
  down to y 168 (1 px above its last row, as the sides keep 1 px to the screen's edges); nothing is drawn in y 169.
  Apps that use the full height give the space to content (a taller graph, one more row, a bigger picture) and put
  what cannot be a whole row into a panel's or frame's edge, so its bottom line sits on y 168 — never an empty band.
  Scroll bars run to the new bottom. Monitor (8 body rows, graphs and dividers to y 168), Timer (56 px digits, 8-row
  well, the default button's ring on y 168), Music (list frame to y 168; Now Playing cover 136 px), Radio (dial / deck
  panel 120 px with the readout plate to y 168, 16 px genre rows, a 24 px header over the podcast lists), Voice Memo
  (40 px waveform, buttons' ring on y 168; Files 8 rows) and Camera (preview 198 x 148 and side panel to y 168) do so.
* **The framed screens use the screen down to its bottom:** Terminal, Text Editor, Browser, Images, Paint's sheet and
  Settings. Their content is one big area in a 1 px frame (`grid::draw_app_frame`): x 1..318, from y 17 (Browser:
  under its toolbar) down to y 168, a plain rectangle with square corners. Content fills `grid::app_content()`
  (x 3..316, y 19..161, 1 px clear of the line): Terminal 52 x 13 cells, Text Editor 11 lines. Paint's sheet keeps its
  own square frame and its horizontal scroll bar ends at y 167. Settings fills the frame with its sidebar (ten 14 px
  rows from y 20; the rest below stays empty) and its page (7 rows of 20 px; the scroll bar column runs on to y 166);
  the sidebar's divider runs from the frame's top line to its bottom line; notes sit in `app_content()`.
* **Home, Files, Sheet, Calendar, Calculator and World Clock also run down to the bottom** (nothing on y 169):
  Home's grid rows are 52 px from y 18 (names end on y 167) and its list has 10 rows of 15 px (y 18..166), the scroll
  bar y 18..167; Files' list box and scroll bar end on y 168 (8 whole rows, the 5 px under them are the box's paper);
  Sheet's grid frame and column lines end on y 168 (7 whole rows); Calendar has a 17 px weekday band and 19 px weeks,
  grid and side panel ending on y 166 (the panel's shadow on y 168); Calculator has a 44 px display and 26 px keys, the
  digit strip ends on y 168; World Clock shows the whole map (121 rows, rule on y 137) and its plates end on y 168
  (the lock screen keeps the 112-row crop).
* Dialog cards (`chrome::draw_app_window`, `widgets::draw_panel`) keep a 1 px gap to the screen edges (318 x 152 at x = 1, y = 17).
* **No window frames.** Every app fills the area under the menu strip (9 whole 16 px rows, `grid::CONTENT_H`, 4 px side
  margins) and the app's name is shown in the menu strip, to the right of the anchor (the folder name on Home; Settings
  shows `Settings: <page>`, Snake its score). All screens share the selection plate, row rules, scroll bar column and fonts.
  (`chrome::draw_app_window` remains only for dialogs that still want a titled card.)
* Rows (lists, menus, title) 16 px; buttons 24 px (small 16 px); scroll bar 4 px wide at x = 312.
* Home grid: 4 columns x 3 rows. A cell is 76 x 52; the tile is 72 x 44 (4 px gutter, 8 px between rows); origin x = 4,
  y = 18, so the third row's names end on y = 167. The 32 px icon sits at the top of the tile, the 12 px label under it.
* Home folders: the top level lists folders first (a folder glyph, tab + body, with up to four of its apps'
  icons as 12 px minis), then the loose apps. A folder opens in place as a sub-page: the grid shows a `..` back-arrow
  tile and the folder's apps, the menu strip names the folder (Cavs), Esc / Backspace / the `..` tile / right click go
  back, a zoom rectangle plays from/to the folder tile. In the list a folder row has a folder glyph and a trailing
  triangle; an open folder starts with a "Back" row. Dragging an app (> 6 px) shows it following the pointer with a Half
  dither shadow; the folder tile under it gets a triple ink frame; dropping outside cancels; inside a folder the `..`
  tile and the strip above the grid return the app to the top level. Model in `launcher/folders.rs`
  (`~/.config/harbour/folders.json`, next to `order.txt`).
  The virtual "Recent" folder (`recent.txt`, 8 keys) is the first tile when it has content; an empty folder is a dotted outline only.
* Everything is placed on multiples of 4 (`grid::snap`, `grid::on_grid`, tested).

## Tones (`assets/themes/*.yaml`)
Base keys: `background`, `foreground`, `muted`, `shadow`, `widget_face(_active)`,
`widget_outline`, `widget_shadow`, `titlebar_stripe_*`.
Sub-colours (all themes define them): `home_bg`, `home_bg_alt` (page wash), `surface_alt`,
`surface_sunken` (tiles, wells, list zebra), `line` (soft rules), `text_faint`
(secondary text), `accent_soft` (selection wash, hover).
Themes: four five-tone palettes (black, A, B, C, white): Amber (default), Sky (light blue), Mobile, Terminal (phosphor green), each with an Invert switch that reverses the tones. Roles: background = C (the 3/4 tone), text = black, rules and hints = B, the selection plate = white with black text (the brightest tone marks highlights only). Generated by `tools/gen_themes.py`; try palettes in `tools/palette-playground.html`.
Rule of thumb: ink for outlines and text, `line` for structure, `surface_*` for fills,
dither for anything that is "in between" (shadow, disabled, selection wash).

## Dither (`src/dither.rs`)
`Pattern::{Eighth,Quarter,Half,ThreeQuarter}`; `fill`, `fill_two_tone`, `gray_out`.
Uses: drop shadows (Half), page background (Quarter), empty slots (Eighth), gauges, disabled
controls (`gray_out`), selection wash on the selected tile.

## Type
* Titles, menu bar, buttons, tile names: **ChiKareGo2** (Giles Booth, CC BY; Chicago clone on a 16 px grid, cap height 9 px,
  descenders to row 11, tracking -1, space 5 px). Cavs (our own Chicago-like face) is the fallback when ChiKareGo2.fnt is missing.
  (Fixed; there is no setting for it.)
* Body: M+ F12 Regular (`draw_text`). Terminal: the body face at a 6 px pitch.
* Numerals: world clock 7-segment atlases; lock screen big 7-segment digits (drawn) and the 5x7 dot glyphs as ASCII art.
* Source fonts live in `assets/fonts/`; the Latin M+ table is compiled into `src/generated/mplus_f12r.rs`,
  while the Japanese BDF and Playdate `.fnt` files load at runtime.

## Components (`src/launcher/widgets.rs`)
Push button (rounded; default button double ring; pressed = inverted), check box, radio,
slider, scroll bar, pop-up button + popup list (solid 1 px shadow, check mark on the
current item), app menu, `PointerEvent` (move/down/up/drag/wheel, double click 450 ms).

Text fields: every one-line entry draws through `widgets::draw_field` (`Field`): one grid row high (`FIELD_H` = 16),
`surface_sunken` fill, 1 px ink frame, body-font text 4 px in, a 1 px caret at the capitals' height, the text scrolled
to keep the caret in view, `*` per character when masked, inverted text when all is selected (the first key replaces it),
a faint placeholder when empty, an optional fixed prefix (`>` prompt, Sheet's cell name). A label beside a field is a
plate of the same 16 px height. In a card the field starts at the text's x and runs to the text's right edge; on a page
it sits on a row of the grid. Pages with their own face (Infoscope, Radio) pass their colours. Used by the dialogs
(name prompt, Save As), the install card, Settings' Wi-Fi password, the Agent, Browser (address bar, form fields),
Infoscope, Radio search, Sheet's input bar and Interceptor's serial line.

Dialogs (`src/launcher/dialogs.rs`): the "Save changes to <file>?" alert (Don't Save / Cancel / Save, Save is the default;
D, S, Esc work too) and the Save As panel (name field with the suggestion selected, folder list, Right opens a folder,
Left goes up, Enter saves, an existing file asks once more before it is replaced). The host shows them over an app;
the Text Editor uses them when it is left with unsaved text (hold Esc, or the menu's Quit) and from its menu's
"Save As...". Home uses the same look for `NamePrompt` (title, name field with the suggestion selected, Cancel / OK),
`ConfirmAlert` (two buttons; a destructive action defaults to Cancel) and `ListPicker` (4 rows, "Move to Folder...").

## Rules and grid lines
Every list, table and panel is built from the same 1 px rules (`line`), so apps look related:
* **List rows**: a rule on the last pixel row of every 16 px row (`widgets::draw_row_rule`), full list width up to the
  scroll bar column. The selection plate is one pixel shorter than the row and sits inside the rules; the rule is
  drawn after the plate. Files, Music, Voice Memo (files), Timer laps and the Calendar day panel do this.
* **Lists as areas**: the list area is outlined 1 px; a path / header bar above it is a zebra (`surface_alt`) band
  closed by a rule.
* **Tables**: grid lines between all cells (Sheet, Calendar). The row / column headers sit on `surface_alt` and are
  closed by a heavier (`foreground`) rule; the selected cell keeps its plate inside the lines (Sheet adds an ink
  outline), today is outlined in ink.
* **Panels**: outlined 1 px (ink for white faces, `line` for list areas); a title row inside a panel is closed by a rule.
* Terminal and Text Editor stay plain on purpose.

## Browser (`src/launcher/apps/browser.rs`, `browser/`)
* A graphical, text-first browser (no JavaScript, almost no CSS). Rows: toolbar (16 px: Back, Next, Go/Stop buttons and the address
  field), page viewport 128 px (8 rows, scroll bar at x = 312), no footer: a one-row overlay at the bottom of the page appears only for the link target, loading (`ProgressBar`) or a message, and the title lives in menu > Page info (title, URL, charset, size, counts). Text is M+ 12
  on a 304 px column, 14 px lines; Japanese wraps between characters.
* Headings are bold (double-strike) with underline / rule, italics are dotted-underlined, links underlined, the selected link inverted,
  `pre` sits on a sunken band, tables become columns when they fit and stack otherwise, pictures are 4-tone dithered (`tone.rs`).
* Keys: Up/Down line, Space / Shift+Space page, Tab / Shift+Tab or Left/Right link, Enter follow / edit / press, Backspace back
  (Shift: forward), `/` or Ctrl+L address bar, Ctrl+R / F5 reload, Esc stop / leave a field.
* `R` / menu "Reader": only the main text (the paragraph-densest block, `article`/`main` preferred) with the title; remembered per host for the session.
  Pages appear as soon as the first chunk is parsed (`fetch_partial`), history keeps the scroll position per entry, and bookmarks/history are
  `NamedList`s (`browser/bookmarks.rs`: `# name` header + `url<TAB>title` lines) that a Home web-shortcut folder can reuse.
* Pipeline: `fetch.rs` (curl, file, mock) -> `load.rs` (charset, parse, layout in a worker thread) -> `layout::Page` (y-sorted items,
  only the visible range is drawn). Fixtures for `HARBOUR_MOCK=1` live in `tests/fixtures/`.

## Keys (one scheme for the whole system)
The device has 46 keys; Fn, Sym and Shift latch in the keyboard hardware, so the host's own gestures use Tab, Esc and Alt.
Rule of thumb: **the host takes chords, holds and the Tab double tap; a single Tab and a short Esc belong to the screen in front.**

| Key / gesture | Where | Does |
|---|---|---|
| Esc (short) | everywhere | closes the innermost thing: IME candidates, then the composition, a popup, a dialog (= Cancel), a menu, the Agent panel, a field, a page (back). Never leaves an app from its top level (Settings included). |
| Esc long (0.9 s, gauge) | any app, the Agent panel | leaves to Home; menus and the Agent panel close too. Unsaved work asks first ("Save changes?": Text Editor, Sheet, Paint; Voice Memo while recording). Not on Home or the lock screen. |
| Alt tapped alone | everywhere | app menu (Agent menu while the panel is open). Alt again, Esc or Tab closes it. Left from it = Harbour menu, Right = back. |
| Ctrl+Tab / right click | everywhere | the same app menu; Ctrl+Tab again closes it. |
| Tab (single) | the screen in front | the app's own Tab (see below); on Home grid/list (after the 300 ms double-tap window). |
| Tab Tab (two taps, release to release <= 300 ms, nothing else pressed between) | screens where Tab means nothing | opens / closes the Agent panel. An open panel always closes with Tab Tab. |
| Alt+Tab | everywhere | opens / closes the Agent panel (the way in where Tab belongs to the screen). Also Harbour menu > Agent. |
| Tab long (1 s, gauge) | everywhere but the Terminal | lock; on the lock screen it unlocks (as do Up/Down x4 and the wheel). Tab's auto-repeat is dropped (only the Terminal and Text Editor get it), so holding Tab in a tabbed app moves one tab, not many. |
| Ctrl+Space / Ctrl+Shift+Space | text screens | IME on/off / kana mode. |
| Ctrl+Q | development window only | quit the launcher (not on the device's framebuffer). |
| F/X/Z/C | outside text entry | Up/Down/Left/Right without Fn. |

**Where Tab belongs to the screen** (`LauncherApp::wants_tab`, `Settings::wants_tab`; the host's `screen_uses_tab`), Tab Tab is just
two Tabs and the Agent opens with Alt+Tab: Terminal (sent to the shell: completion), Text Editor (indent), Settings > Wi-Fi password
(show/hide), Browser (link / field focus), and every app with a tab bar. Everywhere else (Home, Calculator, Clock, Snake, Files, Music,
Images, Camera, Paint, Sheet, Calendar...) Tab Tab opens the Agent; the app sees the two Tab presses but has no use for them.
Reason: a double tap can only be told from two single taps after the fact, so on a screen that uses Tab it would fire whenever the
user steps through tabs or fields quickly.

**Tab bars**: Tab / Shift+Tab = next / previous tab in every tabbed app (Monitor, Infoscope, Interceptor, Timer, Voice Memo; Radio's
two modes); Left/Right do the same where the page has no other use for them (Monitor, Interceptor, Timer, Infoscope outside the search
field); number keys where the app has them (Monitor 1-6, Infoscope 1-4, Voice Memo 1-2).

**Overlays** all close with Esc and with what opened them: Agent panel (Tab Tab / Alt+Tab), app and Harbour menus (Alt / Ctrl+Tab),
dialogs (Esc = Cancel), Settings pop-ups, the IME candidate window (Esc closes it, the next Esc cancels the composition). While the IME is
composing it takes Esc and Tab (Tab commits), so the host's Tab gestures and the Esc gauge do not start mid-word.

Gesture code: `platform::keys::{TabGesture, HoldKey, DoubleTap}` (unit tests there); end-to-end checks in `tests/keymap_headless.rs`.

## Desk: bigger screens (`src/launcher/desk.rs`)
Harbour is drawn for the Cardputer Zero's 320 x 170. On a bigger screen (`HARBOUR_SCREEN=WxH`) the host shows a
**desk** instead of one full-screen app. On 320 x 170 there is no desk and nothing changes.
* **Home is the desk.** Its icons stand on the desk's background in columns from the right edge, top to bottom (each
  name on a small paper plate); Up/Down move in a column, Left/Right to the next column. A press on the empty desk gives
  Home the keyboard. There is no list view and no scrolling on the desk.
* **The desk's strip is the menu bar.** The anchor (Harbour menu), the focused screen's name (its app menu) and the
  status icons; menus drop from it. It belongs to the focused window, or to Home. Each status item opens its own
  menu: the clock (date, World Clock, Calendar, Timer), the battery (Monitor, System settings), the network (Wi-Fi on /
  off, Network settings, Interceptor), the internet (Browser, Infoscope), the speaker (louder, quieter, mute, Sound
  settings) and the input method (Japanese / English, Hiragana / Katakana). (On the Cardputer Zero's strip too.)
* **Every other screen is a window.** An app opened from Home gets one, zooming out of its icon (cascading 16 px right
  and down); going back to Home (Quit, Esc held, the close box) closes it, zooming back into the icon, and the window
  under it takes the focus. A window's top 16 px row is its **title bar**: a close box at the left and the name in the
  middle, with a 1 px dither band either side of the name while it has the focus (the close box shows a cross under
  the pointer; a click on the bar opens the window's menu where it was clicked, as do the desk strip's menus); a flexible window shows a small
  solid triangle in its bottom-right corner, where it is resized from. Windows have a 1 px frame and a 1 px shadow.
* **Two kinds of app** (`LauncherApp::sizing`):
  * `Fixed` (the default): drawn pixel by pixel for 320 x 170, always that size. Calculator, World Clock, Timer, Snake,
    Radio, Camera, Monitor, Music, Voice Memo.
  * `Flexible`: laid out from the surface (`grid::sw()` / `grid::sh()`, `grid::scroll_x()`, `grid::frame_bottom()`,
    `grid::app_frame()`...), never under 320 x 170. They open at about two thirds of the desk and resize from the
    window's bottom-right corner. Browser, Calendar, Infoscope, Interceptor, Files, Sheet, Images, Paint (the sheet's view fills the
    window), Settings, Text Editor, Terminal.
  * Rule for a flexible app: anything that depends on the screen size is computed when drawing or handling input
    (a small `fn`, not a `const`); at 320 x 170 it gives exactly the old numbers.
* **Pointer:** a press on a window behind brings it to the front (that press only focuses). Drag a window by its title
  bar; drag the bottom-right corner of a flexible window to resize it. Windows stay on the desk.
* **Keys** go to the focused window (or Home). Ctrl+` steps through the windows and Home.
* Windows behind keep running (they are ticked and redrawn), so a clock or a graph stays live.
* Help (Harbour menu) opens in a window of its own beside the screen it explains (`help::HelpApp`, flexible); a short
  Esc closes it (`LauncherApp::esc_closes`). On the Cardputer Zero it stays the full-screen overlay.

## Other screens (`src/platform/display.rs`)
Harbour runs on any Linux device with a DRM (or 16-bit fbdev) screen; the picture is still drawn in Harbour pixels.
* **Scale.** The panel's size and physical size give a whole-number scale, so a Harbour pixel comes out about 1/160"
  (without the physical size: one per 360 panel rows), never leaving less than 320 x 170 (`choose_scale`). The picture
  is centred on the panel with black around it. `HARBOUR_SCALE` forces the scale, `HARBOUR_ROTATE=90|180|270` turns it
  for a panel mounted on its side.
* **Mode** (from the picture size): exactly 320 x 170 = the Cardputer Zero, one full-screen app; 560 x 300 or more =
  the **desk** (above); anything between = **full**: one screen at a time over the whole picture: Home's grid gets more
  columns and rows, a flexible app the whole picture, a fixed one sits in the middle at 320 x 170.
  `HARBOUR_MODE=desk|full` forces one.
* **Grey and black-and-white panels.** A driver that only takes 8-bit grey, or a Sharp memory LCD (and kin), gets the
  Gray or 1-bit theme (`assets/themes/theme-gray.yaml`, `theme-mono.yaml`; the colour themes are not offered; Invert
  still works). A 1-bit panel is sent black and white: dark tones solid black, light tones solid white, the tones
  between an ordered dither (`display::tone`), so text and rules stay whole. `HARBOUR_PANEL=color|gray|mono` overrides
  the guess (also for headless screenshots).
* Examples (by their published specs): uConsole 1280 x 720 at 2x = 640 x 360, desk; DevTerm 1280 x 480 at 1x, desk;
  HackberryPi 720 x 720 at 2x = 360 x 360, full; Beepy 400 x 240 1-bit, full; an HDMI monitor, desk.

## Gamepad and on-screen keyboard (`src/platform/gamepad.rs`, `src/launcher/osk.rs`)
For handhelds with only a gamepad (Anbernic and kin) and for USB / Bluetooth pads (SDL's game controllers in a desktop
window). `HARBOUR_PAD_LAYOUT=nintendo` takes the right face button as A (default: the bottom one).
* The left stick moves the pointer (a dead zone, then faster the further it is pushed). While the pointer shows (3 s
  after the stick last moved) A clicks; otherwise A is Return. The stick pressed in clicks, Y right-clicks (context menu).
* D-pad = arrows, B = Esc (held: leave the app), L / R = previous / next tab, Start = app menu, Select = Harbour menu,
  X = the on-screen keyboard.
* **On-screen keyboard:** four rows of 16 px keys across the bottom and nothing else (71 px; space, return, delete and
  the arrows are the gamepad's buttons). Pages, X in turn: lower case, upper case, symbols with Japanese punctuation,
  kana (Japanese input goes on with the kana page and off after it). The d-pad walks the keys, A types, B deletes,
  L / R move the text cursor, Y is a space, Start is Return, Select closes it; a click on a key types it. A screen that
  lays itself out (and Home) ends above the keyboard while it is open, so nothing is hidden behind it (the surface may
  then be as low as `grid::MIN_SURFACE_H`); a fixed-size screen is just covered. The gamepad and keyboard keys are in
  Help. Letters go out as a key and its text like a real keyboard's, so romaji becomes kana when
  Japanese input is on; what is being composed shows just above the keyboard. Labels are centred inside their line.
* **Kana page:** the twelve-key layout of handheld consoles, あ か さ / た な は / ま や ら / ゛゜小 わ(をんー)
  、。？(！「」・), three keys per row on six of the ten columns. The other four show the column of the key: where the tapping is, dark on a plate, the rest faint
  (あ い **う** え お after three presses). Pressing the same key again (without moving) steps through its column (か き
  く け こ); ゛゜小 turns the last kana into its voiced, half-voiced
  or small form (か が, は ば ぱ, つ っ づ). Kana go out as their romaji, so the input method composes them and Space
  (Y) converts as usual.
* Script tokens: `pad:a` (up, down, left, right, a, b, x, y, l, r, start, select, stick), `stick:x:y` (-1000..1000;
  `stick:0:0` lets go).

## App contract (`src/launcher/app.rs`)
* Esc short = back (closes a dialog or page); it does not leave the app from the top level. Esc long (0.9 s) = leave from anywhere, with a gauge; the menu's Quit does the same.
* Alt tapped alone = app menu (Sym/Shift/Fn are hardware toggles) (entries + "Quit"); Ctrl+Tab also works. Right click = same menu. A single Tab and a short Esc stay free for apps (see Keys above); an app that uses Tab says so with `wants_tab`.
* The anchor at the left of the menu strip = the Harbour (system) menu: About, Settings, Home, Agent, Lock Screen, Restart, Power Off. Click the anchor, or from the
  open app menu press Left (Right goes back): the two menus sit side by side like a menu bar. Clicking the app name opens the app menu.
* Tab long (1 s) = lock; scroll or Up/Down unlocks. On Home Tab short switches grid/list.
* F/X/Z/C = arrows without Fn except in text entry.
* Mouse: pointer shows while it moves (hidden after 3 s), left click selects, double click
  opens, wheel = Up/Down, drag for sliders and scroll bars, wheel/drag left = back in Settings.
* Register an app with one line in `apps::REGISTRY` (`id`, `make`) and a package folder `apps/<id>/`
  (`<id>.desktop` with `Exec=harbour:<id>`, `Name`, `Name[ja]`, and a 32x32 `icon.png`; see `launcher/packages.rs`);
  it then gets the home tile, menu, Esc handling, mouse routing and its icon.

## File operations (`src/launcher/fileops.rs`, `dialogs.rs`)
Every app that works on files follows the same rules.
* **Menu names, in this order**: New..., Open..., Save, Save As... for documents (Text Editor, Sheet); Rename..., Delete... for
  things that exist as files (Files, Images, Voice Memo); Copy, Cut, Paste in Files. "..." means a dialog follows. Only entries that
  apply are listed (no Paste without a clipboard, nothing on the ".." row). Shortcuts: Ctrl+S / N / O, F2 rename, Delete key, Ctrl+C / X / V.
* **The file name is the app's name in the menu strip** (Text Editor, Sheet), with ` *` while there are unsaved changes. Names
  longer than the strip are cut with "..." but the `*` stays.
* **Nothing is overwritten silently.** Save As asks with a second Enter; New and Paste never replace a file (New refuses a taken name,
  Paste and Move pick `name copy.ext`); Rename refuses a taken name.
* **Delete** asks first ("Move "x" to the Trash?", buttons Cancel / Trash, Enter = Cancel), then moves to `~/.local/share/Trash`
  (with a `.trashinfo`). Only things already in the Trash are deleted for good, with their own confirm ("Delete"). `/` and `$HOME` are never touched.
* **Unsaved work**: leaving (menu Quit, Esc long), New and Open show "Save changes?" (Save is the default; Esc = Cancel). Apps hook
  this with `LauncherApp::request_close`.
* **Dialogs** come from `dialogs.rs` only: `NamePrompt` (Rename / New folder, 64 characters), `ConfirmAlert`, `SavePanel` (Save As, New, and
  Open mode with an extension filter), `ListPicker` (Open With), `UnsavedCard`. Apps keep one `Dialog<Tag>` and answer its `DialogOut`.
  Esc short closes a dialog; every dialog works with the mouse (buttons, rows, double click, wheel).
* **Open with** (`AppAction::Open { app, path }`): Enter in Files opens by extension (txt md log -> Text Editor; png jpg bmp gif -> Images;
  csv -> Sheet; mp3 wav ogg flac m4a -> Music); anything else shows a toast that says why. Open With... lists the apps. A file that an app
  cannot read (binary, too large for the Text Editor) is refused with a reason, not shown garbled.
* **Names** are one path component: no `/`, no `.` / `..`, no control characters, at most 255 bytes. Japanese names are fine; the display
  face has no Japanese, so a title or field that holds any is drawn in the body face. Symbolic links are never followed (a copy of a link is a link,
  deleting one removes the link only).

## Micro-interactions
Zoom-rect open/close, Settings slides sideways between pages, lock gauge fills with scroll
and decays when idle, hold gauges for Esc/Tab, battery lightning bolt while charging,
toasts for volume/brightness.

## Test hooks
`HARBOUR_HEADLESS=1 HARBOUR_MOCK=1 HARBOUR_CONFIG=... HARBOUR_SHOT_DIR=... HARBOUR_SCRIPT=...`
with tokens: key names, `ctrl+key`, `text:abc`, `wait:ms`, `shot:name`, `hold:Key:ms`,
`down:Key`, `up:Key`, `click:x:y`, `rclick:x:y`, `move:x:y`, `drag:x1:y1:x2:y2` (add `:hold` to keep the button down,
then `release`), `wheel:n`, `menu` (Alt tap);
`HARBOUR_CURSOR=x,y`, `HARBOUR_START_LOCKED=1`.
More tokens: `theme:N[:invert]` (N = 0..3 Amber/Sky/Mobile/Terminal; rewrites config.json and applies at once),
`theme-ui:N` (Left/Right presses on the Theme row of Settings > Theme), `goto:Name` (Home selection jumps to a tile: a package id such as `voicememo`, a shown name in either language, or an old name such as `Recorder`),
`include:file` (script files only). Scripts can be a file (`--demo FILE` or `HARBOUR_SCRIPT_FILE`): tokens separated by
commas or newlines, `#` comments. Frame recording: `--rec DIR` / `HARBOUR_REC_DIR`, `HARBOUR_REC_FPS` (default 15),
`HARBOUR_REC_FILE` (FIFO or /dev/stdout) write a raw RGB565 320x170 stream at constant fps. See `demo/README.md`.
