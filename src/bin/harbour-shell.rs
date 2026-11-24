//! Harbour Shell: a replacement home screen for the Cardputer Zero.
//!
//! On the device it renders with SDL's software renderer into an off-screen
//! window (`SDL_VIDEODRIVER=dummy`), copies the pixels to `/dev/fb0` and reads
//! the keyboard from evdev. On a Mac it opens an ordinary window with a mock
//! system backend, so the whole UI can be developed and tested there.
//!
//! Environment:
//! - `HARBOUR_FB=/dev/fb0`   render to the LCD framebuffer (implies headless)
//! - `HARBOUR_HEADLESS=1`    no window (screenshots / scripted runs)
//! - `HARBOUR_MOCK=1`        use the in-memory backend even on Linux
//! - `HARBOUR_SCALE=N`       window scale on a desktop (default 3)
//! - `HARBOUR_SCRIPT=...`    comma-separated keys for scripted runs, e.g.
//!   `Right,Return,wait:300,shot:settings,text:abc,Escape`
//! - `HARBOUR_SHOT_DIR=dir`  where `shot:` writes BMP files
//! - `HARBOUR_APP_DIR`, `HARBOUR_ROOT`, `HARBOUR_DATA`, `HARBOUR_CONFIG`

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::surface::Surface;
use sdl2::video::Window;

use harbour_shell::assets::AssetStore;
use harbour_shell::launcher::agent::{self, tools::HostReq, AgentApp};
use harbour_shell::launcher::app::{AppAction, Ctx, LauncherApp};
use harbour_shell::launcher::widgets::{PointerEvent, DOUBLE_CLICK, DOUBLE_CLICK_SLOP};
use harbour_shell::launcher::calc::{Calc, CalcEvent, Settings as CalcSettings};
use harbour_shell::chrome::{self, StatusView, SCREEN_H, SCREEN_W};
use harbour_shell::font::{load_mplus_f12r, FontSystem};
use harbour_shell::launcher::desk::{Desk, Press, Release, WinId};
use harbour_shell::launcher::desktop_entry;
use harbour_shell::launcher::fileops;
use harbour_shell::launcher::folders::Dest;
use harbour_shell::launcher::home::{Builtin, DragEnd, Home, HomeAction, Item, Target, View};
use harbour_shell::launcher::lock;
use harbour_shell::launcher::icons;
use harbour_shell::launcher::packages;
use harbour_shell::launcher::process::{self, RunError};
use harbour_shell::launcher::registry::{self, AppEntry};
use harbour_shell::launcher::dialogs::{
    CardChoice, ConfirmAlert, ConfirmChoice, ListPicker, NamePrompt, PickResult, PromptResult, SavePanel, SaveResult, UnsavedCard,
};
use harbour_shell::launcher::settings::{AppRef, Event as SettingsEvent, Settings};
use harbour_shell::launcher::terminal::Terminal;
use harbour_shell::launcher::worldclock::WorldClock;
use harbour_shell::platform::keys::{DoubleTap, HoldKey, Input, MediaKey, TabGesture, TabUp};
use harbour_shell::platform::Shared;
use harbour_shell::snake::SnakeState;
use harbour_shell::system::config::{self, desktop_config_key, Config};
use harbour_shell::system::linux::LinuxBackend;
use harbour_shell::system::mock::MockBackend;
use harbour_shell::system::Backend;
use harbour_shell::text_editor::TextEditor;
use harbour_shell::theme::{Theme, ThemeManager};

use harbour_shell::platform::display::Display;
type Fb = Box<dyn Display>;

enum Screen {
    Home,
    Settings(Box<Settings>),
    Calculator,
    Clock,
    Snake,
    Editor,
    Terminal(Box<Terminal>),
    App(Box<dyn harbour_shell::launcher::app::LauncherApp>),
}

/// A dialog the home screen shows (folders).
enum HomeModal {
    NewFolder(NamePrompt),
    Rename(usize, NamePrompt),
    DeleteFolder(usize, ConfirmAlert),
    AutoOrganize(ConfirmAlert),
    /// Pick where to move an app: the labels' destinations.
    Move(ListPicker, Vec<Dest>),
}

/// Zoom rectangles between the home screen and a window.
struct Zoom {
    t: f32,
    from: Rect,
    to: Rect,
    /// Screen to show once an opening zoom ends.
    pending: Option<Screen>,
}

struct Toast {
    text: String,
    fraction: Option<u32>,
    until: Instant,
}

/// A file another app asked to open (`AppAction::Open`); the main loop picks it up next frame.
static OPEN_REQ: std::sync::Mutex<Option<(Option<String>, PathBuf)>> = std::sync::Mutex::new(None);
/// Something an app asked to install (`AppAction::Install`); the main loop opens the card next frame.
static INSTALL_REQ: std::sync::Mutex<Option<harbour_shell::launcher::installer::InstallReq>> = std::sync::Mutex::new(None);

/// What the Text Editor does once the current note is safe (saved, or its changes dropped).
#[derive(Clone, Debug, PartialEq, Eq)]
enum EditorNext {
    /// Leave to the home screen.
    Home,
    /// Ask for a name, then start an empty file.
    New,
    /// Show the Open panel.
    PickOpen,
    /// Load this file.
    Open(PathBuf),
}

/// Why the file panel is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelFor {
    SaveAs,
    NewFile,
    OpenFile,
}

fn apply_app_action(action: AppAction, screen: &mut Screen, workdir: &Path, shared: &Arc<Shared>, toast: &mut Option<Toast>) {
    match action {
        AppAction::None => {}
        AppAction::Quit => *screen = Screen::Home,
        AppAction::Toast(text) => *toast = Some(Toast { text, fraction: None, until: Instant::now() + Duration::from_secs(2) }),
        AppAction::Open { app, path } => *OPEN_REQ.lock().unwrap() = Some((app, path)),
        AppAction::Install(req) => *INSTALL_REQ.lock().unwrap() = Some(req),
        AppAction::Launch(cmd) => {
            // No shell; an empty command must not reach Terminal::spawn.
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            if argv.is_empty() {
                *toast = Some(Toast { text: "Launch: empty command".into(), fraction: None, until: Instant::now() + Duration::from_secs(3) });
                return;
            }
            match Terminal::spawn(&argv, Some(workdir), shared.clone(), false) {
                Ok(t) => *screen = Screen::Terminal(Box::new(t)),
                Err(e) => *toast = Some(Toast { text: format!("Launch: {e}"), fraction: None, until: Instant::now() + Duration::from_secs(3) }),
            }
        }
    }
}

fn is_double_click(last: Option<(usize, Instant, i32, i32)>, id: usize, x: i32, y: i32) -> bool {
    last.map(|(prev_id, t, prev_x, prev_y)| {
        prev_id == id && t.elapsed() < DOUBLE_CLICK
            && (prev_x - x).abs() <= DOUBLE_CLICK_SLOP
            && (prev_y - y).abs() <= DOUBLE_CLICK_SLOP
    }).unwrap_or(false)
}

fn env_bool(name: &str) -> bool {
    matches!(std::env::var(name).as_deref(), Ok("1" | "true" | "yes" | "on"))
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

static TERMINATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERMINATE.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn main() -> Result<(), String> {
    // systemd stops the service with SIGTERM: leave the main loop cleanly.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    // Command line (single-threaded here, so set_var is safe): `--demo FILE`, `--rec DIR`, `--rec-fps N`.
    {
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let key = match a.as_str() {
                "--demo" => "HARBOUR_SCRIPT_FILE",
                "--rec" => "HARBOUR_REC_DIR",
                "--rec-fps" => "HARBOUR_REC_FPS",
                "--help" | "-h" => {
                    println!("harbour-shell [--demo script.txt] [--rec DIR] [--rec-fps N]   (see docs/DESIGN.md, demo/README.md)");
                    return Ok(());
                }
                _ => continue,
            };
            match args.next() {
                Some(v) => std::env::set_var(key, v),
                None => return Err(format!("{a} needs a value")),
            }
        }
    }
    if let Ok(root) = std::env::var("HARBOUR_ROOT") {
        std::env::set_current_dir(&root).map_err(|e| format!("HARBOUR_ROOT {root}: {e}"))?;
    }
    let fb_path = std::env::var("HARBOUR_FB").ok();
    let headless = env_bool("HARBOUR_HEADLESS") || fb_path.is_some();
    if headless {
        std::env::set_var("SDL_VIDEODRIVER", "dummy");
    }

    let _ = sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");
    sdl2::image::init(sdl2::image::InitFlag::PNG)?;
    let sdl = sdl2::init()?;
    sdl.mouse().show_cursor(false);
    let video = sdl.video()?;
    let scale: u32 = if headless {
        1
    } else {
        std::env::var("HARBOUR_SCALE").ok().and_then(|s| s.parse().ok()).filter(|s| (1..=6).contains(s)).unwrap_or(3)
    };
    // The picture size. On a device: from the panel (its size, physical size and HARBOUR_ROTATE),
    // scaled by whole numbers (HARBOUR_SCALE forces the scale). In a desktop window: HARBOUR_SCREEN=WxH.
    use harbour_shell::platform::display::{self as disp, PanelKind, Rotation};
    let rotation = Rotation::parse(&std::env::var("HARBOUR_ROTATE").unwrap_or_default());
    let mut panel: Option<Fb> = None;
    let mut panel_scale = 1usize;
    let desk_size = match &fb_path {
        Some(p) => {
            let d = disp::open(p).map_err(|e| format!("framebuffer {p}: {e}"))?;
            let forced = std::env::var("HARBOUR_SCALE").ok().and_then(|s| s.parse().ok());
            let ((w, h), k) = disp::choose_scale(rotation.view(d.size()), d.mm(), forced);
            eprintln!("[display] panel {:?} ({:?} mm), {:?}: picture {w}x{h} at {k}x", d.size(), d.mm(), PanelKind::from_env(d.kind()));
            panel = Some(d);
            panel_scale = k;
            (w as i32, h as i32)
        }
        None => std::env::var("HARBOUR_SCREEN").ok().and_then(|v| parse_size(&v)).unwrap_or((SCREEN_W as i32, SCREEN_H as i32)),
    };
    // A grey or black-and-white panel gets its own theme (the colour themes are not offered).
    let panel_kind = PanelKind::from_env(panel.as_ref().map_or(PanelKind::Color, |d| d.kind()));
    if panel_kind != PanelKind::Color {
        harbour_shell::launcher::settings::fix_theme_for_panel(panel_kind == PanelKind::Mono);
    }
    // How a bigger picture is used: a desk with windows when there is room for them, otherwise one
    // screen at a time over the whole picture (HARBOUR_MODE=desk|full forces one).
    let mode_env = std::env::var("HARBOUR_MODE").unwrap_or_default();
    let bigger = desk_size != (SCREEN_W as i32, SCREEN_H as i32);
    let want_desk = bigger && mode_env != "full" && (mode_env == "desk" || (desk_size.0 >= 560 && desk_size.1 >= 300));
    let full = bigger && !want_desk;
    let window = video
        .window("Harbour", desk_size.0 as u32 * scale, desk_size.1 as u32 * scale)
        .position_centered()
        .build()
        .map_err(|e| e.to_string())?;
    let mut builder = window.into_canvas();
    if headless {
        builder = builder.software();
    }
    let mut canvas = builder.build().map_err(|e| e.to_string())?;
    if scale > 1 {
        canvas.set_scale(scale as f32, scale as f32)?;
    }
    let texture_creator = canvas.texture_creator();

    // Fonts, theme, assets.
    let mut fs = FontSystem::new();
    fs.load_font(load_mplus_f12r());
    // Body text: Shinonome 12 dot (6x12 Latin, 12x12 Japanese) (M+ J12 if Shinonome is missing).
    if fs.load_jis_font("assets/fonts/shnmk12.bdf").is_ok() {
        fs.jis_height = 12;
        fs.jis_dy = 1;
        match harbour_shell::bdf::BdfFont::load("assets/fonts/shnm6x12r.bdf") {
            Ok(f) => {
                fs.body_latin = Some(f);
                fs.latin_dy = 1;
            }
            Err(e) => eprintln!("[font] shnm6x12r.bdf: {e}"),
        }
    } else if let Err(e) = fs.load_jis_font("assets/fonts/mplus_j12r.bdf") {
        eprintln!("[font] JIS BDF load failed: {e}");
    }
    let mut assets = AssetStore::new(&texture_creator)?;
    // Built-in app packages: apps/<id>/<id>.desktop, icon.png and res/ (see launcher::packages).
    {
        let pkgs = packages::scan(Path::new(packages::APPS_ROOT));
        packages::load_assets(&mut assets, &texture_creator, &pkgs);
        eprintln!("[apps] {} built-in packages", pkgs.len());
        packages::install(pkgs);
    }
    harbour_shell::launcher::worldmap::insert_map(&mut assets, &texture_creator)?;
    // ChiKareGo2 (Cavs as a fallback) for titles and the menu bar. The terminal uses the body face (Shinonome / M+).
    fs.display = harbour_shell::font::load_display_font();
    // Seven-segment digit atlases for the clock.
    for (key, path) in [
        (harbour_shell::launcher::sevenseg::BIG_KEY, "assets/atlas/segments-table-14-20.png"),
        (harbour_shell::launcher::sevenseg::SMALL_KEY, "assets/atlas/segmentsSmall-table-9-15.png"),
    ] {
        if let Err(e) = harbour_shell::launcher::sevenseg::insert_atlas(&mut assets, &texture_creator, key, path) {
            eprintln!("[atlas] {path}: {e}");
        }
    }

    // System backend and shared config.
    let sys: Arc<dyn Backend> = if cfg!(target_os = "linux") && !env_bool("HARBOUR_MOCK") {
        Arc::new(LinuxBackend::new())
    } else {
        Arc::new(MockBackend::new())
    };
    let cfg = Arc::new(Mutex::new(Config::load(config::default_path())));
    fs.display_cavs = true; // the title face is always Chicago (the Display setting was removed)
    harbour_shell::launcher::i18n::set(harbour_shell::launcher::i18n::Lang::from_index(cfg.lock().map(|c| c.get_int("harbour_lang", 0)).unwrap_or(0)));
    let theme_for = |cfg: &Arc<Mutex<Config>>| -> String {
        std::env::var("HARBOUR_THEME").unwrap_or_else(|_| {
            let (i, inv) = cfg.lock().map(|c| (c.get_int("harbour_theme", 0), c.get_int("harbour_theme_invert", 0) == 1)).unwrap_or((0, false));
            harbour_shell::launcher::settings::theme_path(i, inv)
        })
    };
    let mut tm = ThemeManager::new(Theme::load_from_yaml(&theme_for(&cfg))?);
    // Harbour's data folder (the IME and the Agent already live here). The Text Editor's note pointer
    // used to be in ~/.local/share/harbour-os: it is still read from there when the new one is missing.
    let legacy_data = home_dir().join(".local/share/harbour-os");
    let data_dir = std::env::var("HARBOUR_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".local/share/harbour"));
    let _ = std::fs::create_dir_all(&data_dir);
    let order_path = std::env::var("HARBOUR_ORDER_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".config/harbour/order.txt"));
    let app_dir = PathBuf::from(std::env::var("HARBOUR_APP_DIR").unwrap_or_else(|_| registry::DEFAULT_APP_DIR.into()));
    let workdir = app_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));

    // Apply the saved brightness / volume like the official launcher does at boot.
    {
        let (b, v) = {
            let c = cfg.lock().unwrap();
            (c.get_int("brightness", -1), c.get_int("volume", -1))
        };
        if b >= 0 {
            let max = sys.brightness().map(|(_, m)| m).unwrap_or(100);
            let _ = sys.set_brightness((b.clamp(5, 100) as u32 * max / 100).max(1));
        }
        if v >= 0 {
            let _ = sys.set_volume(v.clamp(0, 100) as u8);
        }
    }

    // Input: evdev keyboard on the device, SDL events on a desktop.
    let shared = Arc::new(Shared::default());
    let (tx, rx) = channel::<Input>();
    #[cfg(target_os = "linux")]
    if !env_bool("HARBOUR_NO_EVDEV") {
        match harbour_shell::platform::evdev::find_keyboard() {
            Some(path) => match harbour_shell::platform::evdev::spawn(path.clone(), tx.clone(), shared.clone()) {
                Ok(_) => eprintln!("[input] evdev keyboard: {path}"),
                Err(e) => eprintln!("[input] cannot open {path}: {e}"),
            },
            None => eprintln!("[input] no evdev keyboard found"),
        }
    }
    if !env_bool("HARBOUR_NO_EVDEV") {
        if let Err(e) = harbour_shell::platform::mouse::spawn(tx.clone(), shared.clone()) {
            eprintln!("[mouse] reader not started: {e}");
        }
        if let Err(e) = harbour_shell::platform::gamepad::spawn(tx.clone(), shared.clone()) {
            eprintln!("[pad] reader not started: {e}");
        }
    }
    let _ = &tx;

    // The panel behind a scaler: whole-number scaling, centred, turned, toned for grey / 1-bit.
    let mut fb: Option<Fb> = panel.take().map(|d| Box::new(disp::Scaled::new(d, (desk_size.0 as usize, desk_size.1 as usize), panel_scale, rotation)) as Fb);
    if let Some(f) = fb.as_ref() {
        f.unblank(); // a previous process may have left the panel powered down
    }

    // Background status poller (battery + Wi-Fi) for the menu strip.
    let status = Arc::new(Mutex::new(StatusView::default()));
    spawn_status(sys.clone(), status.clone());

    // Apps and home screen.
    let mut apps = load_apps(&app_dir, &order_path);
    let mut home = Home::new(Vec::new());
    home.set_folders_path(order_path.parent().map(Path::to_path_buf).unwrap_or_default().join("folders.json"));
    home.set_recent_path(order_path.parent().map(Path::to_path_buf).unwrap_or_default().join("recent.txt"));
    rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
    if cfg.lock().map(|c| c.get_int("harbour_view", 0)).unwrap_or(0) == 1 {
        home.toggle_view();
    }

    let mut screen = Screen::Home;
    let mut calc = {
        let c = cfg.lock().unwrap();
        Calc::new(CalcSettings::from_ints(c.get_int("harbour_calc_rpn", 0), c.get_int("harbour_calc_rad", 0), c.get_int("harbour_calc_fmt", 0)))
    };
    let mut world = WorldClock::new(chrono::Local::now());
    // The lock screen's own map (World Clock style), on the device's time zone.
    let mut lock_world = WorldClock::new(chrono::Local::now());
    let mut snake = SnakeState::new();
    // The note being edited: Documents/note.txt (or the old ~/.local/share/.../note.txt when only
    // that one exists) until "Save As" picks another file (remembered in `note_path.txt`).
    let note_pointer = data_dir.join("note_path.txt");
    let old_data = if std::env::var("HARBOUR_DATA").is_ok() { data_dir.clone() } else { legacy_data.clone() };
    let mut note_path = std::fs::read_to_string(&note_pointer)
        .or_else(|_| std::fs::read_to_string(old_data.join("note_path.txt")))
        .ok()
        .map(|t| PathBuf::from(t.trim()))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| fileops::default_note(&old_data.join("note.txt"), &fileops::user_dir(fileops::UserDir::Documents).join("note.txt")));
    let mut editor = TextEditor::from_text(&std::fs::read_to_string(&note_path).unwrap_or_default());
    // Dialogs shown over an app: "save changes?" and the Save As panel.
    let mut unsaved: Option<UnsavedCard> = None;
    let mut save_panel: Option<SavePanel> = None;
    let mut quit_after_save: Option<EditorNext> = None;
    // What the "Save changes?" card is asking about, and what the file panel is for.
    let mut unsaved_then = EditorNext::Home;
    let mut panel_for = PanelFor::SaveAs;
    let mut run_next: Option<EditorNext> = None;
    let mut dialog_click: Option<(i32, i32)> = None;
    let mut home_modal: Option<HomeModal> = None;
    editor.mark_clean();

    let mut toast: Option<Toast> = None;
    let mut last_input = Instant::now();
    let mut screen_off_prev: Option<u32> = None;
    // Fade-out before the screen turns off: (start, brightness before).
    let mut fade: Option<(Instant, u32)> = None;
    const FADE_MS: f32 = 600.0;
    let mut vol: (u8, bool) = sys.volume().unwrap_or((30, false));
    let mut last_time = String::new();
    let mut dirty = true;
    let mut zoom: Option<Zoom> = None;
    // Tab: short press = switch home view, hold 1 s = lock / unlock.
    let mut tab = TabGesture::default();
    // The Agent: an overlay on top of the live screen, opened by a double tap of Tab. The app behind
    // keeps running; the agent's tools act on it. A single Tab on Home waits one double-tap window
    // before it switches the view.
    let mut agent: Option<Box<AgentApp>> = None;
    let mut agent_open = false;
    let mut installer: Option<harbour_shell::launcher::installer::Installer> = None;
    let mut help: Option<harbour_shell::launcher::help::HelpView> = None;
    // 0 = closed .. 1 = fully open: the Agent slides in from the right over the current screen.
    let mut agent_prog: f32 = 0.0;
    let mut agent_prog_t = Instant::now();
    let mut home_toggle_due: Option<Instant> = None;
    let mut pending_launch: Option<Target> = None;
    let mut inject: Vec<Input> = Vec::new();
    let mut deferred: Vec<Input> = Vec::new();
    // Mouse pointer: shown while the mouse moves, hidden after a few idle seconds
    // (or when a key is pressed).
    let mut cursor = (SCREEN_W as i32 / 2, SCREEN_H as i32 / 2);
    let mut cursor_until: Option<Instant> = None;
    let mut last_mouse_move = Instant::now() - Duration::from_secs(10);
    let mut last_click: Option<(usize, Instant, i32, i32)> = None;
    if let Ok(v) = std::env::var("HARBOUR_CURSOR") {
        // Test hook: HARBOUR_CURSOR=x,y shows the pointer at a fixed place.
        let mut it = v.split(',').filter_map(|p| p.parse::<i32>().ok());
        if let (Some(x), Some(y)) = (it.next(), it.next()) {
            cursor = (x, y);
            cursor_until = Some(Instant::now() + Duration::from_secs(3600));
        }
    }
    const CURSOR_HIDE: Duration = Duration::from_secs(3);
    // Esc: a short press is the app's own "back"; holding it for about a second
    // leaves any built-in app and returns to the home screen.
    let mut esc = HoldKey::default();
    let mut locked = env_bool("HARBOUR_START_LOCKED");
    let mut app_menu: Option<harbour_shell::launcher::widgets::AppMenu> = None;
    let mut hold_gauge: Option<(&'static str, f32)>;
    let mut mouse_ev: Option<SettingsEvent> = None;
    let mut left_held = false;
    let mut unlock = 0.0f32; // scroll-to-unlock progress 0..1
    let mut unlock_at = Instant::now();
    let mut last_sec = 99u32;
    let mut force_full = false;
    // The SPI panel can drop partial updates when frames come quickly (ghosts after an
    // animation). Once things settle, rewrite the whole frame once.
    let mut refresh_due: Option<Instant> = None;
    let mut prev_home = true;
    let mut next_scan = Instant::now() + Duration::from_secs(3);

    let mut script = Script::from_env();
    // Japanese input method (Ctrl+Space toggles; see memos/japanese-ime-plan.md). One seam: its
    // `process` below turns typed keys into `Input::Text` before anything else sees them.
    let mut ime = harbour_shell::launcher::ime_glue::ImeGlue::from_config(&cfg.lock().unwrap());
    if ime.enabled() {
        // The dictionary is optional and user-installed (harbourctl ime-install); without it: kana only.
        eprintln!("{}", ime.load_data(&harbour_shell::launcher::ime_glue::data_dir()));
    }
    let mut rec = Recorder::from_env();
    // Gamepads in a desktop window (SDL's game controllers); on a device they are read from evdev.
    let controllers = if headless { None } else { sdl.game_controller().ok() };
    let mut pads: Vec<sdl2::controller::GameController> = Vec::new();
    let mut stick_axes = (0i16, 0i16);
    let mut events = sdl.event_pump()?;
    // In a desktop window the system draws the pointer, in Harbour's arrow: it moves smoothly
    // however long a frame takes to draw (the drawn pointer waits for the next frame).
    let mut os_cursor_keep = if headless { None } else { make_os_cursor(&tm, scale) };
    let os_cursor = os_cursor_keep.is_some();
    // The theme's pointer colours the system cursor was made with (remade when they change).
    let mut cursor_colors = (tm.get_color("foreground"), tm.get_color("widget_face_active"));
    if os_cursor {
        sdl.mouse().show_cursor(true);
    }
    // The desk (a bigger screen): Home is the desk itself, every other screen a window. `fid` is the
    // focused window, whose screen is `screen` (None: Home has the focus and `screen` is Home); the
    // other windows keep their screen in their slot.
    let mut desk: Option<Desk<Screen>> = want_desk.then(|| Desk::new(desk_size));
    // Gamepad: the on-screen keyboard (X opens it), the stick's position and its leftover fraction of
    // a pixel, when the stick last moved the pointer (A clicks while it shows), what A's press was.
    let mut osk: Option<harbour_shell::launcher::osk::Osk> = None;
    let mut stick: (i16, i16) = (0, 0);
    let mut stick_frac = (0.0f32, 0.0f32);
    let mut stick_t = Instant::now();
    let mut pad_pointer_until = Instant::now();
    let mut pad_a_click = false;
    let mut fid: Option<WinId> = None;
    home.set_desk(desk.is_some());
    // Zoom rectangles on the desk: (progress, from, to, the window to show once it ends).
    let mut desk_zoom: Option<(f32, Rect, Rect, Option<WinId>)> = None;
    // Starts where a script's pointer starts, so scripted clicks give desk coordinates.
    let mut dcur = (SCREEN_W as i32 / 2, SCREEN_H as i32 / 2);

    'main: loop {
        if TERMINATE.load(std::sync::atomic::Ordering::SeqCst) {
            break 'main;
        }
        // ---- gather input ------------------------------------------------------
        let mut inputs: Vec<Input> = Vec::new();
        // In a desktop window the pointer follows the system mouse's absolute position (relative
        // steps divided by the scale lose their remainders and drift away from it).
        let mut abs_at = if desk.is_some() || full { dcur } else { cursor };
        for e in events.poll_iter() {
            match e {
                Event::Quit { .. } => inputs.push(Input::Quit),
                Event::KeyDown { keycode: Some(key), keymod, repeat, .. } => {
                    inputs.push(Input::Key { key, mods: keymod, repeat })
                }
                Event::KeyUp { keycode: Some(key), .. } => inputs.push(Input::KeyUp { key }),
                Event::TextInput { text, .. } => inputs.push(Input::Text(text)),
                Event::MouseMotion { x, y, .. } => {
                    let p = (x / scale as i32, y / scale as i32);
                    let (dx, dy) = (p.0 - abs_at.0, p.1 - abs_at.1);
                    abs_at = p;
                    if dx != 0 || dy != 0 {
                        inputs.push(Input::MouseMove { dx, dy });
                    }
                }
                Event::MouseButtonDown { mouse_btn, .. } | Event::MouseButtonUp { mouse_btn, .. } => {
                    let down = matches!(e, Event::MouseButtonDown { .. });
                    let button = match mouse_btn {
                        sdl2::mouse::MouseButton::Left => 0,
                        sdl2::mouse::MouseButton::Right => 1,
                        _ => 2,
                    };
                    inputs.push(Input::MouseButton { button, down });
                }
                Event::MouseWheel { y, .. } => inputs.push(Input::MouseWheel { dy: y }),
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Some(c) = controllers.as_ref().and_then(|g| g.open(which).ok()) {
                        eprintln!("[pad] {}", c.name());
                        pads.push(c);
                    }
                }
                Event::ControllerButtonDown { button, .. } | Event::ControllerButtonUp { button, .. } => {
                    use harbour_shell::launcher::osk::Pad;
                    use sdl2::controller::Button as B;
                    let down = matches!(e, Event::ControllerButtonDown { .. });
                    let pad = match button {
                        B::DPadUp => Some(Pad::Up),
                        B::DPadDown => Some(Pad::Down),
                        B::DPadLeft => Some(Pad::Left),
                        B::DPadRight => Some(Pad::Right),
                        B::A => Some(Pad::A),
                        B::B => Some(Pad::B),
                        B::X => Some(Pad::X),
                        B::Y => Some(Pad::Y),
                        B::LeftShoulder => Some(Pad::L),
                        B::RightShoulder => Some(Pad::R),
                        B::Start => Some(Pad::Start),
                        B::Back => Some(Pad::Select),
                        B::LeftStick => Some(Pad::Stick),
                        _ => None,
                    };
                    if let Some(pad) = pad {
                        inputs.push(Input::Pad { pad, down });
                    }
                }
                Event::ControllerAxisMotion { axis, value, .. } => {
                    use sdl2::controller::Axis;
                    let v = (value as i32 * 1000 / 32767).clamp(-1000, 1000) as i16;
                    match axis {
                        Axis::LeftX => stick_axes.0 = v,
                        Axis::LeftY => stick_axes.1 = v,
                        _ => continue,
                    }
                    inputs.push(Input::Stick { x: stick_axes.0, y: stick_axes.1 });
                }
                _ => {}
            }
        }
        inputs.extend(rx.try_iter());
        if let Some(s) = script.as_mut() {
            match s.step() {
                ScriptStep::Done => {
                    if s.failed > 0 {
                        eprintln!("script: {} expect step(s) failed", s.failed);
                        std::process::exit(3);
                    }
                    break 'main;
                }
                ScriptStep::Expect(spec) => {
                    // `expect:agent=1,menu=0` style checks for headless tests (also `state` to print).
                    let scr = match &screen {
                        Screen::Home => "home".to_string(),
                        Screen::Settings(_) => "settings".to_string(),
                        Screen::Calculator => "calculator".to_string(),
                        Screen::Clock => "clock".to_string(),
                        Screen::Snake => "snake".to_string(),
                        Screen::Editor => "editor".to_string(),
                        Screen::Terminal(_) => "terminal".to_string(),
                        Screen::App(a) => normalized(&a.title()),
                    };
                    let b = |v: bool| if v { "1" } else { "0" };
                    let state = [
                        ("screen", scr),
                        ("agent", b(agent_open).to_string()),
                        ("menu", b(app_menu.is_some()).to_string()),
                        ("modal", b(home_modal.is_some() || save_panel.is_some() || unsaved.is_some()).to_string()),
                        ("locked", b(locked).to_string()),
                        ("zoom", b(zoom.is_some()).to_string()),
                        ("windows", desk.as_ref().map_or(0, |d| d.len()).to_string()),
                    ];
                    let line = state.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
                    let desc = if let Screen::App(a) = &screen { a.describe().to_lowercase() } else { String::new() };
                    for want in spec.split_whitespace() {
                        let Some((k, v)) = want.split_once('=') else { continue };
                        // has=word: the app's own description (what the Agent reads) mentions it.
                        if k == "has" {
                            if !desc.contains(&v.to_lowercase()) {
                                eprintln!("script: expect {want} failed at step {}: {desc}", s.pos);
                                s.failed += 1;
                            }
                            continue;
                        }
                        let got = state.iter().find(|(n, _)| *n == k).map(|(_, g)| g.as_str());
                        if got != Some(v) {
                            eprintln!("script: expect {want} failed at step {}: {line}", s.pos);
                            s.failed += 1;
                        }
                    }
                    if spec.is_empty() {
                        eprintln!("state: {line}");
                    }
                }
                ScriptStep::Input(v) => inputs.extend(v),
                ScriptStep::Theme(i, invert) => {
                    if let Ok(mut c) = cfg.lock() {
                        c.set_int("harbour_theme", i.clamp(0, 3));
                        c.set_int("harbour_theme_invert", invert as i64);
                        let _ = c.save();
                    }
                    match Theme::load_from_yaml(&theme_for(&cfg)) {
                        Ok(t) => tm = ThemeManager::new(t),
                        Err(e) => eprintln!("script: theme: {e}"),
                    }
                    rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                    dirty = true;
                }
                ScriptStep::Goto(name) => {
                    if matches!(screen, Screen::Home) {
                        // An id ("voicememo"), a name in either language or an old name ("Recorder").
                        let want = normalized(&name);
                        let pkg = packages::find(&name).map(|p| p.id);
                        match home.items.iter().position(|i| normalized(&i.name) == want || pkg.as_deref() == Some(i.key().as_str())) {
                            Some(idx) => {
                                home.select(idx);
                                dirty = true;
                            }
                            None => eprintln!("script: goto: no tile named {name:?}"),
                        }
                    }
                }
                ScriptStep::ThemeUi(n) => {
                    // Walk the Theme row (Settings > Theme) to palette n with Left/Right, as a person would.
                    let cur = cfg.lock().map(|c| c.get_int("harbour_theme", 0)).unwrap_or(0);
                    let key = if n >= cur { Keycode::Right } else { Keycode::Left };
                    for _ in 0..(n.clamp(0, 3) - cur).abs() {
                        inputs.push(Input::Key { key, mods: Mod::NOMOD, repeat: false });
                        inputs.push(Input::KeyUp { key });
                    }
                }
                ScriptStep::Idle => {}
            }
            if s.pending_shot.is_some() {
                dirty = true;
            }
        }

        // Keys the agent presses go to the app behind it (the chat is bypassed for that frame);
        // real input that arrives meanwhile waits one frame.
        let mut agent_restore: Option<bool> = None;
        if !inject.is_empty() && !locked {
            deferred.extend(inputs.drain(..));
            inputs = std::mem::take(&mut inject);
            agent_restore = Some(agent_open);
            agent_open = false;
        } else if !deferred.is_empty() {
            let mut d = std::mem::take(&mut deferred);
            d.extend(inputs.drain(..));
            inputs = d;
        }

        // ---- screen-off: the first input only wakes the display -------------------
        if !inputs.is_empty() {
            last_input = Instant::now();
            dirty = true;
            if let Some((_, v)) = fade.take() {
                let _ = sys.set_brightness(v); // touched during the fade: back to full
            }
            if let Some(prev) = screen_off_prev.take() {
                if let Some(f) = fb.as_mut() {
                    f.unblank();
                    f.invalidate();
                }
                force_full = true;
                let _ = sys.set_brightness(prev);
                inputs.clear();
            }
        }

        // ---- gamepad: the stick moves the pointer; the buttons are keys, or the on-screen keyboard's ----
        if desk.is_none() && !full {
            harbour_shell::grid::set_surface(desk_size.0, desk_size.1);
        }
        {
            use harbour_shell::launcher::osk::{Out as OskOut, Osk, Pad};
            let now = Instant::now();
            let dt = now.duration_since(stick_t).as_secs_f32().min(0.1);
            stick_t = now;
            let key = |k: Keycode, down: bool| if down { Input::Key { key: k, mods: Mod::NOMOD, repeat: false } } else { Input::KeyUp { key: k } };
            if let Some(k) = osk.as_mut() {
                k.set_ime(!ime.mode().is_direct());
            }
            // Where the pointer is on the whole picture, following this frame's moves (for clicks on the keyboard).
            let mut at = if desk.is_some() || full { dcur } else { cursor };
            let mut out = Vec::with_capacity(inputs.len());
            for i in std::mem::take(&mut inputs) {
                match i {
                    Input::Stick { x, y } => stick = (x, y),
                    Input::MouseMove { dx, dy } => {
                        at = (at.0 + dx, at.1 + dy);
                        out.push(i);
                    }
                    Input::MouseButton { button: 0, down: true } if !locked && osk.is_some() && harbour_shell::grid::with_surface(desk_size, || Osk::rect().contains_point(at)) => {
                        dirty = true;
                        let r = harbour_shell::grid::with_surface(desk_size, || osk.as_mut().map(|k| k.click(at.0, at.1)));
                        match r {
                            Some(OskOut::Send(v)) => out.extend(v),
                            Some(OskOut::Close) => osk = None,
                            _ => {}
                        }
                    }
                    Input::Pad { pad, down } if osk.is_some() && !locked => {
                        dirty = true;
                        if down {
                            match osk.as_mut().map(|k| k.pad(pad)) {
                                Some(OskOut::Send(v)) => out.extend(v),
                                Some(OskOut::Close) => osk = None,
                                _ => {}
                            }
                        }
                    }
                    Input::Pad { pad, down } => {
                        dirty = true;
                        match pad {
                            Pad::Up => out.push(key(Keycode::Up, down)),
                            Pad::Down => out.push(key(Keycode::Down, down)),
                            Pad::Left => out.push(key(Keycode::Left, down)),
                            Pad::Right => out.push(key(Keycode::Right, down)),
                            // A clicks while the stick's pointer shows; otherwise it is Return.
                            Pad::A => {
                                if down {
                                    pad_a_click = now < pad_pointer_until;
                                }
                                out.push(if pad_a_click { Input::MouseButton { button: 0, down } } else { key(Keycode::Return, down) });
                            }
                            Pad::Stick => out.push(Input::MouseButton { button: 0, down }),
                            // B is Esc: held, it leaves the app like Esc held.
                            Pad::B => out.push(key(Keycode::Escape, down)),
                            Pad::Y => out.push(Input::MouseButton { button: 1, down }),
                            Pad::L | Pad::R if down => {
                                let mods = if pad == Pad::L { Mod::LSHIFTMOD } else { Mod::NOMOD };
                                out.push(Input::Key { key: Keycode::Tab, mods, repeat: false });
                                out.push(Input::KeyUp { key: Keycode::Tab });
                            }
                            Pad::X if down && !locked => osk = Some(Osk::new(!ime.mode().is_direct())),
                            Pad::Start if down => out.push(Input::Menu),
                            // Select: the Harbour menu (the app menu, then Left to the menu beside it).
                            Pad::Select if down => {
                                out.push(Input::Menu);
                                out.push(key(Keycode::Left, true));
                                out.push(key(Keycode::Left, false));
                            }
                            _ => {}
                        }
                    }
                    other => out.push(other),
                }
            }
            inputs = out;
            // The stick: a dead zone, then speed growing with the push (up to about 400 px/s).
            let (sx, sy) = (stick.0 as f32 / 1000.0, stick.1 as f32 / 1000.0);
            let m = (sx * sx + sy * sy).sqrt();
            if m > 0.18 {
                let speed = 30.0 + 370.0 * ((m - 0.18) / 0.82).min(1.0).powi(2);
                stick_frac.0 += sx / m * speed * dt;
                stick_frac.1 += sy / m * speed * dt;
                let (dx, dy) = (stick_frac.0.trunc() as i32, stick_frac.1.trunc() as i32);
                stick_frac.0 -= dx as f32;
                stick_frac.1 -= dy as f32;
                if dx != 0 || dy != 0 {
                    inputs.push(Input::MouseMove { dx, dy });
                }
                pad_pointer_until = now + Duration::from_secs(3);
                dirty = true;
            } else {
                stick_frac = (0.0, 0.0);
            }
        }

        // ---- desk: the pointer moves on the desk; presses focus, move, resize and close windows ----
        let mut desk_close: Option<WinId> = None;
        // When the screens below get window coordinates: the window's corner on the desk, so a menu
        // they open at the pointer (a right click) is moved onto the desk, where menus are drawn.
        let mut menu_shift: Option<(i32, i32)> = None;
        let mut menu_before = true;
        // A click on a title bar: that window's menu opens at the click (desk coordinates).
        let mut desk_menu_at: Option<(i32, i32)> = None;
        if let Some(d) = desk.as_mut() {
            d.reserve_right = harbour_shell::grid::with_surface(d.size, || home.desk_icons_w());
            let mut kept = Vec::with_capacity(inputs.len());
            // The moves handed on: the screens below add them to `cursor` themselves.
            let mut passed = (0, 0);
            for i in std::mem::take(&mut inputs) {
                match i {
                    Input::MouseMove { dx, dy } => {
                        dcur = ((dcur.0 + dx).clamp(0, d.size.0 - 1), (dcur.1 + dy).clamp(0, d.size.1 - 1));
                        cursor_until = Some(Instant::now() + CURSOR_HIDE);
                        dirty = true;
                        if !d.motion(dcur.0, dcur.1) {
                            passed = (passed.0 + dx, passed.1 + dy);
                            kept.push(i);
                        }
                    }
                    // While a menu is open every press goes to it (it closes on a press outside).
                    Input::MouseButton { button: 0, down: true } if !locked && app_menu.is_none() => {
                        dirty = true;
                        match d.press(dcur.0, dcur.1) {
                            Press::Pass(..) | Press::Desk | Press::Bar => kept.push(i),
                            Press::Focus(_) | Press::Taken => {}
                        }
                    }
                    Input::MouseButton { button: 0, down: false } if !locked => match d.release(dcur.0, dcur.1) {
                        Release::Pass => kept.push(i),
                        Release::Close(id) => desk_close = Some(id),
                        Release::Title(x, y) => desk_menu_at = Some((x, y)),
                        Release::Taken => {}
                    },
                    Input::MouseButton { down: true, .. } | Input::MouseWheel { .. } if !locked && app_menu.is_none() => match d.hit(dcur.0, dcur.1) {
                        Some(id) if Some(id) == d.focused() => kept.push(i),
                        Some(id) if matches!(i, Input::MouseButton { .. }) => {
                            d.raise(id);
                            dirty = true;
                        }
                        Some(_) => {}
                        None => {
                            if matches!(i, Input::MouseButton { .. }) {
                                d.focus_desk();
                            }
                            kept.push(i);
                        }
                    },
                    Input::Key { key: Keycode::Backquote, mods, .. } if !locked && mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) => {
                        d.cycle();
                        dirty = true;
                    }
                    other => kept.push(other),
                }
            }
            inputs = kept;
            // The close box: that window comes to the front, then quits as from its menu's Quit.
            if let Some(id) = desk_close {
                d.raise(id);
            }
            // Focus moved: the focused window's screen comes into `screen`, the old one into its slot.
            let want = d.focused();
            if want != fid {
                let incoming = match want {
                    Some(id) => d.get_mut(id).and_then(|w| w.slot.take()),
                    None => Some(Screen::Home),
                };
                if let Some(inc) = incoming {
                    let out = std::mem::replace(&mut screen, inc);
                    if let Some(w) = fid.and_then(|f| d.get_mut(f)) {
                        w.slot = Some(out);
                    }
                    fid = want;
                    prev_home = matches!(screen, Screen::Home);
                    app_menu = None;
                    dirty = true;
                }
            }
            // Coordinates: a window's own while the pointer is over the focused window; the desk's for
            // Home, the desk's strip and its menus. Decided by where the pointer ends this frame; the
            // start is set back by the moves handed on, so it ends there.
            let d0 = (dcur.0 - passed.0, dcur.1 - passed.1);
            let over_bar = dcur.1 < chrome::BAR_H;
            match d.focused_rect() {
                Some(r) if app_menu.is_none() && !over_bar => {
                    harbour_shell::grid::set_surface(r.width() as i32, r.height() as i32);
                    cursor = (d0.0 - r.x(), d0.1 - r.y());
                    menu_shift = Some((r.x(), r.y()));
                }
                // The desk's strip and the menus that drop from it are laid out on the whole desk.
                Some(_) => {
                    harbour_shell::grid::set_surface(d.size.0, d.size.1);
                    cursor = d0;
                }
                None => {
                    harbour_shell::grid::set_surface(d.size.0, d.size.1);
                    cursor = d0;
                }
            }
            if let Some((x, y)) = desk_menu_at.filter(|_| desk_close.is_none()) {
                harbour_shell::grid::set_surface(d.size.0, d.size.1);
                let mut m = open_menu(&screen, &home, None);
                m.origin = Some((x, y));
                app_menu = Some(m);
                dirty = true;
            }
            if desk_close.is_some() {
                let mut m = open_menu(&screen, &home, None);
                m.cursor = m.items.len().saturating_sub(1);
                app_menu = Some(m);
                inputs.insert(0, Input::Key { key: Keycode::Return, mods: Mod::NOMOD, repeat: false });
            }
            menu_before = app_menu.is_some();
        }

        // ---- full: one screen over a bigger picture; a fixed-size screen sits in the middle ----
        let full_vp = if full { full_viewport(&screen, locked, desk_size) } else { None };
        if full {
            let mut passed = (0, 0);
            for i in &inputs {
                if let Input::MouseMove { dx, dy } = i {
                    dcur = ((dcur.0 + dx).clamp(0, desk_size.0 - 1), (dcur.1 + dy).clamp(0, desk_size.1 - 1));
                    passed = (passed.0 + dx, passed.1 + dy);
                }
            }
            let o = full_vp.map_or((0, 0), |r| (r.x(), r.y()));
            cursor = (dcur.0 - passed.0 - o.0, dcur.1 - passed.1 - o.1);
            match full_vp {
                Some(r) => harbour_shell::grid::set_surface(r.width() as i32, r.height() as i32),
                None => harbour_shell::grid::set_surface(desk_size.0, desk_size.1),
            };
        }

        // ---- the keyboard's room: a screen that lays itself out ends above the on-screen keyboard ----
        let osk_cut = osk_room(osk.is_some(), desk.is_some(), locked, &screen, desk_size);
        if osk_cut > 0 {
            harbour_shell::grid::set_surface(desk_size.0, desk_size.1 - osk_cut);
        }

        // ---- the install card: over everything, it takes every key, typed text and click ----
        if installer.is_none() {
            if let Some(req) = INSTALL_REQ.lock().unwrap().take() {
                installer = Some(harbour_shell::launcher::installer::Installer::new(req));
                dirty = true;
            }
        }
        if let Some(inst) = installer.as_mut() {
            use harbour_shell::launcher::installer::Out;
            let mut out = Out::Stay;
            let mut rest = Vec::new();
            for i in std::mem::take(&mut inputs) {
                match i {
                    Input::Key { key, .. } => out = inst.key(key),
                    Input::Text(t) => inst.text(&t),
                    Input::MouseButton { button: 0, down: true } => out = inst.click(cursor.0, cursor.1),
                    Input::MouseMove { dx, dy } => {
                        cursor.0 = (cursor.0 + dx).clamp(0, chrome::screen_w() as i32 - 1);
                        cursor.1 = (cursor.1 + dy).clamp(0, chrome::screen_h() as i32 - 1);
                        cursor_until = Some(Instant::now() + Duration::from_secs(3));
                    }
                    other @ (Input::Quit | Input::Media(_)) => rest.push(other),
                    _ => {}
                }
                dirty = true;
                if out != Out::Stay {
                    break;
                }
            }
            inputs = rest;
            if inst.tick() {
                dirty = true;
            }
            if let Out::Close { installed } = out {
                if installed && inst.req.job == harbour_shell::launcher::installer::Job::ImeDict {
                    eprintln!("{}", ime.load_data(&harbour_shell::launcher::ime_glue::data_dir()));
                }
                if installed && inst.req.removes() {
                    apps = load_apps(&app_dir, &order_path);
                    rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                    if let Screen::Settings(st) = &mut screen {
                        st.set_apps(app_refs(&apps));
                    }
                }
                installer = None;
                dirty = true;
                force_full = true;
            }
        }

        // ---- Help (Harbour menu > Help): scrolls with the keys, closes with Esc / Return ----
        if installer.is_none() {
            if let Some(h) = help.as_mut() {
                let mut close = false;
                let mut rest = Vec::new();
                for i in std::mem::take(&mut inputs) {
                    match i {
                        Input::Key { key, .. } => close |= h.key(harbour_shell::platform::keys::arrow_alias(key)),
                        Input::MouseButton { button: 0, down: true } => h.click(cursor.0, cursor.1),
                        Input::MouseButton { button: 1, down: true } | Input::Menu => close = true,
                        Input::MouseWheel { dy } => h.wheel(dy),
                        Input::MouseMove { dx, dy } => {
                            cursor.0 = (cursor.0 + dx).clamp(0, chrome::screen_w() as i32 - 1);
                            cursor.1 = (cursor.1 + dy).clamp(0, chrome::screen_h() as i32 - 1);
                            cursor_until = Some(Instant::now() + Duration::from_secs(3));
                        }
                        other @ (Input::Quit | Input::Media(_)) => rest.push(other),
                        _ => {}
                    }
                    dirty = true;
                }
                inputs = rest;
                if close {
                    help = None;
                    force_full = true;
                }
            }
        }

        // Keys are ignored while a window is zooming open or closed.
        if zoom.is_some() {
            inputs.retain(|i| matches!(i, Input::Quit | Input::Media(_) | Input::KeyUp { .. } | Input::MouseMove { .. }));
        }

        // ---- mouse: move the pointer; turn clicks and the wheel into selections / keys ----
        {
            let now = Instant::now();
            let key = |k: Keycode| Input::Key { key: k, mods: Mod::NOMOD, repeat: false };
            let mut expanded: Vec<Input> = Vec::with_capacity(inputs.len());
            for input in inputs {
                match input {
                    Input::MouseMove { dx, dy } => {
                        last_mouse_move = Instant::now();
                        cursor.0 = (cursor.0 + dx).clamp(0, chrome::screen_w() as i32 - 1);
                        cursor.1 = (cursor.1 + dy).clamp(0, chrome::screen_h() as i32 - 1);
                        if agent_open && !locked && zoom.is_none() && app_menu.is_none() && home_modal.is_none() {
                            if let Some(ag) = agent.as_mut() {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                let ev = if left_held { PointerEvent::Drag { x: cursor.0, y: cursor.1, dx, dy } }
                                    else { PointerEvent::Move { x: cursor.0, y: cursor.1 } };
                                ag.pointer(ev, &ctx);
                            }
                        } else if !locked && zoom.is_none() && app_menu.is_none() && home_modal.is_none() {
                            if !left_held {
                                if let Screen::Settings(st) = &mut screen {
                                    st.hover(cursor.0, cursor.1);
                                }
                            }
                            if left_held {
                                if let Screen::Settings(st) = &mut screen {
                                    st.drag(cursor.0, cursor.1);
                                }
                                if matches!(screen, Screen::Home) {
                                    home.drag_move(cursor.0, cursor.1);
                                }
                            }
                            if let Screen::App(a) = &mut screen {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                let ev = if left_held { PointerEvent::Drag { x: cursor.0, y: cursor.1, dx, dy } }
                                    else { PointerEvent::Move { x: cursor.0, y: cursor.1 } };
                                let action = a.pointer(ev, &ctx);
                                apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                            }
                        }
                        if let Some(m) = &mut app_menu {
                            if let Some(i) = m.hit(&fs, cursor.0, cursor.1) {
                                m.cursor = i;
                            }
                        }
                        cursor_until = Some(now + CURSOR_HIDE);
                        dirty = true;
                    }
                    Input::MouseButton { button, down: true } => {
                        cursor_until = Some(now + CURSOR_HIDE);
                        dirty = true;
                        if locked || zoom.is_some() {
                            continue;
                        }
                        if (save_panel.is_some() || unsaved.is_some() || home_modal.is_some()) && button == 0 {
                            dialog_click = Some((cursor.0, cursor.1));
                            expanded.push(key(Keycode::F24));
                            continue;
                        }
                        if let (Some(m), 0) = (&mut app_menu, button) {
                            match m.hit(&fs, cursor.0, cursor.1) {
                                Some(i) => {
                                    m.cursor = i;
                                    expanded.push(key(Keycode::Return));
                                }
                                None => {
                                    // A synthetic Esc must also "release": without it the hold-Esc timer keeps
                                    // running and closes the app about a second later.
                                    expanded.push(key(Keycode::Escape));
                                    expanded.push(Input::KeyUp { key: Keycode::Escape });
                                }
                            }
                            continue;
                        }
                        // The status items at the right of the strip (input method, sound, internet, network,
                        // battery, clock) each open a small menu under themselves.
                        if button == 0 && app_menu.is_none() && !agent_open && cursor.1 < chrome::BAR_H {
                            let bar = desk.as_ref().map(|d| d.size).unwrap_or((harbour_shell::grid::sw(), harbour_shell::grid::sh()));
                            let mut st = status.lock().map(|s| s.clone()).unwrap_or_default();
                            st.time = chrono::Local::now().format("%H:%M").to_string();
                            let text_screen = agent_open || matches!(screen, Screen::Editor | Screen::Terminal(_) | Screen::Settings(_) | Screen::App(_));
                            st.ime = ime.indicator(text_screen).to_string();
                            let hit = harbour_shell::grid::with_surface(bar, || chrome::status_hit(&fs, &st, cursor.0));
                            if let Some((item, x)) = hit {
                                app_menu = Some(harbour_shell::grid::with_surface(bar, || status_menu(item, &st, x, ime.enabled())));
                                continue;
                            }
                        }
                        // The Harbour anchor at the far left opens the system menu; the app name next to it
                        // opens the app menu (Quit etc.), like Alt.
                        if button == 0 && app_menu.is_none() && !agent_open && cursor.1 < chrome::BAR_H && cursor.0 < 20 {
                            let mut m = system_menu(&screen);
                            if desk.is_some() {
                                m.origin = Some((cursor.0.max(0), chrome::BAR_H));
                            }
                            app_menu = Some(m);
                            continue;
                        }
                        if button == 0 && app_menu.is_none() && cursor.1 < chrome::BAR_H && cursor.0 < 150 {
                            let mut m = open_menu(&screen, &home, agent_open.then(|| agent.as_deref()).flatten());
                            // On a desk the menu opens where the strip was clicked.
                            if desk.is_some() {
                                m.origin = Some((cursor.0, chrome::BAR_H));
                            }
                            app_menu = Some(m);
                            continue;
                        }
                        if agent_open && button == 0 {
                            left_held = true;
                            if let Some(ag) = agent.as_mut() {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                ag.pointer(PointerEvent::Down { x: cursor.0, y: cursor.1, button: 0, clicks: 1 }, &ctx);
                            }
                            continue;
                        }
                        match button {
                            0 => match &mut screen {
                                Screen::Home => {
                                    if let Some(i) = home.hit(cursor.0, cursor.1) {
                                        let double = is_double_click(last_click, i, cursor.0, cursor.1);
                                        home.select(i);
                                        left_held = true;
                                        home.press(i, cursor.0, cursor.1);
                                        if double {
                                            expanded.push(key(Keycode::Return));
                                            last_click = None;
                                        } else {
                                            last_click = Some((i, now, cursor.0, cursor.1));
                                        }
                                    } else if home.sb_press(cursor.0, cursor.1) {
                                        left_held = true;
                                    }
                                }
                                Screen::Calculator => {
                                    if calc.click(cursor.0, cursor.1, 0, chrome::BAR_H, chrome::screen_w()) == CalcEvent::SettingsChanged {
                                        let (rpn, rad, fmt) = calc.settings.to_ints();
                                        if let Ok(mut c) = cfg.lock() {
                                            c.set_int("harbour_calc_rpn", rpn);
                                            c.set_int("harbour_calc_rad", rad);
                                            c.set_int("harbour_calc_fmt", fmt);
                                            let _ = c.save();
                                        }
                                    }
                                }
                                Screen::Editor => editor.click(cursor.0, cursor.1, &fs),
                                Screen::Clock => expanded.push(key(if cursor.0 < chrome::screen_w() as i32 / 2 { Keycode::Left } else { Keycode::Right })),
                                Screen::Settings(st) => {
                                    left_held = true;
                                    let clicks = if is_double_click(last_click, usize::MAX, cursor.0, cursor.1) { 2 } else { 1 };
                                    last_click = if clicks == 2 { None } else { Some((usize::MAX, now, cursor.0, cursor.1)) };
                                    mouse_ev = Some(st.click(cursor.0, cursor.1, clicks));
                                    expanded.push(key(Keycode::F24));
                                }
                                Screen::App(a) => {
                                    left_held = true;
                                    let clicks = if is_double_click(last_click, usize::MAX - 1, cursor.0, cursor.1) { 2 } else { 1 };
                                    last_click = if clicks == 2 { None } else { Some((usize::MAX - 1, now, cursor.0, cursor.1)) };
                                    let ctx = harbour_shell::launcher::app::Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                    let action = a.pointer(PointerEvent::Down { x: cursor.0, y: cursor.1, button: 0, clicks }, &ctx);
                                    apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                                }
                                _ => {}
                            },
                            // Right button: a context menu where the screen has one, else the app menu
                            // (in a Home folder, empty space = back).
                            1 => {
                                use harbour_shell::launcher::widgets::{AppMenu, MenuItem};
                                let (px, py) = cursor;
                                if app_menu.is_some() {
                                    app_menu = None;
                                } else if home_modal.is_some() {
                                    expanded.push(key(Keycode::Escape));
                                } else if agent_open {
                                    app_menu = Some(open_menu(&screen, &home, agent.as_deref()));
                                } else if matches!(screen, Screen::Home) {
                                    let hit = home.hit(px, py);
                                    if hit.is_none() && home.in_folder() {
                                        expanded.push(key(Keycode::Escape));
                                        expanded.push(Input::KeyUp { key: Keycode::Escape });
                                    } else {
                                        if let Some(i) = hit {
                                            home.select(i);
                                        }
                                        let items = home.context_entries(hit.is_some()).into_iter().map(|(id, l)| MenuItem::new(id, l)).collect();
                                        app_menu = Some(AppMenu::context(items, px, py));
                                    }
                                } else if let Screen::App(a) = &mut screen {
                                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                    app_menu = Some(match a.context_menu(px, py, &ctx).filter(|v| !v.is_empty()) {
                                        Some(items) => AppMenu::context(items, px, py),
                                        None => open_menu(&screen, &home, None),
                                    });
                                } else if matches!(screen, Screen::Editor) {
                                    let items = vec![MenuItem::new("save", "Save"), MenuItem::new("saveas", "Save As..."), MenuItem::new("undo", "Undo")];
                                    app_menu = Some(AppMenu::context(items, px, py));
                                } else {
                                    app_menu = Some(open_menu(&screen, &home, None));
                                }
                            }
                            _ => {}
                        }
                    }
                    Input::MouseButton { down: false, button } => {
                        if agent_open {
                            if button == 0 {
                                left_held = false;
                                if let Some(ag) = agent.as_mut() {
                                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                    ag.pointer(PointerEvent::Up { x: cursor.0, y: cursor.1, button: 0 }, &ctx);
                                }
                            }
                            continue;
                        }
                        if button == 0 {
                            left_held = false;
                            if matches!(screen, Screen::Home) && zoom.is_none() {
                                if home.release(cursor.0, cursor.1) == DragEnd::Moved {
                                    dirty = true;
                                }
                            }
                            if let Screen::Settings(st) = &mut screen {
                                mouse_ev = Some(st.release(cursor.0, cursor.1));
                                expanded.push(key(Keycode::F24));
                            } else if let Screen::App(a) = &mut screen {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                let action = a.pointer(PointerEvent::Up { x: cursor.0, y: cursor.1, button: 0 }, &ctx);
                                apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                            }
                        }
                    }
                    Input::MouseWheel { dy } => {
                        cursor_until = Some(now + CURSOR_HIDE);
                        dirty = true;
                        if let Some(p) = &mut save_panel {
                            p.wheel(dy);
                        } else if let Some(HomeModal::Move(p, _)) = &mut home_modal {
                            p.wheel(dy);
                        } else if home_modal.is_some() {
                        } else if locked {
                            unlock = (unlock + 0.25 * dy.unsigned_abs().min(3) as f32).min(1.0);
                            unlock_at = now;
                            if unlock >= 1.0 {
                                locked = false;
                                unlock = 0.0;
                            }
                        } else if app_menu.is_some() {
                            for _ in 0..dy.unsigned_abs().min(3) {
                                expanded.push(key(if dy > 0 { Keycode::Up } else { Keycode::Down }));
                            }
                        } else if agent_open && zoom.is_none() {
                            if let Some(ag) = agent.as_mut() {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                ag.pointer(PointerEvent::Wheel { x: cursor.0, y: cursor.1, dy }, &ctx);
                            }
                        } else if let (Screen::Settings(st), true) = (&mut screen, zoom.is_none()) {
                            mouse_ev = Some(st.wheel(-dy));
                            expanded.push(key(Keycode::F24));
                        } else if zoom.is_none() && matches!(screen, Screen::App(_)) {
                            if let Screen::App(a) = &mut screen {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                let action = a.pointer(PointerEvent::Wheel { x: cursor.0, y: cursor.1, dy }, &ctx);
                                apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                            }
                        } else if zoom.is_none() && matches!(screen, Screen::Editor) {
                            editor.wheel(dy);
                        } else if zoom.is_none() && matches!(screen, Screen::Home) {
                            home.wheel_scroll(dy);
                        } else if zoom.is_none() && !matches!(screen, Screen::Terminal(_) | Screen::Editor) {
                            for _ in 0..dy.unsigned_abs().min(3) {
                                expanded.push(key(if dy > 0 { Keycode::Up } else { Keycode::Down }));
                            }
                        }
                    }
                    other => {
                        if matches!(other, Input::Key { .. } | Input::Text(_)) && cursor_until.is_some() {
                            cursor_until = None; // typing hides the pointer
                            dirty = true;
                        }
                        expanded.push(other);
                    }
                }
            }
            inputs = expanded;
            if let Some(t) = cursor_until {
                if now >= t {
                    cursor_until = None;
                    dirty = true;
                }
            }
        }

        // ---- input method: romaji → kana before anything else looks at the keys ---------------
        // (the Agent's own injected keys and screens without a text field are left alone)
        {
            let text_screen = !locked
                && zoom.is_none()
                && app_menu.is_none()
                && unsaved.is_none()
                && agent_restore.is_none()
                && (agent_open
                    || save_panel.is_some()
                    || home_modal.is_some()
                    || matches!(screen, Screen::Editor | Screen::Terminal(_) | Screen::Settings(_) | Screen::App(_)));
            ime.set_learning(!matches!(screen, Screen::Terminal(_)));
            inputs = ime.process(inputs, text_screen);
            ime.tick();
        }

        // ---- handle input -----------------------------------------------------------
        let mut open: Option<usize> = None;
        for input in inputs {
            // Esc: remember when it went down (cleared on release).
            match &input {
                Input::Key { key, repeat: false, .. } if *key == Keycode::Escape => esc.press(Instant::now()),
                Input::KeyUp { key } if *key == Keycode::Escape => {
                    esc.release();
                }
                _ => {}
            }
            // Tab: a short press switches the home view; holding it locks / unlocks. Ctrl+Tab and
            // Alt+Tab are chords (menu / Agent): they start neither a tap nor a long press.
            match &input {
                Input::Key { key, mods, repeat: false } if *key == Keycode::Tab => tab.down(Instant::now(), mods.intersects(CHORD_MODS)),
                // Holding Tab is the lock gesture: its auto-repeat must not step through an app's tabs
                // meanwhile. Only the Terminal and the Text Editor get repeated Tabs.
                Input::Key { key, mods, repeat: true } if *key == Keycode::Tab && !mods.intersects(CHORD_MODS) && !matches!(screen, Screen::Terminal(_) | Screen::Editor) => continue,
                // Any other key between two Tabs means they were not a double tap.
                Input::Key { key, repeat: false, .. } if !is_modifier_key(*key) => tab.other_key(),
                Input::KeyUp { key } => {
                    if *key == Keycode::Tab {
                        let now = Instant::now();
                        let free = !locked && zoom.is_none() && app_menu.is_none() && save_panel.is_none() && unsaved.is_none() && home_modal.is_none();
                        // Tab Tab opens (or closes) the Agent. Where Tab belongs to the screen in front
                        // (Terminal, Text Editor, text fields, tab bars) a double Tab is just two Tabs:
                        // Alt+Tab or Harbour menu > Agent opens it there. An open panel always closes with it.
                        let agent_on = free && agent::history::load_state(&agent::config::data_dir()).enabled;
                        let gesture = agent_on && (agent_open || !screen_uses_tab(&screen));
                        match tab.up(now, gesture) {
                            TabUp::Double => {
                                home_toggle_due = None;
                                toggle_agent(&mut agent_open, &mut agent, &cfg, &mut toast);
                            }
                            TabUp::Single if free && matches!(screen, Screen::Home) && !agent_open => {
                                // With the Agent off there is no double tap to wait for.
                                home_toggle_due = Some(if agent_on { now + DoubleTap::WINDOW } else { now });
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                _ => {}
            }
            if locked {
                match input {
                    Input::Quit => break 'main,
                    Input::Media(k) => toast = Some(media(&sys, &cfg, &mut vol, k)),
                    Input::Key { key, .. } if key == Keycode::Up || key == Keycode::Down => {
                        unlock = (unlock + 0.25).min(1.0);
                        unlock_at = Instant::now();
                        if unlock >= 1.0 {
                            locked = false;
                            unlock = 0.0;
                        }
                        dirty = true;
                    }
                    _ => {}
                }
                continue;
            }
            if save_panel.is_some() || unsaved.is_some() {
                match input {
                    Input::Quit => break 'main,
                    Input::Media(k) => toast = Some(media(&sys, &cfg, &mut vol, k)),
                    Input::Text(t) => {
                        if let Some(p) = &mut save_panel {
                            p.text(&t);
                        }
                    }
                    Input::Key { key, .. } => {
                        let mut card_choice: Option<CardChoice> = None;
                        let mut panel_result: Option<SaveResult> = None;
                        if let Some(p) = &mut save_panel {
                            panel_result = if key == Keycode::F24 {
                                dialog_click.take().and_then(|(x, y)| p.click(x, y))
                            } else {
                                p.key(key)
                            };
                        } else if let Some(c) = &mut unsaved {
                            card_choice = if key == Keycode::F24 {
                                dialog_click.take().and_then(|(x, y)| c.click(x, y))
                            } else {
                                c.key(key)
                            };
                        }
                        match card_choice {
                            Some(CardChoice::Cancel) => unsaved = None,
                            Some(CardChoice::DontSave) => {
                                unsaved = None;
                                editor = TextEditor::from_text(&std::fs::read_to_string(&note_path).unwrap_or_default());
                                editor.mark_clean();
                                run_next = Some(unsaved_then.clone());
                            }
                            Some(CardChoice::Save) => {
                                unsaved = None;
                                save_panel = Some(SavePanel::new(note_path.parent().unwrap_or(&data_dir), &note_name(&note_path), &home_dir()));
                                panel_for = PanelFor::SaveAs;
                                quit_after_save = Some(unsaved_then.clone());
                            }
                            None => {}
                        }
                        match panel_result {
                            Some(SaveResult::Cancel) => {
                                save_panel = None;
                                quit_after_save = None;
                            }
                            Some(SaveResult::Save(path)) if panel_for == PanelFor::OpenFile => match fileops::read_text(&path) {
                                Ok(t) => {
                                    editor = TextEditor::from_text(&t);
                                    editor.mark_clean();
                                    note_path = path;
                                    let _ = std::fs::write(&note_pointer, note_path.to_string_lossy().as_bytes());
                                    save_panel = None;
                                }
                                Err(e) => {
                                    if let Some(p) = &mut save_panel {
                                        p.message = Some(e);
                                    }
                                }
                            },
                            Some(SaveResult::Save(path)) if panel_for == PanelFor::NewFile => {
                                // A new file never replaces an existing one.
                                let made = match path.file_name().and_then(|n| n.to_str()).zip(path.parent()) {
                                    Some((n, d)) => fileops::new_file(d, n),
                                    None => Err("Type a file name".to_string()),
                                };
                                match made {
                                    Ok(path) => {
                                        editor = TextEditor::from_text("");
                                        editor.mark_clean();
                                        note_path = path;
                                        let _ = std::fs::write(&note_pointer, note_path.to_string_lossy().as_bytes());
                                        save_panel = None;
                                    }
                                    Err(e) => {
                                        if let Some(p) = &mut save_panel {
                                            p.message = Some(e);
                                            p.confirm_replace = false;
                                        }
                                    }
                                }
                            }
                            Some(SaveResult::Save(path)) => {
                                let written = path
                                    .parent()
                                    .map(|d| std::fs::create_dir_all(d))
                                    .unwrap_or(Ok(()))
                                    .and_then(|_| std::fs::write(&path, editor.text_for_save()));
                                match written {
                                    Ok(()) => {
                                        note_path = path;
                                        let _ = std::fs::write(&note_pointer, note_path.to_string_lossy().as_bytes());
                                        editor.mark_clean();
                                        toast = Some(Toast { text: harbour_shell::launcher::i18n::tr("Saved").into(), fraction: None, until: Instant::now() + Duration::from_millis(1200) });
                                        save_panel = None;
                                        run_next = quit_after_save.take();
                                    }
                                    Err(e) => {
                                        if let Some(p) = &mut save_panel {
                                            p.message = Some(format!("Cannot save: {e}"));
                                        }
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                    _ => {}
                }
                dirty = true;
                continue;
            }
            if let Some(modal) = home_modal.as_mut() {
                match input {
                    Input::Quit => break 'main,
                    Input::Media(k) => toast = Some(media(&sys, &cfg, &mut vol, k)),
                    Input::Text(t) => {
                        if let HomeModal::NewFolder(p) | HomeModal::Rename(_, p) = modal {
                            p.text(&t);
                        }
                    }
                    Input::Key { key, .. } => {
                        let click = if key == Keycode::F24 { dialog_click.take() } else { None };
                        if key != Keycode::F24 || click.is_some() {
                            let done = match modal {
                                HomeModal::NewFolder(p) | HomeModal::Rename(_, p) => {
                                    let r = match click {
                                        Some((x, y)) => p.click(x, y),
                                        None => p.key(key),
                                    };
                                    r.map(|r| match r {
                                        PromptResult::Cancel => None,
                                        PromptResult::Ok(n) => Some(n),
                                    })
                                    .map(|n| (n, None, None))
                                }
                                HomeModal::DeleteFolder(_, a) | HomeModal::AutoOrganize(a) => {
                                    let r = match click {
                                        Some((x, y)) => a.click(x, y),
                                        None => a.key(key),
                                    };
                                    r.map(|r| (None, Some(r == ConfirmChoice::Ok), None))
                                }
                                HomeModal::Move(p, _) => {
                                    let r = match click {
                                        Some((x, y)) => p.click(x, y),
                                        None => p.key(key),
                                    };
                                    r.map(|r| match r {
                                        PickResult::Cancel => (None, None, None),
                                        PickResult::Choose(i) => (None, None, Some(i)),
                                    })
                                }
                            };
                            if let Some((name, confirmed, chosen)) = done {
                                match home_modal.take() {
                                    Some(HomeModal::NewFolder(_)) => {
                                        if let Some(n) = name {
                                            home.create_folder(&n);
                                        }
                                    }
                                    Some(HomeModal::Rename(i, _)) => {
                                        if let Some(n) = name {
                                            home.rename_folder(i, &n);
                                        }
                                    }
                                    Some(HomeModal::DeleteFolder(i, _)) => {
                                        if confirmed == Some(true) {
                                            home.delete_folder(i);
                                        }
                                    }
                                    Some(HomeModal::AutoOrganize(_)) => {
                                        if confirmed == Some(true) {
                                            let n = home.auto_organize();
                                            let text = if n == 0 { "Nothing to organize".to_string() } else { format!("Moved {n} apps into folders") };
                                            toast = Some(Toast { text, fraction: None, until: Instant::now() + Duration::from_millis(1800) });
                                        }
                                    }
                                    Some(HomeModal::Move(_, dests)) => {
                                        if let Some(d) = chosen.and_then(|i| dests.get(i).copied()) {
                                            home.move_selected(d);
                                        }
                                    }
                                    None => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
                dirty = true;
                continue;
            }
            if app_menu.is_some() {
                match input {
                    Input::Quit => break 'main,
                    Input::Key { key, .. } => {
                        let mut chosen: Option<String> = None;
                        let mut swap_menu: Option<bool> = None;
                        if let Some(m) = &mut app_menu {
                            match harbour_shell::platform::keys::arrow_alias(key) {
                                Keycode::Up => m.up(),
                                Keycode::Down => m.down(),
                                // The two menus sit side by side like a menu bar: Left from the app menu's first
                                // column goes to the Harbour (system) menu, Right from there comes back.
                                Keycode::Left => {
                                    let before = m.cursor;
                                    m.sideways(-1);
                                    if m.cursor == before && m.origin.is_none() && !agent_open && !is_system_menu(m) {
                                        swap_menu = Some(true);
                                    }
                                }
                                Keycode::Right => {
                                    if is_system_menu(m) {
                                        swap_menu = Some(false);
                                    } else {
                                        m.sideways(1);
                                    }
                                }
                                Keycode::Return => chosen = Some(m.selected().id.to_string()),
                                Keycode::Escape | Keycode::Tab => app_menu = None,
                                _ => {}
                            }
                        }
                        match swap_menu {
                            Some(true) => app_menu = Some(system_menu(&screen)),
                            Some(false) => app_menu = Some(open_menu(&screen, &home, None)),
                            None => {}
                        }
                        if let Some(id) = chosen {
                            app_menu = None;
                            // "Home" is the app menu's Quit, from anywhere (the Agent panel closes first).
                            let id = if id == "sys_home" {
                                agent_open = false;
                                if matches!(screen, Screen::Home) { String::new() } else { harbour_shell::launcher::widgets::QUIT_ID.to_string() }
                            } else { id };
                            if id.is_empty() {
                            } else if let Some(act) = id.strip_prefix("st_") {
                                // The status menus: open an app or a Settings section (on a desk in a window
                                // of its own), switch Wi-Fi, set the volume, switch the input method.
                                let opens = act.starts_with("app_") || act.starts_with("set_");
                                if opens {
                                    agent_open = false;
                                    desk_to_home(&mut desk, &mut fid, &mut screen, &mut prev_home);
                                }
                                if let Some(name) = act.strip_prefix("app_") {
                                    pending_launch = resolve_target(name, &apps).map(|(t, _)| t);
                                } else if let Some(section) = act.strip_prefix("set_") {
                                    if let Screen::Settings(s) = &mut screen { s.flush(); }
                                    let mut st = Box::new(Settings::new(sys.clone(), cfg.clone(), order_path.clone(), app_refs(&apps)));
                                    st.open_section(section);
                                    screen = Screen::Settings(st);
                                } else {
                                    match act {
                                        "wifi_toggle" => {
                                            let on = !status.lock().map(|s| s.wifi_radio).unwrap_or(true);
                                            let sys2 = sys.clone();
                                            std::thread::spawn(move || {
                                                let _ = sys2.wifi_set_radio(on);
                                            });
                                        }
                                        "vol_up" => toast = Some(media(&sys, &cfg, &mut vol, MediaKey::VolumeUp)),
                                        "vol_down" => toast = Some(media(&sys, &cfg, &mut vol, MediaKey::VolumeDown)),
                                        "mute" => toast = Some(media(&sys, &cfg, &mut vol, MediaKey::Mute)),
                                        "ime_toggle" | "ime_kana" => {
                                            let mods = if act == "ime_kana" { Mod::LCTRLMOD | Mod::LSHIFTMOD } else { Mod::LCTRLMOD };
                                            deferred.push(Input::Key { key: Keycode::Space, mods, repeat: false });
                                            deferred.push(Input::KeyUp { key: Keycode::Space });
                                        }
                                        _ => {}
                                    }
                                }
                                dirty = true;
                            } else if let Some(act) = id.strip_prefix("sys_") {
                                use harbour_shell::launcher::settings::SystemPage;
                                if let Screen::Settings(s) = &mut screen { s.flush(); }
                                ime.flush();
                                let page = match act {
                                    "about" => Some(SystemPage::About),
                                    "restart" => Some(SystemPage::Restart),
                                    "poweroff" => Some(SystemPage::PowerOff),
                                    _ => None,
                                };
                                if act == "help" {
                                    let (name, rows) = screen_help(&screen, agent_open);
                                    if desk.is_some() {
                                        // On a desk Help is a window of its own, beside the screen it explains.
                                        desk_to_home(&mut desk, &mut fid, &mut screen, &mut prev_home);
                                        screen = Screen::App(Box::new(harbour_shell::launcher::help::HelpApp::new(&name, rows)));
                                    } else {
                                        help = Some(harbour_shell::launcher::help::HelpView::new(&name, rows));
                                    }
                                } else if act == "lock" {
                                    locked = true;
                                } else if act == "agent" {
                                    toggle_agent(&mut agent_open, &mut agent, &cfg, &mut toast);
                                } else {
                                    agent_open = false;
                                    let mut st = match std::mem::replace(&mut screen, Screen::Home) {
                                        Screen::Settings(b) => b,
                                        _ => Box::new(Settings::new(sys.clone(), cfg.clone(), order_path.clone(), app_refs(&apps))),
                                    };
                                    if let Some(pg) = page { st.open_system_page(pg); }
                                    screen = Screen::Settings(st);
                                }
                                dirty = true;
                            } else if agent_open {
                                if id == harbour_shell::launcher::widgets::QUIT_ID {
                                    agent_open = false;
                                } else if let Some(ag) = agent.as_mut() {
                                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                    ag.menu_action(&id, &ctx);
                                }
                            } else if id == harbour_shell::launcher::widgets::QUIT_ID {
                                if let Screen::Settings(s) = &mut screen { s.flush(); }
    ime.flush();
                                if matches!(screen, Screen::Editor) && editor.is_dirty() {
                                    // Unsaved text: ask first.
                                    unsaved = Some(UnsavedCard::new(&note_name(&note_path)));
                                    unsaved_then = EditorNext::Home;
                                } else if let Screen::App(a) = &mut screen {
                                    if a.request_close() {
                                        screen = Screen::Home;
                                    }
                                } else {
                                    screen = Screen::Home;
                                }
                            } else if let Screen::App(a) = &mut screen {
                                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                let action = a.menu_action(&id, &ctx);
                                apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                            } else if matches!(screen, Screen::Home) {
                                match id.as_str() {
                                    "hf_open" => match home.handle_key(Keycode::Return) {
                                        HomeAction::Open(i) => open = Some(i),
                                        HomeAction::Zoom { from, to } => zoom = Some(Zoom { t: 0.0, from, to, pending: None }),
                                        _ => {}
                                    },
                                    "hf_out" => home.move_selected(harbour_shell::launcher::folders::Dest::Top),
                                    "hf_hide" => {
                                        if let Some(key) = home.items.get(home.selected).map(|i| i.key()) {
                                            if let Ok(mut c) = cfg.lock() {
                                                c.set_int(&desktop_config_key(&key), 0);
                                                let _ = c.save();
                                            }
                                            apps = load_apps(&app_dir, &order_path);
                                            rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                                            toast = Some(Toast { text: harbour_shell::launcher::i18n::tr_s("Hidden. Settings > Apps shows it again."), fraction: None, until: Instant::now() + Duration::from_secs(3) });
                                        }
                                    }
                                    "hf_new" => home_modal = Some(HomeModal::NewFolder(NamePrompt::new("New folder name:", harbour_shell::launcher::folders::DEFAULT_NAME))),
                                    "hf_rename" => {
                                        if let Some(i) = home.target_folder() {
                                            let name = home.folders.list[i].name.clone();
                                            home_modal = Some(HomeModal::Rename(i, NamePrompt::new("Rename folder to:", &name)));
                                        }
                                    }
                                    "hf_delete" => {
                                        if let Some(i) = home.target_folder() {
                                            let lines = vec![
                                                format!("Delete the folder \"{}\"?", home.folders.list[i].name),
                                                "Its apps go back to the home screen.".to_string(),
                                            ];
                                            home_modal = Some(HomeModal::DeleteFolder(i, ConfirmAlert::new(lines, "Delete", false)));
                                        }
                                    }
                                    "hf_move" => {
                                        let opts = home.move_options();
                                        if !opts.is_empty() {
                                            let name = home.items.get(home.selected).map(|i| i.name.clone()).unwrap_or_default();
                                            let rows = opts.iter().map(|(n, _)| n.clone()).collect();
                                            let dests = opts.iter().map(|(_, d)| *d).collect();
                                            home_modal = Some(HomeModal::Move(ListPicker::new(&format!("Move \"{name}\" to:"), rows), dests));
                                        }
                                    }
                                    "hf_auto" => {
                                        let lines = vec![
                                            "Organize apps into folders?".to_string(),
                                            "Makes Games, Music and Tools folders".to_string(),
                                            "from the apps on this device.".to_string(),
                                        ];
                                        home_modal = Some(HomeModal::AutoOrganize(ConfirmAlert::new(lines, "Organize", true)));
                                    }
                                    _ => {}
                                }
                            } else {
                                match id.as_str() {
                                    "saveas" => {
                                        save_panel = Some(SavePanel::new(note_path.parent().unwrap_or(&data_dir), &note_name(&note_path), &home_dir()));
                                        panel_for = PanelFor::SaveAs;
                                        quit_after_save = None;
                                    }
                                    "new" | "open" if matches!(screen, Screen::Editor) => {
                                        let next = if id == "new" { EditorNext::New } else { EditorNext::PickOpen };
                                        if editor.is_dirty() {
                                            unsaved = Some(UnsavedCard::new(&note_name(&note_path)));
                                            unsaved_then = next;
                                        } else {
                                            run_next = Some(next);
                                        }
                                    }
                                    "settings" => calc.toggle_modal(),
                                    "prev" => world.handle_key(Keycode::Left),
                                    "next" => world.handle_key(Keycode::Right),
                                    "restart" => snake = SnakeState::new(),
                                    "save" => save_note(&mut editor, &note_path, &mut toast),
                                    "undo" => editor.undo(),
                                    "clear" => match &mut screen {
                                        Screen::Terminal(t) => t.write(b"\x0c"),
                                        _ => calc.clear_all(),
                                    },
                                    _ => {}
                                }
                            }
                        }
                    }
                    Input::Media(k) => toast = Some(media(&sys, &cfg, &mut vol, k)),
                    Input::Menu => app_menu = None,
                    _ => {}
                }
                dirty = true;
                continue;
            }
            match input {
                Input::Quit => break 'main,
                // Pointer input was already turned into selections / keys above.
                Input::KeyUp { .. } | Input::MouseMove { .. } | Input::MouseButton { .. } | Input::MouseWheel { .. } | Input::Pad { .. } | Input::Stick { .. } => {}
                // Hold Sym (push-to-talk) talks to Muse while the Agent panel is open; elsewhere it is ignored.
                Input::Voice(down) => {
                    if agent_open {
                        if let Some(ag) = agent.as_mut() {
                            ag.voice_input(down);
                        }
                    } else if !down {
                        // A release that comes after the panel closed changes nothing.
                    }
                }
                Input::Media(k) => {
                    toast = Some(media(&sys, &cfg, &mut vol, k));
                }
                Input::Key { key, mods, .. } if key == Keycode::Q && mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) && fb.is_none() => {
                    break 'main;
                }
                Input::Menu => app_menu = Some(open_menu(&screen, &home, agent_open.then(|| agent.as_deref()).flatten())),
                Input::Key { key, mods, .. }
                    if key == Keycode::Tab && mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) =>
                {
                    app_menu = Some(open_menu(&screen, &home, agent_open.then(|| agent.as_deref()).flatten()));
                }
                // Alt+Tab opens / closes the Agent from any screen (also where Tab belongs to the app).
                Input::Key { key, mods, repeat: false } if key == Keycode::Tab && mods.intersects(Mod::LALTMOD | Mod::RALTMOD) => {
                    toggle_agent(&mut agent_open, &mut agent, &cfg, &mut toast);
                    home_toggle_due = None;
                }
                Input::Key { key, mods, .. } if key == Keycode::Tab && mods.intersects(Mod::LALTMOD | Mod::RALTMOD) => {}
                Input::Key { key, mods, repeat } if agent_open && agent.is_some() => {
                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                    if let Some(ag) = agent.as_mut() {
                        // Esc closes the chat (nothing in front of it to dismiss) and shows what was behind.
                        if ag.key(key, mods, repeat, &ctx) == AppAction::Quit {
                            agent_open = false;
                        }
                    }
                }
                Input::Text(text) if agent_open && agent.is_some() => {
                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                    if let Some(ag) = agent.as_mut() {
                        ag.text(&text, &ctx);
                    }
                }
                Input::Key { key, mods, repeat } => match &mut screen {
                    Screen::App(a) => {
                        let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                        let action = a.key(key, mods, repeat, &ctx);
                        // The app's own Esc-at-top-level "Quit" is ignored: hold Esc or use the menu.
                        let action = if key == Keycode::Escape && action == AppAction::Quit && !a.esc_closes() { AppAction::None } else { action };
                        apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                    }
                    Screen::Home => {
                        match home.handle_key(key) {
                            HomeAction::Open(i) if !repeat => open = Some(i),
                            HomeAction::Zoom { from, to } if !repeat => {
                                zoom = Some(Zoom { t: 0.0, from, to, pending: None });
                            }
                            _ => {}
                        }
                    }
                    Screen::Settings(s) => match mouse_ev.take().unwrap_or_else(|| s.handle_key(key, mods)) {
                        SettingsEvent::None | SettingsEvent::ScreenChanged => {}
                        // Settings > Agent changed the provider or switched it off: the next open reloads it
                        // (the chat itself is on disk).
                        SettingsEvent::AgentChanged => agent = None,
                        SettingsEvent::ThemeChanged => {
                            fs.display_cavs = true; // the title face is always Chicago (the Display setting was removed)
                            match Theme::load_from_yaml(&theme_for(&cfg)) {
                                Ok(t) => tm = ThemeManager::new(t),
                                Err(e) => toast = Some(Toast { text: format!("Theme: {e}"), fraction: None, until: Instant::now() + Duration::from_secs(3) }),
                            }
                        }
                        // A short Esc at the top level does not leave Settings, as in every app (hold Esc
                        // or the menu's Quit does); the close box still does.
                        SettingsEvent::Close if key == Keycode::Escape => {}
                        SettingsEvent::Close => screen = Screen::Home,
                        SettingsEvent::AppsChanged => {
                            apps = load_apps(&app_dir, &order_path);
                            rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                            if let Screen::Settings(s) = &mut screen {
                                s.set_apps(app_refs(&apps));
                            }
                        }
                        SettingsEvent::Install(req) => *INSTALL_REQ.lock().unwrap() = Some(req),
                        SettingsEvent::Launch(exec) => {
                            let entry = AppEntry {
                                id: String::new(),
                                name: "App Store".into(),
                                icon: None,
                                exec,
                                terminal: false,
                                sysplause: true,
                                mtime: None,
                                user: false,
                            };
                            run_external(&fs, &tm, &entry, &workdir, &shared, &mut canvas, &mut fb, &rx, &mut toast, &mut dirty, &mut last_input);
                        }
                        SettingsEvent::Exit => {
                            harbour_shell::rt::sleep(Duration::from_millis(1500));
                            break 'main;
                        }
                    },
                    Screen::Calculator => match calc.handle_key(key, mods) {
                        // A short Esc does not leave the app: hold Esc or use the menu's Quit.
                        CalcEvent::Close => {}
                        CalcEvent::SettingsChanged => {
                            let (rpn, rad, fmt) = calc.settings.to_ints();
                            if let Ok(mut c) = cfg.lock() {
                                c.set_int("harbour_calc_rpn", rpn);
                                c.set_int("harbour_calc_rad", rad);
                                c.set_int("harbour_calc_fmt", fmt);
                                let _ = c.save();
                            }
                        }
                        CalcEvent::None => {}
                    },
                    // Esc is ignored in these (a short press must not close the app; hold it to leave).
                    Screen::Clock => {
                        if key != Keycode::Escape {
                            world.handle_key(key);
                        }
                    }
                    Screen::Snake => {
                        if key != Keycode::Escape {
                            snake.input_key(harbour_shell::platform::keys::arrow_alias(key));
                        }
                    }
                    Screen::Terminal(t) => {
                        if t.exited() {
                            // "press any key" after the program ended.
                            screen = Screen::Home;
                        } else {
                            t.send_key(key, mods);
                        }
                    }
                    Screen::Editor => {
                        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
                        if key == Keycode::Escape {
                            // Esc alone does nothing: hold it to leave (or use the menu's Quit).
                        } else if ctrl && key == Keycode::S {
                            save_note(&mut editor, &note_path, &mut toast);
                        } else if ctrl && (key == Keycode::N || key == Keycode::O) {
                            let next = if key == Keycode::N { EditorNext::New } else { EditorNext::PickOpen };
                            if editor.is_dirty() {
                                unsaved = Some(UnsavedCard::new(&note_name(&note_path)));
                                unsaved_then = next;
                            } else {
                                run_next = Some(next);
                            }
                        } else {
                            editor.input_key(key, mods);
                        }
                    }
                },
                Input::Text(text) => match &mut screen {
                    Screen::Home => {}
                    Screen::Settings(s) => s.handle_text(&text),
                    Screen::Calculator => calc.handle_text(&text),
                    Screen::Editor => text.chars().for_each(|c| editor.input_char(c)),
                    Screen::Terminal(t) => t.write(text.as_bytes()),
                    Screen::App(a) => {
                        let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                        let action = a.text(&text, &ctx);
                        apply_app_action(action, &mut screen, &workdir, &shared, &mut toast);
                    }
                    _ => {}
                },
            }
        }

        if let Some(o) = agent_restore {
            agent_open = o;
        }

        // ---- open the chosen item (or the one the agent asked for) ---------------------------
        // Text Editor: what to do now that the note is safe (New, Open, leave).
        if let Some(next) = run_next.take() {
            let dir = note_path.parent().map(Path::to_path_buf).unwrap_or_else(|| data_dir.clone());
            match next {
                EditorNext::Home => screen = Screen::Home,
                EditorNext::New => {
                    save_panel = Some(SavePanel::new(&dir, "untitled.txt", &home_dir()).titled("New file:", "Create"));
                    panel_for = PanelFor::NewFile;
                }
                EditorNext::PickOpen => {
                    save_panel = Some(SavePanel::open(&dir, &home_dir(), &fileops::TEXT_EXT));
                    panel_for = PanelFor::OpenFile;
                }
                EditorNext::Open(path) => match fileops::read_text(&path) {
                    Ok(t) => {
                        editor = TextEditor::from_text(&t);
                        editor.mark_clean();
                        note_path = path;
                        let _ = std::fs::write(&note_pointer, note_path.to_string_lossy().as_bytes());
                        screen = Screen::Editor;
                    }
                    Err(e) => toast = Some(Toast { text: e, fraction: None, until: Instant::now() + Duration::from_secs(3) }),
                },
            }
            dirty = true;
        }
        // A file another app wants opened ("Open With" in Files, Edit in Paint in Images...).
        let open_req = OPEN_REQ.lock().unwrap().take();
        if let Some((app, path)) = open_req {
            let name = app.or_else(|| fileops::open_kind(&path).map(|k| k.app_name().to_string()));
            let say = |t: String| Toast { text: t, fraction: None, until: Instant::now() + Duration::from_secs(3) };
            match name.as_deref() {
                None => toast = Some(say(fileops::cannot_open_reason(&path))),
                Some("Text Editor") => {
                    if matches!(screen, Screen::Editor) && editor.is_dirty() {
                        unsaved = Some(UnsavedCard::new(&note_name(&note_path)));
                        unsaved_then = EditorNext::Open(path);
                    } else {
                        run_next = Some(EditorNext::Open(path));
                    }
                }
                Some(n) => match harbour_shell::launcher::apps::REGISTRY.iter().find(|d| d.name == n || d.id == n) {
                    Some(d) => {
                        let mut a = (d.make)();
                        match a.open_path(&path) {
                            Ok(()) => screen = Screen::App(a),
                            Err(e) => toast = Some(say(format!("{n}: {e}"))),
                        }
                    }
                    None => toast = Some(say(format!("{n} is not installed"))),
                },
            }
            dirty = true;
        }

        let launch: Option<Target> = match open {
            Some(i) => home.items.get(i).cloned().map(|item| {
                home.record_launch(&item.key());
                item.target
            }),
            None => pending_launch.take(),
        };
        if let Some(target) = launch {
            {
                match target {
                    Target::Builtin(Builtin::Settings) => {
                        screen = Screen::Settings(Box::new(Settings::new(
                            sys.clone(),
                            cfg.clone(),
                            order_path.clone(),
                            app_refs(&apps),
                        )));
                    }
                    Target::Builtin(Builtin::Calculator) => screen = Screen::Calculator,
                    Target::Builtin(Builtin::Clock) => screen = Screen::Clock,
                    Target::Builtin(Builtin::Snake) => {
                        snake = SnakeState::new();
                        screen = Screen::Snake;
                    }
                    Target::Builtin(Builtin::App(n)) => {
                        if let Some(d) = harbour_shell::launcher::apps::REGISTRY.get(n) {
                            screen = Screen::App((d.make)());
                        }
                    }
                    Target::Builtin(Builtin::Monitor) => screen = Screen::App(harbour_shell::launcher::apps::monitor::make()),
                    Target::Builtin(Builtin::TextEditor) => screen = Screen::Editor,
                    Target::Builtin(Builtin::Terminal) => {
                        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
                        let argv = vec![if Path::new(&sh).is_file() { sh } else { "/bin/sh".to_string() }];
                        match Terminal::spawn(&argv, Some(&home_dir()), shared.clone(), false) {
                            Ok(t) => screen = Screen::Terminal(Box::new(t)),
                            Err(e) => toast = Some(Toast { text: format!("Terminal: {e}"), fraction: None, until: Instant::now() + Duration::from_secs(3) }),
                        }
                    }
                    Target::External(id) => {
                        if let Some(app) = apps.iter().find(|a| a.id == id).cloned() {
                            if app.terminal {
                                let spawned = desktop_entry::exec_argv(&app.exec)
                                    .ok_or_else(|| "Exec not allowed".to_string())
                                    .and_then(|argv| Terminal::spawn(&argv, Some(&workdir), shared.clone(), app.sysplause));
                                match spawned {
                                    Ok(t) => screen = Screen::Terminal(Box::new(t)),
                                    Err(e) => toast = Some(Toast { text: format!("{}: {e}", app.name), fraction: None, until: Instant::now() + Duration::from_secs(3) }),
                                }
                            } else {
                                run_external(&fs, &tm, &app, &workdir, &shared, &mut canvas, &mut fb, &rx, &mut toast, &mut dirty, &mut last_input);
                                apps = load_apps(&app_dir, &order_path);
                                rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                            }
                        }
                    }
                }
            }
        }

        // ---- periodic updates ------------------------------------------------------------------
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + Duration::from_secs(3);
            // Pick up apps installed or removed while the launcher is open.
            if matches!(screen, Screen::Home) {
                let fresh = load_apps(&app_dir, &order_path);
                if fresh.len() != apps.len() || fresh.iter().zip(&apps).any(|(a, b)| a.id != b.id || a.mtime != b.mtime) {
                    apps = fresh;
                    rebuild_home(&mut home, &apps, &mut assets, &texture_creator, &cfg);
                    dirty = true;
                }
            }
        }
        if let Some(t) = home_toggle_due {
            if Instant::now() >= t {
                home_toggle_due = None;
                if matches!(screen, Screen::Home) && !agent_open && !locked && zoom.is_none() {
                    home.toggle_view();
                    if let Ok(mut c) = cfg.lock() {
                        c.set_int("harbour_view", (home.view == View::List) as i64);
                        let _ = c.save();
                    }
                    dirty = true;
                }
            }
        }
        if !agent_open {
            if let Some(ag) = agent.as_mut() {
                ag.on_hidden(); // a recording in progress is dropped when the panel closes
            }
        }
        if agent_open && !locked {
            if let Some(ag) = agent.as_mut() {
                let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                if ag.tick(&ctx) {
                    dirty = true;
                }
                // One tool for the screen per frame, so a launch is done before the keys that follow it.
                if let Some(req) = ag.take_host_req() {
                    let reply: Result<String, String> = match req {
                        HostReq::ListApps => Ok(launchable_names(&apps).join("\n")),
                        HostReq::Launch(name) => match resolve_target(&name, &apps) {
                            Some((t, label)) => {
                                pending_launch = Some(t);
                                Ok(format!("Launched {label}. It is open behind this chat (Esc shows it)."))
                            }
                            None => Err(format!("no app called \"{name}\"; call list_apps")),
                        },
                        HostReq::CurrentScreen => Ok(describe_screen(&screen, &home, &world, &editor, &snake)),
                        HostReq::PressKeys(keys) => {
                            let n = keys.iter().filter(|i| matches!(i, Input::Key { .. })).count();
                            inject.extend(keys);
                            Ok(format!("Pressed {n} keys in the app behind this chat."))
                        }
                        HostReq::ShowMessage(text) => {
                            toast = Some(Toast { text, fraction: None, until: Instant::now() + Duration::from_secs(3) });
                            Ok("Message shown.".into())
                        }
                    };
                    ag.host_reply(reply, sys.as_ref());
                    dirty = true;
                }
                // The empty state's "Open Settings": close the panel and show Settings > Agent.
                if ag.take_open_settings() {
                    agent_open = false;
                    let mut st = Settings::new(sys.clone(), cfg.clone(), order_path.clone(), app_refs(&apps));
                    st.open_agent();
                    screen = Screen::Settings(Box::new(st));
                    dirty = true;
                }
                if ag.take_restyle() {
                    fs.display_cavs = true; // the title face is always Chicago (the Display setting was removed)
                    match Theme::load_from_yaml(&theme_for(&cfg)) {
                        Ok(t) => tm = ThemeManager::new(t),
                        Err(e) => toast = Some(Toast { text: format!("Theme: {e}"), fraction: None, until: Instant::now() + Duration::from_secs(3) }),
                    }
                    dirty = true;
                }
            }
        }
        if locked && unlock > 0.0 {
            if unlock_at.elapsed() >= Duration::from_millis(900) {
                unlock = (unlock - 0.05).max(0.0);
                unlock_at = Instant::now() - Duration::from_millis(850);
            }
            dirty = true;
        }
        if !locked {
            unlock = 0.0;
        }
        if esc.active() || tab.hold.active() {
            dirty = true;
        }
        {
            if !locked && zoom.is_none() && (agent_open || !matches!(screen, Screen::Home)) && unsaved.is_none() && save_panel.is_none() && esc.poll(Instant::now(), HoldKey::ESC_LONG) {
                // Esc long leaves the app from anywhere: menus, dialogs and the Agent panel close too.
                agent_open = false;
                app_menu = None;
                if let Screen::Settings(s) = &mut screen { s.flush(); }
                if matches!(screen, Screen::Editor) && editor.is_dirty() {
                    // Unsaved text: ask first.
                    unsaved = Some(UnsavedCard::new(&note_name(&note_path)));
                    unsaved_then = EditorNext::Home;
                } else if let Screen::App(a) = &mut screen {
                    if a.request_close() {
                        screen = Screen::Home;
                    }
                } else {
                    screen = Screen::Home; // the old screen is dropped (a terminal program gets SIGHUP)
                }
                dirty = true;
            }
        }
        if zoom.is_none() && tab_lock_allowed(locked, &screen) && tab.hold.poll(Instant::now(), HoldKey::TAB_LONG) {
            locked = !locked;
            dirty = true;
        }
        {
            let dt = agent_prog_t.elapsed().as_secs_f32().min(0.05);
            agent_prog_t = Instant::now();
            let target = if agent_open { 1.0 } else { 0.0 };
            if agent_prog != target {
                let step = dt / 0.18;
                agent_prog = if agent_prog < target { (agent_prog + step).min(1.0) } else { (agent_prog - step).max(0.0) };
                dirty = true;
            }
        }
        if let Some(d) = desk.as_mut() {
            // The windows behind keep running (a clock, a download, a graph).
            for w in d.windows_mut() {
                let size = (w.rect.width() as i32, w.rect.height() as i32);
                match &mut w.slot {
                    Some(Screen::App(a)) => {
                        let ctx = harbour_shell::launcher::app::Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                        if harbour_shell::grid::with_surface(size, || a.tick(&ctx)) {
                            dirty = true;
                        }
                    }
                    Some(Screen::Settings(s)) => s.tick(),
                    _ => {}
                }
            }
        }
        match &mut screen {
            Screen::App(a) => {
                let ctx = harbour_shell::launcher::app::Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                if a.tick(&ctx) {
                    dirty = true;
                }
            }
            Screen::Settings(s) => {
                s.tick();
                // A page slide must be redrawn every frame, not just on input.
                if s.is_busy() || s.animating() {
                    dirty = true;
                }
            }
            Screen::Snake => {
                if !locked {
                    snake.update();
                    dirty = true;
                }
            }
            Screen::Calculator => {
                if calc.is_flashing() {
                    dirty = true;
                }
            }
            Screen::Clock => {
                if world.poll_redraw(chrono::Local::now()) {
                    dirty = true;
                }
            }
            Screen::Terminal(t) => {
                if t.poll() {
                    dirty = true;
                }
                if t.exited() && !t.pause_on_exit {
                    screen = Screen::Home;
                    dirty = true;
                }
            }
            _ => {}
        }
        if toast.as_ref().map(|t| Instant::now() >= t.until).unwrap_or(false) {
            toast = None;
            dirty = true;
        }
        if toast.is_some() {
            dirty = true;
        }
        if locked {
            let sec = chrono::Timelike::second(&chrono::Local::now());
            if sec != last_sec {
                last_sec = sec;
                dirty = true;
            }
        }
        let now_text = chrono::Local::now().format("%H:%M").to_string();
        if now_text != last_time {
            last_time = now_text.clone();
            dirty = true;
        }
        if dirty {
            // fall through to draw
        }

        // Auto lock (`harbour_lock_auto`, seconds of idle time; 0 = never), as a long Tab would.
        // Not while a script drives the screen, nor while the zoom animation runs.
        if !locked && script.is_none() && zoom.is_none() {
            let auto = cfg.lock().map(|c| lock::auto_lock_secs(&c)).unwrap_or(0);
            if auto > 0 && last_input.elapsed() >= Duration::from_secs(auto) {
                println!("[lock] locked after {auto} s idle");
                locked = true;
                dirty = true;
            }
        }
        // Screen timeout (official `dark_time`, seconds; 0 = never).
        // A desk is a computer's screen, and the web page has no backlight: Harbour does not turn it off.
        let mut dark = if desk.is_some() || harbour_shell::rt::WEB { 0 } else { cfg.lock().map(|c| c.get_int("dark_time", 30)).unwrap_or(0) };
        if locked && !harbour_shell::rt::WEB {
            // The lock screen goes dark sooner (`harbour_lock_dark`, seconds, default 8; 0 = follow
            // dark_time), and even when the normal screen timeout is "never".
            let lock_dark = cfg.lock().map(|c| c.get_int("harbour_lock_dark", 8)).unwrap_or(8);
            if lock_dark > 0 {
                dark = if dark > 0 { dark.min(lock_dark) } else { lock_dark };
            }
        }
        if dark > 0 && desk.is_none() && screen_off_prev.is_none() && fade.is_none() && last_input.elapsed() > Duration::from_secs(dark as u64) && script.is_none() {
            if let Some((v, _)) = sys.brightness() {
                if v > 0 {
                    println!("[screen] off after {} s idle (locked: {locked})", dark);
                    fade = Some((Instant::now(), v));
                }
            }
        }
        if let Some((t0, v)) = fade {
            let p = t0.elapsed().as_secs_f32() * 1000.0 / FADE_MS;
            if p >= 1.0 {
                fade = None;
                screen_off_prev = Some(v);
                // Backlight 0 alone leaves the panel faintly lit, so first make every pixel black
                // (a full rewrite of the LCD, no static image left on the glass), then switch the
                // backlight off.
                canvas.set_draw_color(Color::RGB(0, 0, 0));
                canvas.clear();
                canvas.present();
                if let Some(f) = fb.as_mut() {
                    f.invalidate();
                    let _ = push_frame(&canvas, f);
                }
                harbour_shell::rt::sleep(Duration::from_millis(60));
                let _ = sys.set_brightness(0);
            } else {
                // The fade is only the backlight ramping down (cheap); redrawing the LCD for
                // every step is far too slow on the SPI panel.
                let _ = sys.set_brightness(((v as f32) * (1.0 - p)).max(1.0) as u32);
            }
        }

        // ---- Zoom rectangles between the home screen and a window ----------------
        if let Some(z) = zoom.as_mut() {
            z.t += 0.14;
            dirty = true;
            if z.t >= 1.0 {
                force_full = true;
                if let Some(z) = zoom.take() {
                    if let Some(s) = z.pending {
                        screen = s;
                        prev_home = false;
                    }
                }
            }
        }
        // A menu a window's screen opened at the pointer this frame: from window to desk coordinates.
        if let (Some((ox, oy)), false, Some(m)) = (menu_shift, menu_before, app_menu.as_mut()) {
            if let Some((x, y)) = m.origin {
                m.origin = Some((x + ox, y + oy));
            }
        }
        if let Some(d) = desk.as_mut() {
            // On a desk an app opened from Home gets a window of its own (zooming out of its icon),
            // and an app that goes back to Home closes its window (zooming back).
            let now_home = matches!(screen, Screen::Home);
            if prev_home && !now_home {
                let flexible = screen_flexible(&screen);
                let size = d.initial_size(flexible);
                let id = d.open(size, flexible, None);
                fid = Some(id);
                if let Some(w) = d.get(id) {
                    let from = harbour_shell::grid::with_surface(d.size, || home.selected_icon_rect());
                    desk_zoom = Some((0.0, from, harbour_shell::launcher::desk::outer(w.rect), Some(id)));
                }
                force_full = true;
                dirty = true;
            } else if !prev_home && now_home {
                if let Some(f) = fid.take() {
                    if let Some(w) = d.get(f) {
                        let to = harbour_shell::grid::with_surface(d.size, || home.selected_icon_rect());
                        desk_zoom = Some((0.0, harbour_shell::launcher::desk::outer(w.rect), to, None));
                    }
                    d.close(f);
                }
                // The window under it (if any) takes the focus.
                if let Some(next) = d.windows().last().map(|w| w.id) {
                    d.raise(next);
                    if let Some(inc) = d.get_mut(next).and_then(|w| w.slot.take()) {
                        screen = inc;
                        fid = Some(next);
                    }
                } else {
                    d.focus_desk();
                }
                force_full = true;
                dirty = true;
            }
            prev_home = matches!(screen, Screen::Home);
            match d.focused_rect() {
                Some(r) => harbour_shell::grid::set_surface(r.width() as i32, r.height() as i32),
                None => harbour_shell::grid::set_surface(d.size.0, d.size.1),
            };
            if let Some(z) = desk_zoom.as_mut() {
                z.0 += 0.12;
                dirty = true;
                if z.0 >= 1.0 {
                    desk_zoom = None;
                }
            }
        } else if zoom.is_none() {
            let now_home = matches!(screen, Screen::Home);
            if prev_home && !now_home {
                let target = std::mem::replace(&mut screen, Screen::Home);
                zoom = Some(Zoom { t: 0.0, from: home.selected_icon_rect(), to: chrome::app_rect(), pending: Some(target) });
                dirty = true;
            } else if !prev_home && now_home {
                zoom = Some(Zoom { t: 0.0, from: chrome::app_rect(), to: home.selected_icon_rect(), pending: None });
                dirty = true;
            }
            prev_home = matches!(screen, Screen::Home);
        }

        // ---- draw ---------------------------------------------------------------------------------
        if screen_off_prev.is_some() {
            dirty = false; // the panel is black: nothing to draw
        }
        if os_cursor && (tm.get_color("foreground"), tm.get_color("widget_face_active")) != cursor_colors {
            cursor_colors = (tm.get_color("foreground"), tm.get_color("widget_face_active"));
            os_cursor_keep = make_os_cursor(&tm, scale).or(os_cursor_keep);
        }
        if dirty {
            dirty = false;
            let mut desk_status: Option<(StatusView, String)> = None;
            // On a desk the lock screen is drawn this many times bigger (1 otherwise).
            let mut lock_zoom = 1;
            // On a desk: the desk with Home's icons, the windows behind, then the focused window through
            // a viewport (the code below draws it as if it were the whole screen).
            let showing = desk_zoom.and_then(|z| z.3);
            let base_vp: Option<sdl2::rect::Rect> = match desk.as_mut() {
                Some(d) => {
                    canvas.set_viewport(None);
                    let bar_style = cfg.lock().map(|c| c.get_int("harbour_statusbar", 0)).unwrap_or(0).clamp(0, 2) as u8;
                    harbour_shell::grid::with_surface(d.size, || {
                        chrome::draw_desktop(&mut canvas, &tm);
                        if !locked {
                            home.draw(&mut canvas, &mut assets, &fs, &tm);
                        }
                    });
                    if locked {
                        // The lock screen is drawn for 320 x 170: on the desk it is shown as big as
                        // whole-number scaling allows, in the middle.
                        let (w, h) = (SCREEN_W as i32, SCREEN_H as i32);
                        let k = (d.size.0 / w).min(d.size.1 / h).max(1);
                        lock_zoom = k;
                        let _ = canvas.set_scale((scale as i32 * k) as f32, (scale as i32 * k) as f32);
                        let r = sdl2::rect::Rect::new((d.size.0 / k - w) / 2, (d.size.1 / k - h) / 2, w as u32, h as u32);
                        canvas.set_viewport(r);
                        harbour_shell::grid::set_surface(w, h);
                        Some(r)
                    } else {
                        for w in d.windows_mut() {
                            if Some(w.id) == fid || Some(w.id) == showing {
                                continue;
                            }
                            draw_window_frame(&mut canvas, &tm, w.rect, false);
                            canvas.set_viewport(w.rect);
                            let size = (w.rect.width() as i32, w.rect.height() as i32);
                            if let Some(sc) = w.slot.as_mut() {
                                harbour_shell::grid::with_surface(size, || {
                                    let ctx = Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                                    draw_back_window(&mut canvas, &mut assets, &ctx, sc, &mut home, &mut calc, &mut world, &snake, &mut editor, bar_style);
                                });
                            }
                            canvas.set_viewport(None);
                            if w.flexible {
                                draw_resize_mark(&mut canvas, &tm, w.rect);
                            }
                        }
                        match d.focused_rect() {
                            Some(r) => {
                                if fid != showing {
                                    draw_window_frame(&mut canvas, &tm, r, true);
                                }
                                canvas.set_viewport(r);
                                harbour_shell::grid::set_surface(r.width() as i32, r.height() as i32);
                                Some(r)
                            }
                            None => {
                                harbour_shell::grid::set_surface(d.size.0, d.size.1);
                                Some(sdl2::rect::Rect::new(0, 0, d.size.0 as u32, d.size.1 as u32))
                            }
                        }
                    }
                }
                None => None,
            };
            // Full: the picture's background, then the fixed-size screen in the middle.
            let full_vp = if full { full_viewport(&screen, locked, desk_size) } else { None };
            if full {
                canvas.set_viewport(None);
                harbour_shell::grid::set_surface(desk_size.0, desk_size.1);
                if let Some(r) = full_vp {
                    chrome::draw_desktop(&mut canvas, &tm);
                    canvas.set_viewport(r);
                    harbour_shell::grid::set_surface(r.width() as i32, r.height() as i32);
                }
            }
            if desk.is_none() && !full {
                harbour_shell::grid::set_surface(desk_size.0, desk_size.1);
            }
            let osk_cut = osk_room(osk.is_some(), desk.is_some(), locked, &screen, desk_size);
            if osk_cut > 0 {
                harbour_shell::grid::set_surface(desk_size.0, desk_size.1 - osk_cut);
            }
            // Home on a desk is already drawn under the windows; a window still zooming open is not
            // drawn yet.
            let desk_home = desk.is_some() && fid.is_none() && !locked;
            let hidden = desk.is_some() && showing.is_some() && fid == showing;
            if !desk_home && !hidden {
                chrome::draw_desktop(&mut canvas, &tm);
            }
            if locked {
                let st = status.lock().map(|s| s.clone()).unwrap_or_default();
                let mut st = st;
                st.time = now_text.clone();
                st.bar_style = cfg.lock().map(|c| c.get_int("harbour_statusbar", 0)).unwrap_or(0).clamp(0, 2) as u8;
                chrome::draw_menu_bar(&mut canvas, &mut assets, &fs, &tm, "", &st);
                let opts = cfg.lock().map(|c| lock::Opts::from_config(&c, st.battery, st.charging)).unwrap_or(lock::Opts { style: lock::Style::Wall, date: true, battery: None });
                lock::draw(&mut canvas, &mut assets, &fs, &tm, &mut lock_world, chrono::Local::now(), unlock, &opts);
            } else {
                let mut st = status.lock().map(|s| s.clone()).unwrap_or_default();
                st.time = now_text;
                st.bar_style = cfg.lock().map(|c| c.get_int("harbour_statusbar", 0)).unwrap_or(0).clamp(0, 2) as u8;
                let ime_text_screen = app_menu.is_none()
                    && unsaved.is_none()
                    && (agent_open
                        || save_panel.is_some()
                        || home_modal.is_some()
                        || matches!(screen, Screen::Editor | Screen::Terminal(_) | Screen::Settings(_) | Screen::App(_)));
                st.ime = ime.indicator(ime_text_screen).to_string();
                // The current app's name sits to the right of the anchor (the folder name on Home).
                let context: String = if agent_open { harbour_shell::launcher::i18n::app_name("Agent") } else { match &screen {
                    Screen::Home => if home.context().is_empty() { "Harbour".to_string() } else { home.context().to_string() },
                    Screen::Settings(s) => harbour_shell::launcher::i18n::tr_s(&s.title()),
                    Screen::Calculator => harbour_shell::launcher::i18n::app_name("calculator"),
                    Screen::Clock => harbour_shell::launcher::i18n::tr("World Clock").to_string(),
                    Screen::Snake => format!("Snake - {}", snake.footer_text()),
                    Screen::Editor => fit_title(&fs, &editor_title(&note_path, editor.is_dirty())),
                    Screen::Terminal(_) => harbour_shell::launcher::i18n::app_name("terminal"),
                    Screen::App(a) => fit_title(&fs, &a.title()),
                } };
                if desk.is_some() {
                    // The desk's strip carries the menus and the status; a window gets a title bar.
                    desk_status = Some((st.clone(), context.clone()));
                    if !desk_home && !hidden {
                        let hover = desk.as_ref().zip(fid).is_some_and(|(d, f)| harbour_shell::launcher::desk::on_close_box(d, f, dcur.0, dcur.1));
                        draw_title_bar(&mut canvas, &fs, &tm, &context, true, hover);
                    }
                } else {
                    chrome::draw_menu_bar(&mut canvas, &mut assets, &fs, &tm, &context, &st);
                }
                if !desk_home && !hidden { match &mut screen {
                    Screen::Home => home.draw(&mut canvas, &mut assets, &fs, &tm),
                    Screen::Settings(s) => s.draw(&mut canvas, &fs, &tm),
                    Screen::Calculator => calc.draw(&mut canvas, &fs, &tm, 0, chrome::BAR_H, chrome::screen_w(), chrome::screen_h() - chrome::BAR_H as u32),
                    Screen::Clock => world.draw(&mut canvas, &mut assets, &fs, &tm, chrono::Local::now()),
                    Screen::Snake => {
                        let title = format!("Snake - {}", snake.footer_text());
                        draw_app(&mut canvas, &fs, &tm, &title, "", |c, x, y, w, h| snake.draw(c, &fs, &tm, x, y, w, h))
                    }
                    Screen::App(a) => {
                        let ctx = harbour_shell::launcher::app::Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                        a.draw(&mut canvas, &mut assets, &ctx);
                    }
                    Screen::Terminal(t) => t.draw(&mut canvas, &fs, &tm),
                    Screen::Editor => {
                        let title = if editor.is_dirty() { "Text Editor *" } else { "Text Editor" };
                        let _ = title;
                        // Framed like the Terminal: the text area is the frame's content rectangle.
                        harbour_shell::ui::fill_rect(&mut canvas, sdl2::rect::Rect::new(0, chrome::BAR_H, chrome::screen_w(), chrome::screen_h() - chrome::BAR_H as u32), tm.get_color("home_bg"));
                        harbour_shell::grid::draw_app_frame(&mut canvas, harbour_shell::grid::app_frame().y(), tm.get_color("foreground"), None);
                        let r = harbour_shell::grid::app_content();
                        editor.set_view(r.width(), r.height());
                        editor.draw(&mut canvas, &fs, &tm, r.x(), r.y(), r.width(), r.height())
                    }
                } }
                if agent_prog > 0.0 && agent.is_some() {
                    let e = 1.0 - (1.0 - agent_prog).powi(2);
                    let off = ((1.0 - e) * harbour_shell::launcher::agent::app::PANEL_W as f32).round() as i32;
                    let ctx = harbour_shell::launcher::app::Ctx { fs: &fs, tm: &tm, sys: sys.as_ref(), now: chrono::Local::now() };
                    // The panel is drawn for 320 x 170 at the right of the screen (or window).
                    let (bx, by) = base_vp.map(|r| (r.x() + r.width() as i32 - SCREEN_W as i32, r.y())).unwrap_or((0, 0));
                    if off > 0 || base_vp.is_some() {
                        canvas.set_viewport(sdl2::rect::Rect::new(bx + off, by, (SCREEN_W as i32 - off) as u32, SCREEN_H));
                    }
                    if let Some(ag) = agent.as_mut() {
                        harbour_shell::grid::with_surface((SCREEN_W as i32, SCREEN_H as i32), || ag.draw(&mut canvas, &mut assets, &ctx));
                    }
                    if off > 0 || base_vp.is_some() {
                        canvas.set_viewport(base_vp);
                    }
                }
                if desk.is_none() {
                    if let Some(m) = &app_menu {
                        m.draw(&mut canvas, &fs, &tm);
                    }
                }
                if let Some(card) = &unsaved {
                    card.draw(&mut canvas, &fs, &tm);
                }
                match &home_modal {
                    Some(HomeModal::NewFolder(p)) | Some(HomeModal::Rename(_, p)) => p.draw(&mut canvas, &fs, &tm),
                    Some(HomeModal::DeleteFolder(_, a)) | Some(HomeModal::AutoOrganize(a)) => a.draw(&mut canvas, &fs, &tm),
                    Some(HomeModal::Move(p, _)) => p.draw(&mut canvas, &fs, &tm),
                    None => {}
                }
                if let Some(p) = &save_panel {
                    p.draw(&mut canvas, &fs, &tm);
                }
                if ime.is_composing() {
                    // Over the Agent panel the line stays inside it; elsewhere it spans the screen.
                    let area = if agent_open && agent_prog > 0.0 {
                        let x = chrome::screen_w() as i32 - harbour_shell::launcher::agent::app::PANEL_W;
                        sdl2::rect::Rect::new(x, 0, harbour_shell::launcher::agent::app::PANEL_W as u32, chrome::screen_h())
                    } else {
                        sdl2::rect::Rect::new(0, 0, chrome::screen_w(), chrome::screen_h())
                    };
                    ime.draw(&mut canvas, &fs, &tm, area);
                }
                hold_gauge = None;
                let now = Instant::now();
                if let Some(t) = esc.gauge(now) {
                    if !locked && (agent_open || !matches!(screen, Screen::Home)) {
                        hold_gauge = Some(("Hold Esc: Quit", t / HoldKey::ESC_LONG.as_secs_f32()));
                    }
                }
                if let Some(t) = tab.hold.gauge(now) {
                    if tab_lock_allowed(locked, &screen) {
                        hold_gauge = Some(("Hold Tab: Lock", t / HoldKey::TAB_LONG.as_secs_f32()));
                    }
                }
                if let Some((label, f)) = hold_gauge {
                    chrome::draw_hold_gauge(&mut canvas, &fs, &tm, label, f);
                }
                if let Some(z) = &zoom {
                    chrome::draw_zoom(&mut canvas, &tm, z.from, z.to, z.t);
                }
                if let Some(t) = &toast {
                    chrome::draw_toast(&mut canvas, &fs, &tm, &t.text, t.fraction);
                }
            }
            if let Some(h) = &help {
                h.draw(&mut canvas, &fs, &tm);
            }
            if let Some(inst) = &installer {
                inst.draw(&mut canvas, &fs, &tm);
            }
            // The on-screen keyboard, across the bottom of the whole picture.
            if let Some(k) = osk.as_ref().filter(|_| !locked) {
                canvas.set_viewport(None);
                harbour_shell::grid::with_surface(desk_size, || {
                    // The band under a screen that ends above the keyboard.
                    if osk_cut > 0 {
                        harbour_shell::ui::fill_rect(&mut canvas, sdl2::rect::Rect::new(0, desk_size.1 - osk_cut, desk_size.0 as u32, osk_cut as u32), tm.get_color("home_bg"));
                    }
                    k.draw(&mut canvas, &fs, &tm);
                    // What the input method is composing shows just above the keyboard (a screen that
                    // ends above the keyboard shows it itself).
                    if ime.is_composing() && osk_cut == 0 {
                        let top = harbour_shell::launcher::osk::Osk::rect().y();
                        ime.draw(&mut canvas, &fs, &tm, sdl2::rect::Rect::new(0, 0, desk_size.0 as u32, top as u32));
                    }
                });
            }
            if let Some(d) = desk.as_ref() {
                // The desk's strip: the anchor, the focused screen's name (its menus open from here)
                // and the status. Then the menus, the zoom and the pointer, all on the desk.
                canvas.set_viewport(None);
                if lock_zoom > 1 {
                    let _ = canvas.set_scale(scale as f32, scale as f32);
                }
                let (mut st, context) = desk_status.take().unwrap_or_default();
                st.time = chrono::Local::now().format("%H:%M").to_string();
                let win_size = (harbour_shell::grid::sw(), harbour_shell::grid::sh());
                if let Some(w) = fid.and_then(|f| d.get(f)).filter(|w| w.flexible && !locked && Some(w.id) != showing) {
                    draw_resize_mark(&mut canvas, &tm, w.rect);
                }
                harbour_shell::grid::with_surface(d.size, || {
                    if !locked {
                        chrome::draw_menu_bar(&mut canvas, &mut assets, &fs, &tm, &context, &st);
                    }
                    if let Some(m) = &app_menu {
                        m.draw(&mut canvas, &fs, &tm);
                    }
                    if let Some((t, from, to, _)) = desk_zoom {
                        chrome::draw_zoom(&mut canvas, &tm, from, to, t.min(1.0));
                    }
                    chrome::draw_corners(&mut canvas, &tm);
                });
                if !os_cursor && cursor_until.is_some() && !matches!(&screen, Screen::App(a) if a.hides_pointer() && !locked) {
                    chrome::draw_cursor(&mut canvas, &tm, dcur.0, dcur.1);
                }
                harbour_shell::grid::set_surface(win_size.0, win_size.1);
            } else if full {
                // The pointer is drawn on the whole picture.
                canvas.set_viewport(None);
                harbour_shell::grid::set_surface(desk_size.0, desk_size.1);
                chrome::draw_corners(&mut canvas, &tm);
                if !os_cursor && cursor_until.is_some() && !locked && !matches!(&screen, Screen::App(a) if a.hides_pointer()) {
                    chrome::draw_cursor(&mut canvas, &tm, dcur.0, dcur.1);
                }
            } else {
                chrome::draw_corners(&mut canvas, &tm);
                if !os_cursor && cursor_until.is_some() && !locked && !matches!(&screen, Screen::App(a) if a.hides_pointer()) {
                    chrome::draw_cursor(&mut canvas, &tm, cursor.0, cursor.1);
                }
            }
            canvas.present();
            if let Some(r) = rec.as_mut() {
                r.capture(&canvas);
            }
            if let Some(f) = fb.as_mut() {
                if force_full {
                    f.invalidate();
                    force_full = false;
                }
                if let Err(e) = push_frame(&canvas, f) {
                    eprintln!("[fb] {e}");
                }
                if f.wants_refresh() {
                    refresh_due = Some(Instant::now() + Duration::from_millis(120));
                }
            }
            if let Some(s) = script.as_mut() {
                if let Some(name) = s.pending_shot.take() {
                    let dir = std::env::var("HARBOUR_SHOT_DIR").unwrap_or_else(|_| ".".into());
                    let path = format!("{dir}/{name}.bmp");
                    match save_shot(&canvas, &path) {
                        Ok(()) => eprintln!("shot {path}"),
                        Err(e) => eprintln!("shot failed: {e}"),
                    }
                }
            }
        }

        if let (Some(due), Some(f)) = (refresh_due, fb.as_mut()) {
            if !dirty && Instant::now() >= due {
                f.invalidate();
                if let Err(e) = push_frame(&canvas, f) {
                    eprintln!("[fb] {e}");
                }
                refresh_due = None;
            }
        }

        let settings_sliding = matches!(&screen, Screen::Settings(st) if st.animating());
        // While the mouse is moving the loop runs at ~80 Hz, so the pointer glides instead of stepping at 25 Hz.
        let mouse_active = last_mouse_move.elapsed() < Duration::from_millis(300);
        let frame = if mouse_active { 12 } else if zoom.is_some() || settings_sliding { 16 } else if matches!(screen, Screen::Snake) { 33 } else if script.is_some() { 16 } else { 40 };
        if let Some(r) = rec.as_mut() {
            r.tick();
        }
        let frame = if rec.is_some() { frame.min(16) } else { frame };
        harbour_shell::rt::sleep(Duration::from_millis(frame));
    }

    if let Some(r) = rec.as_mut() {
        r.finish();
    }
    if let Screen::Settings(s) = &mut screen { s.flush(); }
    if let Some(prev) = screen_off_prev {
        let _ = sys.set_brightness(prev);
    }
    shared.quit.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

// ---- helpers ------------------------------------------------------------------------------------

fn load_apps(app_dir: &Path, order_path: &Path) -> Vec<AppEntry> {
    let mut apps = registry::scan_with_user(app_dir, registry::user_app_dir().as_deref());
    registry::apply_order(&mut apps, &registry::load_order(order_path));
    apps
}

/// Settings > Apps: the installed apps (their order can be changed), then Harbour's own by package
/// id (Show / Hide only; Settings itself is always shown).
fn app_refs(apps: &[AppEntry]) -> Vec<AppRef> {
    let dir = PathBuf::from(std::env::var("HARBOUR_APP_DIR").unwrap_or_else(|_| registry::DEFAULT_APP_DIR.into()));
    let user = registry::user_app_dir();
    let mut v: Vec<AppRef> = apps
        .iter()
        .map(|a| {
            let folder = if a.user { user.clone().unwrap_or_else(|| dir.clone()) } else { dir.clone() };
            AppRef { id: a.id.clone(), name: a.name.clone(), exec: a.exec.clone(), desktop: Some(folder.join(&a.id)), user: a.user }
        })
        .collect();
    v.extend(
        packages::catalog()
            .into_iter()
            .filter(|p| p.builtin != Builtin::Settings)
            .map(|p| AppRef { name: p.shown_name(), exec: format!("{}{}", packages::EXEC_SCHEME, p.id), id: p.id, desktop: None, user: false }),
    );
    v
}

fn normalized(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Build the home items: Settings, Terminal, installed apps (visible ones), then
/// Harbour's own tools. Icons for external apps are converted on first use.
fn rebuild_home<'r>(
    home: &mut Home,
    apps: &[AppEntry],
    assets: &mut AssetStore<'r>,
    creator: &'r sdl2::render::TextureCreator<sdl2::video::WindowContext>,
    cfg: &Arc<Mutex<Config>>,
) {
    let _ = &*assets;
    let pkgs = packages::catalog();
    let hidden = |id: &str| cfg.lock().map(|c| c.get_int(&desktop_config_key(id), 1) == 0).unwrap_or(false);
    // Settings can never be hidden (it is where apps are shown again).
    let tile = |p: &packages::Package| {
        let icon = if p.icon.is_some() { packages::icon_key(&p.id) } else { "icons/NoIcon".to_string() };
        Item::app(p.name.clone(), Target::Builtin(p.builtin), icon, None)
    };
    let shown = |p: &&packages::Package| p.builtin == Builtin::Settings || !hidden(&p.id);
    let mut items: Vec<Item> = pkgs.iter().take(packages::LEADING).filter(shown).map(tile).collect();
    let mut taken: Vec<String> = Vec::new();
    for app in apps.iter().filter(|a| !hidden(&a.id)) {
        let key = if let Some(k) = icons::known_icon_for(&app.name) {
            k
        } else {
            let tile = format!("tile/{}", app.id);
            if assets.get(&tile).is_none() {
                let rgba = icons::app_tile_rgba(app.icon.as_deref().map(std::path::Path::new));
                if let Err(e) = icons::insert_tile_rgba(assets, creator, &app.id, &rgba) {
                    eprintln!("[icons] {}: {e}", app.id);
                }
            }
            tile
        };
        taken.push(normalized(&app.name));
        items.push(Item::app(app.name.clone(), Target::External(app.id.clone()), key, None));
    }
    // An installed app of the same name takes precedence over Harbour's own.
    items.extend(pkgs.iter().skip(packages::LEADING).filter(|p| !taken.contains(&normalized(&p.name))).filter(shown).map(tile));
    home.set_items(items);
    // Apps that are gone for good (uninstalled) leave their folders; hidden ones stay.
    if !apps.is_empty() {
        let mut known: Vec<String> = apps.iter().map(|a| a.id.clone()).collect();
        known.extend(pkgs.iter().map(|p| p.id.clone()));
        home.prune_missing(&known);
    }
}

/// Is the internet reachable? A TCP connection to a public DNS server within 1.5 s (no data is sent).
fn reachable() -> bool {
    use std::net::{SocketAddr, TcpStream};
    ["1.1.1.1:53", "8.8.8.8:53"].iter().any(|a| a.parse::<SocketAddr>().ok().map(|a| TcpStream::connect_timeout(&a, Duration::from_millis(1500)).is_ok()).unwrap_or(false))
}

fn spawn_status(sys: Arc<dyn Backend>, status: Arc<Mutex<StatusView>>) {
    let mock = std::env::var("HARBOUR_MOCK").is_ok();
    // The web build has no threads: read the (pretend) status once.
    let once = harbour_shell::rt::WEB;
    let run = move || loop {
        let b = sys.battery();
        let w = sys.wifi_status();
        let eth = sys.eth_ip().is_some();
        let online = mock || once || reachable();
        let vol = sys.volume();
        if let Ok(mut s) = status.lock() {
            s.battery = b.percent;
            s.charging = b.charging;
            s.wifi = w.signal;
            s.wifi_radio = w.radio;
            s.ethernet = eth;
            s.online = online;
            s.volume = vol;
        }
        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    harbour_shell::rt::spawn(run);
}

/// Hardware media keys: volume / mute / brightness with an on-screen toast.
fn media(sys: &Arc<dyn Backend>, cfg: &Arc<Mutex<Config>>, vol: &mut (u8, bool), key: MediaKey) -> Toast {
    let until = Instant::now() + Duration::from_millis(1200);
    let save = |k: &str, v: i64| {
        if let Ok(mut c) = cfg.lock() {
            c.set_int(k, v);
            let _ = c.save();
        }
    };
    match key {
        MediaKey::VolumeUp | MediaKey::VolumeDown => {
            let step: i32 = if key == MediaKey::VolumeUp { 5 } else { -5 };
            vol.0 = (vol.0 as i32 + step).clamp(0, 100) as u8;
            vol.1 = false;
            let v = vol.0;
            let s = sys.clone();
            std::thread::spawn(move || {
                let _ = s.set_mute(false);
                let _ = s.set_volume(v);
            });
            save("volume", v as i64);
            Toast { text: format!("Volume {v}%"), fraction: Some(v as u32), until }
        }
        MediaKey::Mute => {
            vol.1 = !vol.1;
            let (m, s) = (vol.1, sys.clone());
            std::thread::spawn(move || {
                let _ = s.set_mute(m);
            });
            Toast { text: if m { "Muted".into() } else { format!("Volume {}%", vol.0) }, fraction: if m { None } else { Some(vol.0 as u32) }, until }
        }
        MediaKey::BrightnessUp | MediaKey::BrightnessDown => {
            let (cur, max) = sys.brightness().unwrap_or((50, 100));
            let pct = (cur * 100 / max.max(1)) as i32;
            let next = (pct + if key == MediaKey::BrightnessUp { 10 } else { -10 }).clamp(5, 100) as u32;
            let _ = sys.set_brightness((next * max / 100).max(1));
            save("brightness", next as i64);
            Toast { text: format!("Brightness {next}%"), fraction: Some(next), until }
        }
    }
}

/// Hand the screen and keyboard to an external app and wait for it to finish.
#[allow(clippy::too_many_arguments)]
fn run_external(
    fs: &FontSystem,
    tm: &ThemeManager,
    app: &AppEntry,
    workdir: &Path,
    shared: &Arc<Shared>,
    canvas: &mut Canvas<Window>,
    fb: &mut Option<Fb>,
    rx: &Receiver<Input>,
    toast: &mut Option<Toast>,
    dirty: &mut bool,
    last_input: &mut Instant,
) {
    use std::sync::atomic::Ordering;
    shared.suspended.store(true, Ordering::SeqCst);
    if let Some(f) = fb.as_mut() {
        f.enter_external(); // DRM: hand the LCD back to fbdev for the app
    }
    let started = Instant::now();
    // The loading screen (barber-pole bar) stays until the app draws on the LCD itself (or leaves).
    let mut shown = true;
    let mut first_draw: Option<Duration> = None;
    let mut frame = 0u32;
    let result = process::run_watched(&app.exec, workdir, shared, &mut |elapsed| {
        if !shown {
            return;
        }
        if first_draw.is_none() && frame > 1 && fb.as_ref().map_or(false, |f| f.taken_over()) {
            first_draw = Some(elapsed);
            shown = false;
            return;
        }
        chrome::draw_launching(canvas, fs, tm, &app.name, frame);
        canvas.present();
        if let Some(f) = fb.as_mut() {
            let _ = push_frame(canvas, f);
        }
        if frame == 20 {
            // Test hook: HARBOUR_LAUNCH_SHOT=<file.bmp> saves the loading screen.
            if let Ok(path) = std::env::var("HARBOUR_LAUNCH_SHOT") {
                let _ = save_shot(canvas, &path);
            }
        }
        frame += 1;
    });
    shared.suspended.store(false, Ordering::SeqCst);
    while rx.try_recv().is_ok() {}
    if let Some(f) = fb.as_mut() {
        f.leave_external();
        f.invalidate();
    }
    let _ = canvas;
    *dirty = true;
    *last_input = Instant::now();
    let until = Instant::now() + Duration::from_millis(2500);
    *toast = match result {
        Err(RunError::UnsafeExec) => Some(Toast { text: format!("{}: Exec not allowed", app.name), fraction: None, until }),
        Err(RunError::Spawn(e)) => {
            let why = if e.contains("No such file") {
                "program not found"
            } else if e.contains("Permission denied") {
                "not allowed"
            } else {
                "can't start"
            };
            Some(Toast { text: format!("{}: {why}", app.name), fraction: None, until })
        }
        Ok(st) if !st.success() && started.elapsed() < Duration::from_secs(3) => {
            Some(Toast { text: format!("{} exited ({})", app.name, st.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())), fraction: None, until })
        }
        Ok(_) => None,
    };
    let _ = desktop_entry::filename_valid("x.desktop");
}

fn save_note(editor: &mut TextEditor, path: &Path, toast: &mut Option<Toast>) {
    if !editor.is_dirty() {
        return;
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let until = Instant::now() + Duration::from_millis(1200);
    match std::fs::write(path, editor.text_for_save()) {
        Ok(()) => {
            editor.mark_clean();
            *toast = Some(Toast { text: harbour_shell::launcher::i18n::tr("Saved").into(), fraction: None, until });
        }
        Err(e) => *toast = Some(Toast { text: harbour_shell::launcher::i18n::tr_s(&format!("Save failed: {e}")), fraction: None, until }),
    }
}

fn draw_app(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, title: &str, hint: &str, body: impl FnOnce(&mut Canvas<Window>, i32, i32, u32, u32)) {
    draw_app_footer(c, fs, tm, title, "", hint, body)
}

fn draw_app_footer(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, title: &str, status: &str, hint: &str, body: impl FnOnce(&mut Canvas<Window>, i32, i32, u32, u32)) {
    // No window frame: the app fills the area under the menu strip (its name is in the menu strip).
    let _ = title;
    harbour_shell::ui::fill_rect(c, chrome::app_rect(), tm.get_color("home_bg"));
    // Under the menu strip's 1 px gap down to y 168, the last row the framed screens use.
    let top = harbour_shell::grid::BAR_H + harbour_shell::grid::WINDOW_INSET;
    let inner = Rect::new(harbour_shell::grid::MARGIN, top, (chrome::screen_w() as i32 - 2 * harbour_shell::grid::MARGIN) as u32, (harbour_shell::grid::frame_bottom() + 1 - top) as u32);
    let content = inner;
    body(c, content.x(), content.y(), content.width(), content.height());
    chrome::draw_command_bar(c, fs, tm, &inner, status, hint);
}

/// Copy the rendered frame to the LCD as RGB565.
/// A status item's menu, opened under it (`x`: the item's left edge on the strip).
fn status_menu(item: chrome::StatusItem, st: &StatusView, x: i32, ime_on: bool) -> harbour_shell::launcher::widgets::AppMenu {
    use chrome::StatusItem;
    use harbour_shell::launcher::i18n::tr;
    use harbour_shell::launcher::widgets::{AppMenu, MenuItem};
    let info = |t: String| MenuItem::new("st_info", t);
    let items = match item {
        StatusItem::Clock => vec![
            info(chrono::Local::now().format("%a %e %b %Y").to_string()),
            MenuItem::new("st_app_clock", harbour_shell::launcher::i18n::app_name("clock")),
            MenuItem::new("st_app_calendar", harbour_shell::launcher::i18n::app_name("calendar")),
            MenuItem::new("st_app_timer", harbour_shell::launcher::i18n::app_name("timer")),
        ],
        StatusItem::Battery => vec![
            info(match (st.battery, st.charging) {
                (Some(p), true) => format!("{} {p}%", tr("Charging")),
                (Some(p), false) => format!("{} {p}%", tr("Battery")),
                (None, _) => tr("Charging").to_string(),
            }),
            MenuItem::new("st_app_monitor", harbour_shell::launcher::i18n::app_name("monitor")),
            MenuItem::new("st_set_system", "System Settings..."),
        ],
        StatusItem::Network => vec![
            info(if st.ethernet { tr("Ethernet").to_string() } else if st.wifi_radio { format!("Wi-Fi {}", st.wifi.map(|w| format!("{w}%")).unwrap_or_default()) } else { tr("Wi-Fi off").to_string() }),
            MenuItem::new("st_wifi_toggle", if st.wifi_radio { "Turn Wi-Fi Off" } else { "Turn Wi-Fi On" }),
            MenuItem::new("st_set_network", "Network Settings..."),
            MenuItem::new("st_app_interceptor", harbour_shell::launcher::i18n::app_name("interceptor")),
        ],
        StatusItem::Internet => vec![
            info(tr(if st.online { "Online" } else { "Offline" }).to_string()),
            MenuItem::new("st_app_browser", harbour_shell::launcher::i18n::app_name("browser")),
            MenuItem::new("st_app_infoscope", harbour_shell::launcher::i18n::app_name("infoscope")),
            MenuItem::new("st_set_network", "Network Settings..."),
        ],
        StatusItem::Speaker => {
            let (v, muted) = st.volume.unwrap_or((0, false));
            vec![
                info(if muted { tr("Muted").to_string() } else { format!("{} {v}%", tr("Volume")) }),
                MenuItem::new("st_vol_up", "Louder"),
                MenuItem::new("st_vol_down", "Quieter"),
                MenuItem::new("st_mute", if muted { "Unmute" } else { "Mute" }),
                MenuItem::new("st_set_sound", "Sound Settings..."),
            ]
        }
        StatusItem::Ime => vec![
            info(tr(if ime_on { "Japanese input" } else { "Japanese input off" }).to_string()),
            MenuItem::new("st_ime_toggle", "Japanese / English (Ctrl+Space)"),
            MenuItem::new("st_ime_kana", "Hiragana / Katakana (Ctrl+Shift+Space)"),
        ],
    };
    // The first row says how things are; the selection starts on the first thing to do.
    let mut m = AppMenu::context(items, x, chrome::BAR_H);
    m.cursor = 1;
    m
}

/// On a desk, before opening something from a menu: Home takes the focus (the focused window keeps
/// its screen), so what opens gets a window of its own instead of replacing that window's screen.
fn desk_to_home(desk: &mut Option<Desk<Screen>>, fid: &mut Option<WinId>, screen: &mut Screen, prev_home: &mut bool) {
    let Some(d) = desk.as_mut() else { return };
    if let Some(f) = fid.take() {
        let out = std::mem::replace(screen, Screen::Home);
        if let Some(w) = d.get_mut(f) {
            w.slot = Some(out);
        }
    }
    d.focus_desk();
    *prev_home = true;
}

/// The system cursor in Harbour's arrow (`chrome::CURSOR_ART`), `scale` times bigger like the
/// window. None when the system cannot make one (the drawn pointer is used then).
fn make_os_cursor(tm: &ThemeManager, scale: u32) -> Option<sdl2::mouse::Cursor> {
    let (w, h) = (12 * scale, 16 * scale);
    let mut surf = Surface::new(w, h, PixelFormatEnum::ARGB8888).ok()?;
    let pitch = surf.pitch() as usize;
    let (ink, edge) = (tm.get_color("foreground"), tm.get_color("widget_face_active"));
    surf.with_lock_mut(|px| {
        for (row, line) in chrome::CURSOR_ART.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                let c = match ch {
                    b'X' => edge,
                    b'O' => ink,
                    _ => continue,
                };
                for dy in 0..scale as usize {
                    for dx in 0..scale as usize {
                        let (x, y) = (col * scale as usize + dx, row * scale as usize + dy);
                        let i = y * pitch + x * 4;
                        px[i..i + 4].copy_from_slice(&[c.b, c.g, c.r, 255]);
                    }
                }
            }
        }
    });
    let cursor = sdl2::mouse::Cursor::from_surface(surf, 0, 0).ok()?;
    cursor.set();
    Some(cursor)
}

/// `HARBOUR_SCREEN`: "480x272" -> (480, 272); never under 320 x 170.
fn parse_size(v: &str) -> Option<(i32, i32)> {
    let (w, h) = v.trim().split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    Some((w.clamp(SCREEN_W as i32, 8192), h.clamp(SCREEN_H as i32, 8192)))
}

/// The on-screen keyboard's room: how much lower a screen that lays itself out (and Home) becomes
/// while the keyboard is open, so nothing it shows is hidden behind it (0: no keyboard, a desk, the
/// lock screen or a fixed-size screen, which the keyboard just covers).
fn osk_room(open: bool, desk: bool, locked: bool, screen: &Screen, picture: (i32, i32)) -> i32 {
    if !open || desk || locked || !(matches!(screen, Screen::Home) || screen_flexible(screen)) {
        return 0;
    }
    harbour_shell::grid::with_surface(picture, || harbour_shell::launcher::osk::Osk::rect().height() as i32 + 2)
}

/// Full mode: where a screen that is always 320 x 170 (or the lock screen) is drawn, in the middle
/// of the picture; None for one that lays itself out over the whole picture (Home too).
fn full_viewport(screen: &Screen, locked: bool, size: (i32, i32)) -> Option<Rect> {
    if !locked && (matches!(screen, Screen::Home) || screen_flexible(screen)) {
        return None;
    }
    let (w, h) = (SCREEN_W as i32, SCREEN_H as i32);
    Some(Rect::new((size.0 - w) / 2, (size.1 - h) / 2, w as u32, h as u32))
}

/// On a desk: does this screen lay itself out from its window (so the window can be resized)?
fn screen_flexible(screen: &Screen) -> bool {
    match screen {
        Screen::App(a) => a.sizing() == harbour_shell::launcher::app::Sizing::Flexible,
        Screen::Settings(_) | Screen::Editor | Screen::Terminal(_) => true,
        Screen::Home | Screen::Calculator | Screen::Clock | Screen::Snake => false,
    }
}

/// A window's frame on the desk: a 1 px line around its inside and a 1 px shadow; the focused
/// window's line is the ink colour, the others' the softer line colour.
fn draw_window_frame(c: &mut Canvas<Window>, tm: &ThemeManager, r: Rect, focused: bool) {
    use harbour_shell::launcher::desk::outer;
    let o = outer(r);
    let line = if focused { tm.get_color("foreground") } else { tm.get_color("line") };
    let shadow = tm.get_color("widget_shadow");
    harbour_shell::ui::fill_rect(c, Rect::new(o.x() + 1, o.bottom() - 1, o.width() - 1, 1), shadow);
    harbour_shell::ui::fill_rect(c, Rect::new(o.right() - 1, o.y() + 1, 1, o.height() - 1), shadow);
    harbour_shell::ui::draw_rect(c, Rect::new(o.x(), o.y(), o.width() - 1, o.height() - 1), line);
}

/// A window's title bar on a desk (its top 16 px row): a close box at the left and the name in the
/// middle; the focused window's bar has a 1 px dither band on both sides of the name. The close box
/// shows a cross while the pointer is on it.
fn draw_title_bar(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, title: &str, focused: bool, close_hover: bool) {
    use harbour_shell::launcher::desk::CLOSE_BOX;
    let w = chrome::screen_w() as i32;
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("topbar_bg");
    harbour_shell::ui::fill_rect(c, Rect::new(0, 0, w as u32, chrome::BAR_H as u32), paper);
    harbour_shell::ui::line(c, 0, chrome::BAR_H - 1, w - 1, chrome::BAR_H - 1, ink);
    let text = chrome::fit_display_text(fs, title, w - 2 * (CLOSE_BOX.0 + CLOSE_BOX.2 + 12));
    let tw = fs.measure_display_text(&text);
    let tx = (w - tw) / 2;
    if focused {
        // A 1 px dither band from the close box to the name and on to the right edge.
        let (x, y, s) = CLOSE_BOX;
        let band = |c: &mut Canvas<Window>, x0: i32, x1: i32| {
            if x1 > x0 {
                harbour_shell::dither::fill(c, Rect::new(x0, 3, (x1 - x0) as u32, 10), ink, harbour_shell::dither::Pattern::Half);
            }
        };
        band(c, x + s + 3, tx - 5);
        band(c, tx + tw + 5, w - 3);
        harbour_shell::ui::draw_rect(c, Rect::new(x, y, s as u32, s as u32), ink);
        if close_hover {
            // Under the pointer the box shows what it does: a cross.
            harbour_shell::ui::line(c, x + 2, y + 2, x + s - 3, y + s - 3, ink);
            harbour_shell::ui::line(c, x + s - 3, y + 2, x + 2, y + s - 3, ink);
        }
    }
    fs.draw_display_text(c, tx, 1, &text, if focused { ink } else { harbour_shell::launcher::widgets::secondary_ink(tm) });
}

/// The resize mark of a flexible window: a small solid triangle in its bottom-right corner (the
/// grip it is resized from), drawn in desk coordinates.
fn draw_resize_mark(c: &mut Canvas<Window>, tm: &ThemeManager, r: Rect) {
    let ink = tm.get_color("foreground");
    let (x1, y1) = (r.right() - 2, r.bottom() - 2);
    for k in 0..6 {
        harbour_shell::ui::line(c, x1 - k, y1 - 5 + k, x1, y1 - 5 + k, ink);
    }
}

/// A window behind the focused one, drawn through its viewport: its title strip and its screen.
#[allow(clippy::too_many_arguments)]
fn draw_back_window(
    c: &mut Canvas<Window>,
    assets: &mut harbour_shell::assets::AssetStore,
    ctx: &Ctx,
    screen: &mut Screen,
    home: &mut Home,
    calc: &mut Calc,
    world: &mut WorldClock,
    snake: &SnakeState,
    editor: &mut TextEditor,
    bar_style: u8,
) {
    let (fs, tm) = (ctx.fs, ctx.tm);
    chrome::draw_desktop(c, tm);
    let title = match screen {
        Screen::Home => if home.context().is_empty() { "Harbour".to_string() } else { home.context().to_string() },
        Screen::Settings(s) => harbour_shell::launcher::i18n::tr_s(&s.title()),
        Screen::Calculator => harbour_shell::launcher::i18n::app_name("calculator"),
        Screen::Clock => harbour_shell::launcher::i18n::tr("World Clock").to_string(),
        Screen::Snake => "Snake".to_string(),
        Screen::Editor => harbour_shell::launcher::i18n::app_name("texteditor"),
        Screen::Terminal(_) => harbour_shell::launcher::i18n::app_name("terminal"),
        Screen::App(a) => fit_title(fs, &a.title()),
    };
    let _ = bar_style;
    draw_title_bar(c, fs, tm, &title, false, false);
    match screen {
        Screen::Home => {}
        Screen::Settings(s) => s.draw(c, fs, tm),
        Screen::Calculator => calc.draw(c, fs, tm, 0, chrome::BAR_H, chrome::screen_w(), chrome::screen_h() - chrome::BAR_H as u32),
        Screen::Clock => world.draw(c, assets, fs, tm, ctx.now),
        Screen::Snake => draw_app(c, fs, tm, "Snake", "", |c, x, y, w, h| snake.draw(c, fs, tm, x, y, w, h)),
        Screen::App(a) => a.draw(c, assets, ctx),
        Screen::Terminal(t) => t.draw(c, fs, tm),
        Screen::Editor => {
            harbour_shell::ui::fill_rect(c, Rect::new(0, chrome::BAR_H, chrome::screen_w(), chrome::screen_h() - chrome::BAR_H as u32), tm.get_color("home_bg"));
            harbour_shell::grid::draw_app_frame(c, harbour_shell::grid::app_frame().y(), tm.get_color("foreground"), None);
            let r = harbour_shell::grid::app_content();
            editor.draw(c, fs, tm, r.x(), r.y(), r.width(), r.height())
        }
    }
}

fn push_frame(canvas: &Canvas<Window>, fb: &mut Fb) -> Result<(), String> {
    let t0 = Instant::now();
    let px = canvas.read_pixels(None, PixelFormatEnum::RGB565)?;
    let t1 = Instant::now();
    let sent = fb.present(&px).map_err(|e| e.to_string())?;
    if fb_stats_enabled() {
        fb_stats_add(t1 - t0, t1.elapsed(), sent, fb.backend());
    }
    Ok(())
}

fn fb_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| env_bool("HARBOUR_FB_STATS"))
}

/// HARBOUR_FB_STATS=1: every 30 pushes print the mean read_pixels / present time and bytes sent.
fn fb_stats_add(read: Duration, present: Duration, sent: usize, backend: &str) {
    use std::cell::RefCell;
    thread_local!(static ST: RefCell<(u32, u32, Duration, Duration, usize)> = RefCell::new((0, 0, Duration::ZERO, Duration::ZERO, 0)));
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.0 += 1;
        if sent > 0 {
            s.1 += 1;
        }
        s.2 += read;
        s.3 += present;
        s.4 += sent;
        if s.0 >= 30 {
            eprintln!(
                "[fbstats] {backend}: {} pushes, {} with damage; read_pixels {:.2} ms, present {:.2} ms (mean), {} B/push",
                s.0,
                s.1,
                s.2.as_secs_f64() * 1000.0 / 30.0,
                s.3.as_secs_f64() * 1000.0 / 30.0,
                s.4 / s.0 as usize
            );
            *s = (0, 0, Duration::ZERO, Duration::ZERO, 0);
        }
    });
}

fn save_shot(canvas: &Canvas<Window>, path: &str) -> Result<(), String> {
    let (w, h) = canvas.output_size()?;
    // A grey or 1-bit panel (HARBOUR_PANEL): the shot shows what that panel would.
    let kind = harbour_shell::platform::display::PanelKind::from_env(harbour_shell::platform::display::PanelKind::Color);
    if kind != harbour_shell::platform::display::PanelKind::Color {
        let mut px = canvas.read_pixels(None, PixelFormatEnum::RGB565)?;
        harbour_shell::platform::display::tone(&mut px, w as usize, h as usize, kind);
        let surf = Surface::from_data(&mut px, w, h, w * 2, PixelFormatEnum::RGB565)?;
        return surf.save_bmp(path);
    }
    let mut px = canvas.read_pixels(None, PixelFormatEnum::ARGB8888)?;
    let surf = Surface::from_data(&mut px, w, h, w * 4, PixelFormatEnum::ARGB8888)?;
    surf.save_bmp(path)
}

// ---- scripted runs -----------------------------------------------------------------------------------

enum ScriptStep {
    Idle,
    Input(Vec<Input>),
    /// `theme:N[:invert]`: switch palette now (config file rewritten, applied at once).
    Theme(i64, bool),
    /// `theme-ui:N`: press Left/Right on the Theme row of Settings > Theme until palette N.
    ThemeUi(i64),
    /// `goto:Name`: put the Home selection on the tile with that name (current folder and view).
    Goto(String),
    /// `expect:k=v k=v` (or `state`, which only prints): compare the shell's state (screen, agent,
    /// menu, modal, locked, zoom); a mismatch is printed and the run exits with status 3 at the end.
    Expect(String),
    Done,
}

struct Script {
    tokens: Vec<String>,
    pos: usize,
    resume: Instant,
    pending_shot: Option<String>,
    /// Key to release once `resume` passes (for `hold:`).
    pending_up: Option<Keycode>,
    /// Pointer position the script has moved to (for `click:x:y`).
    pointer: (i32, i32),
    /// `expect:` steps that did not match.
    failed: usize,
}

impl Script {
    fn from_env() -> Option<Self> {
        let tokens = if let Ok(path) = std::env::var("HARBOUR_SCRIPT_FILE") {
            match load_script_file(Path::new(&path), 0) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("demo script: {e}");
                    std::process::exit(2);
                }
            }
        } else {
            parse_script(&std::env::var("HARBOUR_SCRIPT").ok()?)
        };
        Some(Self {
            tokens,
            pos: 0,
            resume: Instant::now() + Duration::from_millis(300),
            pending_shot: None,
            pending_up: None,
            pointer: std::env::var("HARBOUR_CURSOR")
                .ok()
                .and_then(|v| {
                    let mut it = v.split(',').filter_map(|p| p.parse::<i32>().ok());
                    Some((it.next()?, it.next()?))
                })
                .unwrap_or((SCREEN_W as i32 / 2, SCREEN_H as i32 / 2)),
            failed: 0,
        })
    }

    fn step(&mut self) -> ScriptStep {
        if Instant::now() < self.resume || self.pending_shot.is_some() {
            return ScriptStep::Idle;
        }
        if let Some(key) = self.pending_up.take() {
            return ScriptStep::Input(vec![Input::KeyUp { key }]);
        }
        let Some(tok) = self.tokens.get(self.pos).cloned() else {
            return ScriptStep::Done;
        };
        self.pos += 1;
        if let Some(ms) = tok.strip_prefix("wait:") {
            self.resume = Instant::now() + Duration::from_millis(ms.parse().unwrap_or(100));
            return ScriptStep::Idle;
        }
        if let Some(spec) = tok.strip_prefix("expect:") {
            return ScriptStep::Expect(spec.replace('&', " "));
        }
        if tok == "state" {
            return ScriptStep::Expect(String::new());
        }
        if let Some(name) = tok.strip_prefix("goto:") {
            self.resume = Instant::now() + Duration::from_millis(60);
            return ScriptStep::Goto(name.trim().to_string());
        }
        if let Some(rest) = tok.strip_prefix("theme-ui:") {
            self.resume = Instant::now() + Duration::from_millis(60);
            return ScriptStep::ThemeUi(rest.parse().unwrap_or(0));
        }
        if let Some(rest) = tok.strip_prefix("theme:") {
            let mut it = rest.split(':');
            let n = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let invert = matches!(it.next(), Some("invert") | Some("inv") | Some("1"));
            self.resume = Instant::now() + Duration::from_millis(60);
            return ScriptStep::Theme(n, invert);
        }
        if let Some(name) = tok.strip_prefix("shot:") {
            self.pending_shot = Some(name.to_string());
            return ScriptStep::Idle;
        }
        // down:Key / up:Key -> press without release, release (hold gauges).
        for (prefix, down) in [("down:", true), ("up:", false)] {
            if let Some(name) = tok.strip_prefix(prefix) {
                return match Keycode::from_name(name) {
                    Some(key) if down => ScriptStep::Input(vec![Input::Key { key, mods: Mod::NOMOD, repeat: false }]),
                    Some(key) => ScriptStep::Input(vec![Input::KeyUp { key }]),
                    None => ScriptStep::Idle,
                };
            }
        }
        // hold:Key:ms  -> press, wait, release (to see hold gauges).
        if let Some(rest) = tok.strip_prefix("hold:") {
            let mut it = rest.split(':');
            let key = it.next().and_then(Keycode::from_name);
            let ms: u64 = it.next().and_then(|m| m.parse().ok()).unwrap_or(500);
            if let Some(key) = key {
                self.resume = Instant::now() + Duration::from_millis(ms);
                self.pending_up = Some(key);
                return ScriptStep::Input(vec![Input::Key { key, mods: Mod::NOMOD, repeat: false }]);
            }
            return ScriptStep::Idle;
        }
        // click:x:y / rclick:x:y / move:x:y / wheel:n -> mouse.
        for (prefix, button) in [("click:", 0u8), ("rclick:", 1u8), ("move:", 255u8)] {
            if let Some(rest) = tok.strip_prefix(prefix) {
                let v: Vec<i32> = rest.split(':').filter_map(|p| p.parse().ok()).collect();
                if let [x, y] = v[..] {
                    let (dx, dy) = (x - self.pointer.0, y - self.pointer.1);
                    self.pointer = (x, y);
                    self.resume = Instant::now() + Duration::from_millis(120);
                    let mut out = vec![Input::MouseMove { dx, dy }];
                    if button != 255 {
                        out.push(Input::MouseButton { button, down: true });
                        out.push(Input::MouseButton { button, down: false });
                    }
                    return ScriptStep::Input(out);
                }
                return ScriptStep::Idle;
            }
        }
        // drag:x1:y1:x2:y2 -> move to (x1,y1), press, move to (x2,y2) in steps, release.
        if let Some(rest) = tok.strip_prefix("drag:") {
            let v: Vec<i32> = rest.split(':').filter_map(|p| p.parse().ok()).collect();
            if let [x1, y1, x2, y2] = v[..] {
                let mut out = vec![Input::MouseMove { dx: x1 - self.pointer.0, dy: y1 - self.pointer.1 }, Input::MouseButton { button: 0, down: true }];
                let (mut px, mut py) = (x1, y1);
                for k in 1..=6 {
                    let (nx, ny) = (x1 + (x2 - x1) * k / 6, y1 + (y2 - y1) * k / 6);
                    out.push(Input::MouseMove { dx: nx - px, dy: ny - py });
                    (px, py) = (nx, ny);
                }
                out.push(Input::MouseButton { button: 0, down: false });
                self.pointer = (x2, y2);
                self.resume = Instant::now() + Duration::from_millis(120);
                if rest.ends_with(":hold") {
                    out.pop(); // stay pressed; `release` lets go
                }
                return ScriptStep::Input(out);
            }
            return ScriptStep::Idle;
        }
        if let Some(n) = tok.strip_prefix("wheel:") {
            self.resume = Instant::now() + Duration::from_millis(120);
            return ScriptStep::Input(vec![Input::MouseWheel { dy: n.parse().unwrap_or(1) }]);
        }
        if tok == "release" {
            self.resume = Instant::now() + Duration::from_millis(120);
            return ScriptStep::Input(vec![Input::MouseButton { button: 0, down: false }]);
        }
        // voicedown / voiceup: push-to-talk without a Sym key (screenshots).
        if tok == "voicedown" || tok == "voiceup" {
            return ScriptStep::Input(vec![Input::Voice(tok == "voicedown")]);
        }
        // rep:Key -> one auto-repeat press of a held key (no release).
        if let Some(name) = tok.strip_prefix("rep:") {
            return match Keycode::from_name(name) {
                Some(key) => ScriptStep::Input(vec![Input::Key { key, mods: Mod::NOMOD, repeat: true }]),
                None => ScriptStep::Idle,
            };
        }
        // pad:a (press and release a gamepad button), stick:x:y (hold the stick; stick:0:0 lets go).
        if let Some(name) = tok.strip_prefix("pad:") {
            return match harbour_shell::launcher::osk::Pad::from_name(name) {
                Some(pad) => {
                    self.resume = Instant::now() + Duration::from_millis(60);
                    ScriptStep::Input(vec![Input::Pad { pad, down: true }, Input::Pad { pad, down: false }])
                }
                None => {
                    eprintln!("script: unknown key {tok:?}");
                    ScriptStep::Idle
                }
            };
        }
        if let Some(rest) = tok.strip_prefix("stick:") {
            let v: Vec<i16> = rest.split(':').filter_map(|p| p.parse().ok()).collect();
            if let [x, y] = v[..] {
                return ScriptStep::Input(vec![Input::Stick { x, y }]);
            }
            return ScriptStep::Idle;
        }
        if tok == "menu" {
            return ScriptStep::Input(vec![Input::Menu]);
        }
        if let Some(text) = tok.strip_prefix("text:") {
            return ScriptStep::Input(vec![Input::Text(text.to_string())]);
        }
        let (mods, name) = if let Some(n) = tok.strip_prefix("ctrl+") {
            (Mod::LCTRLMOD, n)
        } else if let Some(n) = tok.strip_prefix("alt+") {
            (Mod::LALTMOD, n)
        } else if let Some(n) = tok.strip_prefix("shift+") {
            (Mod::LSHIFTMOD, n)
        } else {
            (Mod::NOMOD, tok.as_str())
        };
        match Keycode::from_name(name) {
            Some(key) => {
                self.resume = Instant::now() + Duration::from_millis(60);
                ScriptStep::Input(vec![Input::Key { key, mods, repeat: false }, Input::KeyUp { key }])
            }
            None => {
                eprintln!("script: unknown key {name:?}");
                ScriptStep::Idle
            }
        }
    }
}

#[allow(dead_code)]
fn _unused(_: Color) {}

/// Script text to tokens: separated by commas or newlines; `#` starts a comment (to end of line).
fn parse_script(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        out.extend(line.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()));
    }
    out
}

/// A script file. `include:other.script` (relative to the including file) splices another file in,
/// so a demo can be split into chapters.
fn load_script_file(path: &Path, depth: u32) -> Result<Vec<String>, String> {
    if depth > 8 {
        return Err(format!("{}: includes nested too deeply", path.display()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    for tok in parse_script(&text) {
        match tok.strip_prefix("include:") {
            Some(rel) => out.extend(load_script_file(&base.join(rel.trim()), depth + 1)?),
            None => out.push(tok),
        }
    }
    Ok(out)
}

// ---- frame recording ---------------------------------------------------------------------------------

/// HARBOUR_REC_DIR=dir (or `--rec dir`) writes `dir/frames.rgb565`: the rendered 320x170 frames as one raw
/// RGB565-LE stream at a constant HARBOUR_REC_FPS (default 15), the last frame repeated while nothing changes, so
/// the file is already in real time. Nothing is compressed on the device (compress on the Mac with
/// tools/rec_to_mp4.sh). HARBOUR_REC_FILE=path overrides the stream's destination (a FIFO, or /dev/stdout to pipe
/// through ssh); the stream is then not seekable but identical. `dir/rec.txt` records fps and size.
struct Recorder {
    out: std::io::BufWriter<Box<dyn std::io::Write>>,
    fps: u32,
    start: Instant,
    written: u64,
    last: Vec<u8>,
    failed: bool,
}

use std::io::Write as _;

impl Recorder {
    fn from_env() -> Option<Self> {
        let dir = std::env::var("HARBOUR_REC_DIR").ok();
        let file = std::env::var("HARBOUR_REC_FILE").ok();
        if dir.is_none() && file.is_none() {
            return None;
        }
        let fps: u32 = std::env::var("HARBOUR_REC_FPS").ok().and_then(|v| v.parse().ok()).filter(|f| (1..=60).contains(f)).unwrap_or(15);
        if let Some(d) = &dir {
            if let Err(e) = std::fs::create_dir_all(d) {
                eprintln!("[rec] {d}: {e}");
                return None;
            }
            let info = format!("size={SCREEN_W}x{SCREEN_H}\npix_fmt=rgb565le\nfps={fps}\nfile=frames.rgb565\n");
            let _ = std::fs::write(Path::new(d).join("rec.txt"), info);
        }
        let path = file.unwrap_or_else(|| format!("{}/frames.rgb565", dir.unwrap_or_default()));
        let sink: Box<dyn std::io::Write> = match std::fs::File::create(&path) {
            Ok(f) => Box::new(f),
            Err(e) => {
                eprintln!("[rec] {path}: {e}");
                return None;
            }
        };
        eprintln!("[rec] recording {SCREEN_W}x{SCREEN_H} RGB565 at {fps} fps to {path}");
        Some(Self {
            out: std::io::BufWriter::with_capacity(1 << 20, sink),
            fps,
            start: Instant::now(),
            written: 0,
            last: Vec::new(),
            failed: false,
        })
    }

    /// Remember the frame just presented.
    fn capture(&mut self, canvas: &Canvas<Window>) {
        if let Ok(px) = canvas.read_pixels(None, PixelFormatEnum::RGB565) {
            self.last = px;
        }
        self.tick();
    }

    /// Write as many repeats of the latest frame as wall-clock time says are due.
    fn tick(&mut self) {
        if self.last.is_empty() || self.failed {
            return;
        }
        let due = (self.start.elapsed().as_secs_f64() * self.fps as f64) as u64;
        while self.written < due {
            if self.out.write_all(&self.last).is_err() {
                self.failed = true;
                eprintln!("[rec] write failed, recording stopped");
                return;
            }
            self.written += 1;
        }
    }

    fn finish(&mut self) {
        self.tick();
        let _ = self.out.flush();
        eprintln!("[rec] {} frames ({:.1} s)", self.written, self.written as f64 / self.fps as f64);
    }
}


/// The standard menu for the screen in front (Tab / right click).
/// The Harbour anchor's menu: the things that belong to the device, not to the app on screen.
fn system_menu(screen: &Screen) -> harbour_shell::launcher::widgets::AppMenu {
    use harbour_shell::launcher::widgets::{AppMenu, MenuItem};
    let mut items = vec![MenuItem::new("sys_about", "About Harbour"), MenuItem::new("sys_settings", "Settings...")];
    if !matches!(screen, Screen::Home) {
        items.push(MenuItem::new("sys_home", "Home"));
    }
    items.push(MenuItem::new("sys_help", "Help"));
    items.push(MenuItem::new("sys_agent", "Agent"));
    items.push(MenuItem::new("sys_lock", "Lock Screen"));
    items.push(MenuItem::new("sys_restart", "Restart..."));
    items.push(MenuItem::new("sys_poweroff", "Power Off..."));
    AppMenu::plain(items)
}

/// Help for the screen in front: its name and its own keys (apps give theirs; the host's screens here).
fn screen_help(screen: &Screen, agent_open: bool) -> (String, Vec<(String, String)>) {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if agent_open {
        return ("Agent".into(), vec![k("Return", "Send"), k("Up / Down", "Scroll the chat"), k("Tab Tab", "Close the panel")]);
    }
    match screen {
        Screen::App(a) => (a.title(), a.help()),
        Screen::Home => ("Home".into(), vec![
            k("Arrows", "Move the selection"),
            k("Return", "Open"),
            k("Right click", "Folders: new, rename, move"),
            k("Esc", "Leave a folder"),
        ]),
        Screen::Settings(_) => ("Settings".into(), vec![
            k("Up / Down", "Choose a row"),
            k("Return", "Change it / open"),
            k("Left / Right", "Choose a value"),
            k("Esc", "Back"),
            k("Tab", "Show / hide a password"),
        ]),
        Screen::Editor => ("Text Editor".into(), vec![k("Arrows", "Move the caret"), k("Tab", "Indent"), k("Alt", "New, Open, Save")]),
        Screen::Terminal(_) => ("Terminal".into(), vec![k("Tab", "Completion (sent to the shell)"), k("Esc (hold)", "Leave")]),
        Screen::Calculator => ("Calculator".into(), vec![k("Letters", "Functions on the keys"), k("Return", "="), k("Backspace", "Delete"), k("Left / Right", "Settings: change a value")]),
        Screen::Clock => ("World Clock".into(), vec![k("Left / Right", "Move the map"), k("Up / Down", "Choose a city")]),
        Screen::Snake => ("Snake".into(), vec![k("Arrows", "Steer"), k("Space", "Play again")]),
    }
}

/// Ctrl or Alt held: Tab is part of a chord (Ctrl+Tab menu, Alt+Tab Agent), not a tap.
const CHORD_MODS: Mod = Mod::from_bits_truncate(Mod::LCTRLMOD.bits() | Mod::RCTRLMOD.bits() | Mod::LALTMOD.bits() | Mod::RALTMOD.bits());

fn is_modifier_key(k: Keycode) -> bool {
    matches!(k, Keycode::LShift | Keycode::RShift | Keycode::LCtrl | Keycode::RCtrl | Keycode::LAlt | Keycode::RAlt | Keycode::LGui | Keycode::RGui)
}

/// Tab long locks everywhere but the Terminal (a shell program may need a held Tab); it always unlocks.
fn tab_lock_allowed(locked: bool, screen: &Screen) -> bool {
    locked || !matches!(screen, Screen::Terminal(_))
}

/// True where Tab is the screen's own key (Terminal, the Text Editor, Settings' text fields, apps
/// with tab bars or field focus): there a double Tab is two Tabs, not the Agent gesture.
fn screen_uses_tab(screen: &Screen) -> bool {
    match screen {
        Screen::Terminal(_) | Screen::Editor => true,
        Screen::Settings(s) => s.wants_tab(),
        Screen::App(a) => a.wants_tab(),
        Screen::Home | Screen::Calculator | Screen::Clock | Screen::Snake => false,
    }
}

/// Open or close the Agent panel (Tab Tab, Alt+Tab, Harbour menu > Agent). Off in Settings: a toast.
fn toggle_agent(open: &mut bool, agent: &mut Option<Box<AgentApp>>, cfg: &Arc<Mutex<Config>>, toast: &mut Option<Toast>) {
    if harbour_shell::rt::WEB {
        *toast = Some(Toast { text: "No Agent in the web demo".to_string(), fraction: None, until: Instant::now() + Duration::from_millis(1500) });
        return;
    }
    if !*open && !agent::history::load_state(&agent::config::data_dir()).enabled {
        *toast = Some(Toast { text: "Agent is off (Settings)".to_string(), fraction: None, until: Instant::now() + Duration::from_millis(1500) });
        return;
    }
    *open = !*open;
    if *open && agent.is_none() {
        *agent = Some(Box::new(agent::app::make_with(cfg.clone(), env_bool("HARBOUR_MOCK"))));
    }
}

/// True for the Harbour (system) menu, whose entries all carry the `sys_` prefix.
fn is_system_menu(m: &harbour_shell::launcher::widgets::AppMenu) -> bool {
    m.items.first().is_some_and(|i| i.id.starts_with("sys_"))
}

fn open_menu(screen: &Screen, home: &Home, agent: Option<&AgentApp>) -> harbour_shell::launcher::widgets::AppMenu {
    use harbour_shell::launcher::widgets::{AppMenu, MenuItem};
    if let Some(a) = agent {
        return AppMenu::new(a.menu());
    }
    if matches!(screen, Screen::Home) {
        // The home menu has no Quit: Home is where everything else quits to.
        let items = home.menu_entries().into_iter().map(|(id, label)| MenuItem::new(id, label)).collect();
        return AppMenu::plain(items);
    }
    let items = match screen {
        Screen::Calculator => vec![MenuItem::new("settings", "Settings..."), MenuItem::new("clear", "Clear")],
        Screen::Clock => vec![MenuItem::new("prev", "Previous City"), MenuItem::new("next", "Next City")],
        Screen::Snake => vec![MenuItem::new("restart", "Restart")],
        Screen::Editor => vec![MenuItem::new("new", "New..."), MenuItem::new("open", "Open..."), MenuItem::new("save", "Save"), MenuItem::new("saveas", "Save As...")],
        Screen::Terminal(_) => vec![MenuItem::new("clear", "Clear Screen")],
        Screen::App(a) => a.menu(),
        _ => vec![],
    };
    AppMenu::new(items)
}

/// Widest app title the menu strip shows before the status icons (longer names get "...").
const STRIP_TITLE_W: i32 = 120;

/// Fit a title to the menu strip; a trailing " *" (unsaved changes) always stays visible.
fn fit_title(fs: &FontSystem, title: &str) -> String {
    match title.strip_suffix(" *") {
        Some(name) => format!("{} *", chrome::fit_display_text(fs, name, STRIP_TITLE_W - 12)),
        None => chrome::fit_display_text(fs, title, STRIP_TITLE_W),
    }
}

/// The Text Editor's name in the menu strip: the file, with * while it has unsaved changes.
fn editor_title(path: &Path, dirty: bool) -> String {
    let name = note_name(path);
    if dirty { format!("{name} *") } else { name }
}

/// File name of the note shown in dialogs.
fn note_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "note.txt".into())
}

#[cfg(test)]
mod app_action_tests {
    use super::*;

    #[test]
    fn clicks_on_different_positions_do_not_count_as_double() {
        let now = Instant::now();
        let last = Some((usize::MAX - 1, now, 10, 10));
        assert!(is_double_click(last, usize::MAX - 1, 13, 13));
        assert!(!is_double_click(last, usize::MAX - 1, 100, 100));
        assert!(!is_double_click(last, usize::MAX, 10, 10));
    }

    fn script(tokens: &[&str]) -> Script {
        Script {
            tokens: tokens.iter().map(|t| t.to_string()).collect(),
            pos: 0,
            resume: Instant::now() - Duration::from_secs(1),
            pending_shot: None,
            pending_up: None,
            pointer: (0, 0),
            failed: 0,
        }
    }

    #[test]
    fn script_chords_and_expect_steps() {
        let mut s = script(&["alt+Tab", "shift+Tab", "expect:agent=1&menu=0", "state"]);
        match s.step() {
            ScriptStep::Input(v) => assert_eq!(v[0], Input::Key { key: Keycode::Tab, mods: Mod::LALTMOD, repeat: false }),
            _ => panic!("alt+Tab is a key"),
        }
        s.resume = Instant::now() - Duration::from_secs(1);
        match s.step() {
            ScriptStep::Input(v) => assert_eq!(v[0], Input::Key { key: Keycode::Tab, mods: Mod::LSHIFTMOD, repeat: false }),
            _ => panic!("shift+Tab is a key"),
        }
        s.resume = Instant::now() - Duration::from_secs(1);
        assert!(matches!(s.step(), ScriptStep::Expect(e) if e == "agent=1 menu=0"));
        assert!(matches!(s.step(), ScriptStep::Expect(e) if e.is_empty()));
    }

    #[test]
    fn script_text_takes_commas_newlines_and_comments() {
        let t = parse_script("Right, wait:10 # skip\n\n# whole line\ntheme:2:invert,Return\n");
        assert_eq!(t, ["Right", "wait:10", "theme:2:invert", "Return"]);
    }

    #[test]
    fn script_files_include_chapters_relative_to_themselves() {
        let dir = std::env::temp_dir().join(format!("harbour-script-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ch")).unwrap();
        std::fs::write(dir.join("ch/a.script"), "Left # a\nwait:5").unwrap();
        std::fs::write(dir.join("main.script"), "Right\ninclude:ch/a.script\nDown").unwrap();
        assert_eq!(load_script_file(&dir.join("main.script"), 0).unwrap(), ["Right", "Left", "wait:5", "Down"]);
        std::fs::write(dir.join("loop.script"), "include:loop.script").unwrap();
        assert!(load_script_file(&dir.join("loop.script"), 0).is_err(), "recursion is cut off");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_and_goto_tokens() {
        let mut s = script(&["theme:2:invert", "theme:1", "theme-ui:3", "goto:Text Editor"]);
        assert!(matches!(s.step(), ScriptStep::Theme(2, true)));
        s.resume = Instant::now() - Duration::from_secs(1);
        assert!(matches!(s.step(), ScriptStep::Theme(1, false)));
        s.resume = Instant::now() - Duration::from_secs(1);
        assert!(matches!(s.step(), ScriptStep::ThemeUi(3)));
        s.resume = Instant::now() - Duration::from_secs(1);
        assert!(matches!(s.step(), ScriptStep::Goto(n) if n == "Text Editor"));
    }

    #[test]
    fn the_drag_token_moves_presses_steps_and_releases() {
        let mut s = script(&["drag:10:20:70:20"]);
        let ScriptStep::Input(v) = s.step() else { panic!("expected input") };
        assert!(matches!(v[0], Input::MouseMove { dx: 10, dy: 20 }), "moves to the start first");
        assert!(matches!(v[1], Input::MouseButton { button: 0, down: true }));
        assert!(matches!(v.last(), Some(Input::MouseButton { button: 0, down: false })));
        let moved: i32 = v[2..v.len() - 1].iter().map(|i| if let Input::MouseMove { dx, .. } = i { *dx } else { 0 }).sum();
        assert_eq!(moved, 60, "steps add up to the whole drag");
        assert_eq!(s.pointer, (70, 20));
    }

    #[test]
    fn drag_hold_keeps_the_button_down_until_release() {
        let mut s = script(&["drag:0:0:30:0:hold", "release"]);
        let ScriptStep::Input(v) = s.step() else { panic!() };
        assert!(!matches!(v.last(), Some(Input::MouseButton { down: false, .. })));
        s.resume = Instant::now() - Duration::from_secs(1);
        let ScriptStep::Input(v) = s.step() else { panic!() };
        assert!(matches!(v[0], Input::MouseButton { button: 0, down: false }));
    }

    #[test]
    fn dispatches_toast_quit_and_empty_command() {
        let mut screen = Screen::Calculator;
        let mut toast = None;
        let shared = Arc::new(Shared::default());
        apply_app_action(AppAction::Toast("hello".into()), &mut screen, Path::new("/"), &shared, &mut toast);
        assert_eq!(toast.as_ref().map(|t| t.text.as_str()), Some("hello"));
        apply_app_action(AppAction::Launch(" ".into()), &mut screen, Path::new("/"), &shared, &mut toast);
        assert_eq!(toast.as_ref().map(|t| t.text.as_str()), Some("Launch: empty command"));
        apply_app_action(AppAction::Quit, &mut screen, Path::new("/"), &shared, &mut toast);
        assert!(matches!(screen, Screen::Home));
    }
}

// ---- Agent helpers --------------------------------------------------------------------------------

/// Everything `launch_app` can open: Harbour's own apps first (by their English name), then the installed ones.
fn launchable_names(apps: &[AppEntry]) -> Vec<String> {
    let mut v: Vec<String> = packages::catalog().into_iter().map(|p| p.name).collect();
    v.extend(apps.iter().map(|a| a.name.clone()));
    v
}

/// A name (case and punctuation ignored) to the thing to open, with the name to show. Built-ins
/// also answer to their id, Japanese name and old names.
fn resolve_target(name: &str, apps: &[AppEntry]) -> Option<(Target, String)> {
    let want = normalized(name);
    if want.is_empty() {
        return None;
    }
    if let Some(a) = apps.iter().find(|a| normalized(&a.name) == want) {
        return Some((Target::External(a.id.clone()), a.name.clone()));
    }
    packages::find(name).map(|p| (Target::Builtin(p.builtin), p.name))
}

/// What the screen behind the chat shows, as compact text for the `current_screen` tool.
fn describe_screen(screen: &Screen, home: &Home, world: &WorldClock, editor: &TextEditor, snake: &SnakeState) -> String {
    match screen {
        Screen::Home => {
            let names: Vec<String> = home.items.iter().map(|i| i.name.clone()).collect();
            let sel = home.items.get(home.selected).map(|i| i.name.clone()).unwrap_or_default();
            let ctx = if home.context().is_empty() { "top level".to_string() } else { format!("folder {}", home.context()) };
            format!("Home screen ({}, {}). Selected: {sel}. Items: {}", if home.view == View::List { "list" } else { "grid" }, ctx, names.join(", "))
        }
        Screen::Settings(s) => s.describe(),
        Screen::Calculator => "Calculator".into(),
        Screen::Clock => world.describe(chrono::Local::now()),
        Screen::Snake => format!("Snake: {}", snake.footer_text()),
        Screen::Editor => format!("Text Editor{}", if editor.is_dirty() { " (unsaved changes)" } else { "" }),
        Screen::Terminal(_) => "Terminal (a shell; its contents are not readable)".into(),
        Screen::App(a) => a.describe(),
    }
}
