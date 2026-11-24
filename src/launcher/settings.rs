//! Harbour Settings: the device settings the official launcher offers
//! (Display, Theme, Lock Screen, Sound, Network, Bluetooth, Devices, System, Apps, Agent),
//! drawn in the Harbour amber style. Hardware access goes through
//! [`crate::system::Backend`]; settings shared with the official launcher are
//! written to the same `config.json`.
//!
//! Slow operations (Wi-Fi scan/connect, Bluetooth, apt) run on worker threads
//! and report back through a channel polled every frame.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::chrome;
use crate::dither::{self, Pattern};
use crate::launcher::widgets::{self, BtnState, Dropdown, SbPart, ScrollBar};
use crate::grid::SCROLL_W;
use crate::font::FontSystem;
use crate::launcher::agent::{config as agent_config, history as agent_history};
use crate::launcher::i18n;
use crate::launcher::qr;
use crate::launcher::registry;
use crate::system::config::{desktop_config_key, Config};
use crate::system::{Ap, Backend, BtDevice, ExtPort, WifiStatus, R};
use crate::theme::ThemeManager;
use crate::ui;

/// Screen-off choices in seconds (same list as the official launcher).
pub const DARK_TIMES: [i64; 5] = [0, 10, 30, 60, 300];
/// Lock-screen-off choices in seconds (0 = same as Screen off).
pub const LOCK_DARK_TIMES: [i64; 6] = [0, 5, 8, 15, 30, 60];
/// Auto lock choices in seconds of idle time (0 = never): `harbour_lock_auto`.
pub const AUTO_LOCK_TIMES: [i64; 4] = [0, 60, 300, 900];
/// Lock screen styles (index = `harbour_lock_style`), see [`crate::launcher::lock::Style`].
pub const LOCK_STYLES: [&str; 4] = ["Wall clock", "World Clock", "ASCII art", "7-segment"];
/// Status bar colours (index = `harbour_statusbar`).
const BAR_STYLES: [&str; 3] = ["Default", "Black", "White"];
/// Colour themes (index = `harbour_theme` in config.json).
pub const THEMES: [(&str, &str); 5] = [
    ("Amber", "assets/themes/theme-black.yaml"),
    ("Sky", "assets/themes/theme-paper.yaml"),
    ("Mobile", "assets/themes/theme-mobile.yaml"),
    ("Terminal", "assets/themes/theme-terminal.yaml"),
    ("Platinum", "assets/themes/theme-platinum.yaml"),
];

/// A grey or black-and-white panel has one theme of its own (name, file): the colour themes are not
/// offered there. Set once at start-up by the host from the panel it found.
static PANEL_THEME: std::sync::OnceLock<(&'static str, &'static str)> = std::sync::OnceLock::new();

/// The panel shows only greys (`gray`) or only black and white (`mono`): fix the theme.
pub fn fix_theme_for_panel(mono: bool) {
    let _ = PANEL_THEME.set(if mono { ("1-bit", "assets/themes/theme-mono.yaml") } else { ("Gray", "assets/themes/theme-gray.yaml") });
}

/// True on a black-and-white panel: every "fainter" ink prints as plain black there.
pub fn panel_is_mono() -> bool {
    PANEL_THEME.get().is_some_and(|p| p.0 == "1-bit")
}

fn panel_theme() -> Option<(&'static str, &'static str)> {
    PANEL_THEME.get().copied()
}

/// The theme file for theme `index` (clamped) and the Invert switch: `theme-<name>-invert.yaml`
/// is the same palette with the tones reversed. A grey or 1-bit panel always gets its own theme.
pub fn theme_path(index: i64, invert: bool) -> String {
    let base = match panel_theme() {
        Some((_, path)) => path,
        None => THEMES[index.clamp(0, THEMES.len() as i64 - 1) as usize].1,
    };
    if invert { base.replace(".yaml", "-invert.yaml") } else { base.to_string() }
}
const BRIGHTNESS_MIN: u32 = 5;
/// Settings > About: who made the parts Harbour is built from.
const CREDITS: [(&str, &str); 5] = [
    ("Harbour Shell", "kurogedelic"),
    ("Fonts", "Shinonome 12 (PD), ChiKareGo2 (Giles Booth, CC BY), M+ F12"),
    ("Map", "Natural Earth"),
    ("Design", "1-bit, dithered, 4 px grid"),
    ("Made for", "M5Stack Cardputer Zero"),
];
const PANEL_ROW_H: i32 = 20;
const SLIDE_MS: u64 = 200;
const SB_W: i32 = 80;
/// Ten sections of 14 px fill 140 of the 144 px content (2 px gap above).
const SIDE_ROW_H: i32 = 14;
const SECTIONS: [&str; 10] = ["Display", "Theme", "Lock Screen", "Sound", "Network", "Bluetooth", "Devices", "System", "Apps", "Agent"];
const SECTION_PAGES: [Page; 10] = [
    Page::Display,
    Page::Theme,
    Page::Lock,
    Page::Audio,
    Page::Network,
    Page::Bluetooth,
    Page::ExtPort,
    Page::System,
    Page::Apps,
    Page::Agent,
];
/// What the Connect page's QR code opens (the repository is private for now: the link only works for signed-in collaborators).
pub const AGENT_DOC_URL: &str = "https://github.com/kurogedelic/HarbourShell/blob/main/docs/AGENT.md";

#[derive(Clone, Debug, PartialEq)]
enum Page {
    Root,
    Display,
    Theme,
    Lock,
    Audio,
    Network,
    WifiList,
    WifiPassword(String),
    Bluetooth,
    BtList,
    ExtPort,
    System,
    Apps,
    /// One app from the Apps list: show on Home, what it is, uninstall.
    AppInfo(usize),
    About,
    Agent,
    AgentConnect,
    Confirm(Confirm),
}

/// Pages the system menu opens directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SystemPage {
    About,
    Restart,
    PowerOff,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Confirm {
    Reboot,
    Poweroff,
    Official,
    /// Move app `i`'s `.desktop` file (the user's own folder) to the Trash.
    RemoveApp(usize),
}

/// What the main loop must do after a key press.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Close,
    /// App visibility or order changed: rebuild the home screen.
    AppsChanged,
    /// Run an external executable (e.g. the App Store).
    Launch(String),
    /// Harbour should exit (the official launcher is taking over).
    Exit,
    /// Display settings changed: re-apply idle timeout.
    ScreenChanged,
    /// The colour theme changed: reload it.
    ThemeChanged,
    /// The Agent was switched on / off or its provider changed: the host drops its open chat.
    AgentChanged,
    /// Install something (the Japanese dictionary): the host shows its install card.
    Install(crate::launcher::installer::InstallReq),
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    #[allow(dead_code)]
    None,
    Text(String),
    Toggle(bool),
    Slider(u32),
    Arrow,
    Check(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    val: Val,
}

fn row(label: &str, val: Val) -> Row {
    Row { label: label.to_string(), val }
}

enum Done {
    Status(WifiStatus, Option<String>),
    Aps(R<Vec<Ap>>),
    Connect(R<()>, String),
    Bt(R<Vec<BtDevice>>),
    BtOp(R<()>, String),
    Count(R<usize>),
    Power(R<()>),
    Official(R<()>),
}

struct Pending {
    rx: Receiver<Done>,
    label: &'static str,
}

#[derive(Clone, Debug)]
pub struct AppRef {
    pub id: String,
    pub name: String,
    pub exec: String,
    /// The `.desktop` file of an installed app (None for the built-in ones).
    pub desktop: Option<PathBuf>,
    /// From the user's own app folder (`registry::user_app_dir`): removed without root.
    pub user: bool,
}

/// `dpkg-query -S <file>` output -> the package that owns the file (`pkg:arch: /path` -> `pkg`).
pub fn parse_dpkg_owner(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.starts_with("diversion"))?;
    let (pkgs, _) = line.split_once(": ")?;
    let first = pkgs.split(',').next()?.trim();
    let name = first.split(':').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The package an installed app came from (None when no package owns its file).
fn owning_package(desktop: &std::path::Path) -> Option<String> {
    if matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on")) {
        return desktop.file_stem().map(|s| s.to_string_lossy().to_lowercase());
    }
    let out = std::process::Command::new("dpkg-query").arg("-S").arg(desktop).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| parse_dpkg_owner(&String::from_utf8_lossy(&out.stdout))).flatten()
}

pub struct Settings {
    sys: Arc<dyn Backend>,
    cfg: Arc<Mutex<Config>>,
    order_path: PathBuf,
    apps: Vec<AppRef>,
    /// The package of the app whose page is open (Apps > app).
    app_pkg: Option<String>,
    page: Page,
    stack: Vec<(Page, usize, usize)>,
    cursor: usize,
    scroll: usize,
    pending: Option<Pending>,
    note: Option<(String, Instant)>,
    cfg_dirty: Option<Instant>,
    // Cached values (refreshed on page entry / after changes).
    wifi: WifiStatus,
    wifi_loaded: bool,
    slide: Option<Slide>,
    /// Open pop-up: (row, highlighted item).
    popup: Option<(usize, usize)>,
    hits: Vec<(Rect, Hit)>,
    drag: Option<(usize, Rect)>,
    /// Dragging the scroll bar thumb: (track, total rows, visible rows).
    sb_drag: Option<(Rect, usize, usize)>,
    drag_x0: Option<i32>,
    flash: Option<Instant>,
    eth_ip: Option<String>,
    aps: Vec<Ap>,
    bt: Vec<BtDevice>,
    updates: Option<usize>,
    password: String,
    show_password: bool,
    agent_dir: PathBuf,
    agent_cfg_path: PathBuf,
    agent: AgentView,
}

/// The Agent page's cached view: names and one status line, never a key or a URL.
#[derive(Clone, Debug, Default)]
struct AgentView {
    enabled: bool,
    names: Vec<String>,
    provider: String,
    model: String,
    status: String,
}

impl Settings {
    pub fn new(sys: Arc<dyn Backend>, cfg: Arc<Mutex<Config>>, order_path: PathBuf, apps: Vec<AppRef>) -> Self {
        Self {
            sys,
            cfg,
            order_path,
            apps,
            app_pkg: None,
            page: Page::Root,
            stack: Vec::new(),
            cursor: 0,
            scroll: 0,
            pending: None,
            note: None,
            cfg_dirty: None,
            wifi: WifiStatus::default(),
            wifi_loaded: false,
            slide: None,
            popup: None,
            hits: Vec::new(),
            sb_drag: None,
            drag: None,
            drag_x0: None,
            flash: None,
            eth_ip: None,
            aps: Vec::new(),
            bt: Vec::new(),
            updates: None,
            password: String::new(),
            show_password: false,
            agent_dir: agent_config::data_dir(),
            agent_cfg_path: agent_config::config_path(),
            agent: AgentView::default(),
        }
        .with_agent_loaded()
    }

    fn with_agent_loaded(mut self) -> Self {
        self.refresh_agent();
        self
    }

    /// Jump straight to Agent (the Agent panel's "Open Settings" button).
    pub fn open_agent(&mut self) {
        self.stack.clear();
        self.page = Page::Root;
        self.cursor = SECTION_PAGES.iter().position(|p| *p == Page::Agent).unwrap_or(0);
        self.activate();
    }

    /// Jump straight to a section (the menu-bar status menus): "network", "sound", "display",
    /// "system". Unknown names leave the section list.
    pub fn open_section(&mut self, name: &str) {
        let page = match name {
            "network" => Page::Network,
            "sound" => Page::Audio,
            "display" => Page::Display,
            "system" => Page::System,
            _ => return,
        };
        self.stack.clear();
        self.page = Page::Root;
        self.scroll = 0;
        self.popup = None;
        self.cursor = SECTION_PAGES.iter().position(|p| *p == page).unwrap_or(0);
        self.activate();
    }

    /// Jump straight to a page below a section (the system menu: About, Restart, Power Off); Back
    /// returns through the section's page to the section list.
    pub fn open_system_page(&mut self, target: SystemPage) {
        self.stack.clear();
        self.page = Page::Root;
        self.cursor = 0;
        self.scroll = 0;
        self.popup = None;
        let path = match target {
            SystemPage::About => [Page::System, Page::About],
            SystemPage::Restart => [Page::System, Page::Confirm(Confirm::Reboot)],
            SystemPage::PowerOff => [Page::System, Page::Confirm(Confirm::Poweroff)],
        };
        // The section list keeps the cursor on the section being opened (the sidebar highlights it).
        if let Some(i) = SECTION_PAGES.iter().position(|p| *p == path[0]) {
            self.cursor = i;
        }
        for p in path {
            self.push(p);
        }
    }

    /// Where the Agent keeps its state and its config (tests point these at a temp folder).
    pub fn set_agent_paths(&mut self, dir: PathBuf, cfg: PathBuf) {
        self.agent_dir = dir;
        self.agent_cfg_path = cfg;
        self.refresh_agent();
    }

    /// Re-read the Agent's state file and config for the Agent page (no secrets: names only).
    fn refresh_agent(&mut self) {
        let st = agent_history::load_state(&self.agent_dir);
        let loaded = agent_config::load(&self.agent_cfg_path, false);
        let names = loaded.config.names();
        let provider = st.provider.clone().filter(|p| names.contains(p)).or_else(|| loaded.config.default_name()).unwrap_or_default();
        let model = loaded.config.providers.get(&provider).and_then(|p| p.model.clone()).unwrap_or_default();
        let status = loaded
            .warning
            .map(|w| format!("! {}", first_line(&w)))
            .or(st.last_status.clone())
            .unwrap_or_else(|| if provider == "mock" { "Mock (no network)".into() } else { "No calls yet".into() });
        self.agent = AgentView { enabled: st.enabled, names, provider, model, status };
    }

    fn save_agent_state(&mut self, f: impl FnOnce(&mut agent_history::State)) {
        let mut st = agent_history::load_state(&self.agent_dir);
        f(&mut st);
        agent_history::save_state(&self.agent_dir, &st);
        self.refresh_agent();
    }

    pub fn set_apps(&mut self, apps: Vec<AppRef>) {
        self.apps = apps;
        // An uninstalled app leaves its page: back to the list.
        if let Page::AppInfo(i) = self.page {
            if i >= self.apps.len() || self.app_pkg.is_some() && self.apps[i].desktop.as_deref().and_then(owning_package) != self.app_pkg {
                self.page = Page::Apps;
                self.cursor = i.min(self.apps.len().saturating_sub(1));
            }
        }
    }

    /// Show / hide app `i` on Home (the official launcher's config key).
    fn toggle_app(&mut self, i: usize) -> Event {
        let Some(a) = self.apps.get(i).cloned() else { return Event::None };
        let key = desktop_config_key(&a.id);
        let v = if self.visible(&a.id) { 0 } else { 1 };
        self.cfg_set(&key, v);
        self.flush_cfg();
        Event::AppsChanged
    }

    fn note(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    // ---- config helpers -------------------------------------------------

    fn cfg_int(&self, key: &str, default: i64) -> i64 {
        self.cfg.lock().map(|c| c.get_int(key, default)).unwrap_or(default)
    }

    fn cfg_set(&mut self, key: &str, v: i64) {
        if let Ok(mut c) = self.cfg.lock() {
            c.set_int(key, v);
        }
        self.cfg_dirty = Some(Instant::now());
    }

    /// Switch the display language now and persist it.
    fn set_lang(&mut self, i: i64) {
        self.cfg_set("harbour_lang", i);
        self.flush_cfg();
        i18n::set(i18n::Lang::from_index(i));
    }

    fn flush_cfg(&mut self) {
        if self.cfg_dirty.take().is_some() {
            let err = match self.cfg.lock() {
                Ok(c) => c.save().err(),
                Err(_) => None,
            };
            if let Some(e) = err {
                self.note(format!("save failed: {e}"));
            }
        }
    }

    fn visible(&self, id: &str) -> bool {
        self.cfg_int(&desktop_config_key(id), 1) != 0
    }

    // ---- async ----------------------------------------------------------

    fn spawn(&mut self, label: &'static str, f: impl FnOnce(Arc<dyn Backend>) -> Done + Send + 'static) {
        let (tx, rx) = channel();
        let sys = self.sys.clone();
        crate::rt::spawn(move || {
            let _ = tx.send(f(sys));
        });
        self.pending = Some(Pending { rx, label });
    }

    /// Fire-and-forget hardware write (volume etc.) that must not block the UI.
    fn background(&self, f: impl FnOnce(Arc<dyn Backend>) + Send + 'static) {
        let sys = self.sys.clone();
        crate::rt::spawn(move || f(sys));
    }

    /// Poll workers and flush debounced config writes. Call once per frame.
    pub fn tick(&mut self) {
        if let Some((_, t)) = &self.note {
            if t.elapsed() > Duration::from_secs(4) {
                self.note = None;
            }
        }
        if self.cfg_dirty.map(|t| t.elapsed() > Duration::from_millis(600)).unwrap_or(false) {
            self.flush_cfg();
        }
        let Some(p) = &self.pending else { return };
        let Ok(done) = p.rx.try_recv() else { return };
        self.pending = None;
        match done {
            Done::Status(w, eth) => {
                self.wifi = w;
                self.wifi_loaded = true;
                self.eth_ip = eth;
            }
            Done::Aps(Ok(aps)) => {
                self.aps = aps;
                self.cursor = 0;
                self.scroll = 0;
                if self.aps.is_empty() {
                    self.note("No Wi-Fi networks found");
                }
            }
            Done::Aps(Err(e)) => self.note(format!("Scan failed: {e}")),
            Done::Connect(Ok(()), ssid) => {
                self.note(format!("Connected to {ssid}"));
                // Return to the Network page and refresh its status.
                self.page = Page::Network;
                self.stack.clear();
                self.stack.push((Page::Root, 2, 0));
                self.cursor = 0;
                self.scroll = 0;
                self.enter_page();
            }
            Done::Connect(Err(e), ssid) => {
                let msg = first_line(&e);
                self.note(format!("{ssid}: {msg}"));
                // Wrong password: stay on the password page.
            }
            Done::Bt(Ok(devs)) => {
                self.bt = devs;
                self.cursor = 0;
                self.scroll = 0;
                if self.bt.is_empty() {
                    self.note("No devices found");
                }
            }
            Done::Bt(Err(e)) => self.note(format!("Scan failed: {}", first_line(&e))),
            Done::BtOp(r, what) => match r {
                Ok(()) => {
                    self.note(what);
                    self.refresh_bt_list();
                }
                Err(e) => self.note(first_line(&e)),
            },
            Done::Count(Ok(n)) => self.updates = Some(n),
            Done::Count(Err(e)) => self.note(format!("Update check failed: {}", first_line(&e))),
            Done::Power(Err(e)) => self.note(e),
            Done::Power(Ok(())) => self.note("Working..."),
            Done::Official(Ok(())) => {
                // Systemd will stop Harbour; nothing else to do.
                self.note("Switching...");
            }
            Done::Official(Err(e)) => self.note(first_line(&e)),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// True when the Esc key should be consumed by a text field first.
    pub fn flush(&mut self) {
        self.flush_cfg();
    }

    // ---- navigation -------------------------------------------------------

    fn push(&mut self, page: Page) {
        self.popup = None;
        self.start_slide(1);
        self.stack.push((self.page.clone(), self.cursor, self.scroll));
        self.page = page;
        self.cursor = 0;
        self.scroll = 0;
        self.enter_page();
    }

    fn pop(&mut self) -> bool {
        self.popup = None;
        self.flush_cfg();
        if !self.stack.is_empty() {
            self.start_slide(-1);
        }
        if let Some((p, c, s)) = self.stack.pop() {
            self.page = p;
            self.cursor = c;
            self.scroll = s;
            self.pending = None;
            self.enter_page();
            true
        } else {
            false
        }
    }

    /// Headings (`Val::None` rows) on the System page are not selectable: step over them in `dir`.
    fn settle_cursor(&mut self, dir: i32) {
        if self.page != Page::System {
            return;
        }
        let rows = self.rows();
        let is_head = |i: usize| rows.get(i).map_or(false, |r| r.val == Val::None);
        let mut c = self.cursor as i32;
        while is_head(c as usize) && (0..rows.len() as i32).contains(&(c + dir)) {
            c += dir;
        }
        if is_head(c as usize) {
            // At the very end: go back the other way.
            while is_head(c as usize) && c - dir >= 0 && c - dir < rows.len() as i32 {
                c -= dir;
            }
        }
        self.cursor = c as usize;
    }

    fn enter_page(&mut self) {
        self.settle_cursor(1);
        match &self.page {
            Page::Network => {
                self.spawn("Checking network", |sys| Done::Status(sys.wifi_status(), sys.eth_ip()));
            }
            Page::WifiList => {
                self.aps.clear();
                self.spawn("Scanning", |sys| Done::Aps(sys.wifi_scan()));
            }
            Page::BtList => {
                self.bt.clear();
                self.spawn("Scanning", |sys| Done::Bt(sys.bt_scan()));
            }
            Page::About | Page::AgentConnect => {
                self.spawn("Checking network", |sys| Done::Status(sys.wifi_status(), sys.eth_ip()));
            }
            Page::Agent => self.refresh_agent(),
            Page::WifiPassword(_) => {
                self.password.clear();
                self.show_password = false;
            }
            _ => {}
        }
    }

    fn refresh_bt_list(&mut self) {
        if self.page == Page::BtList {
            self.spawn("Updating", |sys| Done::Bt(sys.bt_scan()));
        }
    }

    // ---- rows ---------------------------------------------------------------

    pub fn title(&self) -> String {
        match &self.page {
            Page::Root => "Settings".into(),
            Page::Display => "Settings: Display".into(),
            Page::Theme => "Settings: Theme".into(),
            Page::Lock => "Settings: Lock Screen".into(),
            Page::Audio => "Settings: Sound".into(),
            Page::Network => "Settings: Network".into(),
            Page::WifiList => "Wi-Fi Networks".into(),
            Page::WifiPassword(_) => "Wi-Fi Password".into(),
            Page::Bluetooth => "Settings: Bluetooth".into(),
            Page::BtList => "Bluetooth Devices".into(),
            Page::ExtPort => "Settings: Devices".into(),
            Page::System => "Settings: System".into(),
            Page::Apps => "Settings: Apps".into(),
            Page::AppInfo(i) => self.apps.get(*i).map_or_else(|| "Settings: Apps".into(), |a| a.name.clone()),
            Page::About => "About Harbour".into(),
            Page::Agent => "Settings: Agent".into(),
            Page::AgentConnect => "Agent: Connect".into(),
            Page::Confirm(Confirm::Reboot) => "Restart".into(),
            Page::Confirm(Confirm::Poweroff) => "Power Off".into(),
            Page::Confirm(Confirm::Official) => "Official Launcher".into(),
            Page::Confirm(Confirm::RemoveApp(i)) => self.apps.get(*i).map_or_else(|| "Settings: Apps".into(), |a| a.name.clone()),
        }
    }

    fn theme_name(&self) -> &'static str {
        if let Some((name, _)) = panel_theme() {
            return name;
        }
        THEMES[self.cfg_int("harbour_theme", 0).clamp(0, THEMES.len() as i64 - 1) as usize].0
    }

    fn lock_style(&self) -> usize {
        self.cfg_int("harbour_lock_style", 0).clamp(0, LOCK_STYLES.len() as i64 - 1) as usize
    }

    fn brightness_pct(&self) -> u32 {
        self.sys.brightness().map(|(v, m)| if m == 0 { 0 } else { v * 100 / m }).unwrap_or(0)
    }

    pub fn rows(&self) -> Vec<Row> {
        self.rows_of(&self.page)
    }

    /// Page title and rows as compact text for the Agent's `current_screen` (`>` = cursor row).
    pub fn describe(&self) -> String {
        let mut out = format!("{}\n", self.title());
        for (i, r) in self.rows().iter().enumerate() {
            let v = match &r.val {
                Val::None => String::new(),
                Val::Text(t) => t.clone(),
                Val::Toggle(b) | Val::Check(b) => if *b { "on" } else { "off" }.to_string(),
                Val::Slider(n) => format!("{n}%"),
                Val::Arrow => ">".to_string(),
            };
            out.push_str(&format!("{} {}: {}\n", if i == self.cursor { ">" } else { " " }, r.label, v));
        }
        out.trim_end().to_string()
    }

    fn rows_of(&self, page: &Page) -> Vec<Row> {
        let on_off = |b: Option<bool>| match b {
            Some(v) => Val::Toggle(v),
            None => Val::Text("N/A".into()),
        };
        match page {
            Page::Root => {
                let vol = self.sys.volume();
                vec![
                    row("Display", Val::Text(format!("{}%", self.brightness_pct()))),
                    row("Theme", Val::Text(self.theme_name().to_string())),
                    row("Lock Screen", Val::Text(LOCK_STYLES[self.lock_style()].to_string())),
                    row("Sound", Val::Text(match vol {
                        Some((_, true)) => "Muted".into(),
                        Some((p, false)) => format!("{p}%"),
                        None => "N/A".into(),
                    })),
                    row("Network", Val::Arrow),
                    row("Bluetooth", Val::Arrow),
                    row("Devices", Val::Arrow),
                    row("System", Val::Arrow),
                    row("Apps", Val::Arrow),
                    row("Agent", Val::Arrow),
                ]
            }
            Page::Display => {
                let dark = self.cfg_int("dark_time", 30);
                vec![
                    row("Language", Val::Text(i18n::LANG_NAMES[self.cfg_int("harbour_lang", 0).clamp(0, 1) as usize].to_string())),
                    row("Brightness", Val::Slider(self.brightness_pct())),
                    row("Screen off", Val::Text(dark_label(dark))),
                    row("Japanese dictionary", Val::Text(if ime_dict_installed() { "Installed" } else { "Not installed" }.into())),
                ]
            }
            Page::Theme => vec![
                row("Theme", Val::Text(self.theme_name().to_string())),
                row("Invert", Val::Toggle(self.cfg_int("harbour_theme_invert", 0) == 1)),
                row("Status bar", Val::Text(BAR_STYLES[self.cfg_int("harbour_statusbar", 0).clamp(0, 2) as usize].to_string())),
            ],
            Page::Lock => vec![
                row("Style", Val::Text(LOCK_STYLES[self.lock_style()].to_string())),
                row("Auto lock", Val::Text(dark_label(self.cfg_int("harbour_lock_auto", 0)))),
                row("Lock screen off", Val::Text(lock_label(self.cfg_int("harbour_lock_dark", 8)))),
                row("Show date", Val::Toggle(self.cfg_int("harbour_lock_date", 1) == 1)),
                row("Show battery", Val::Toggle(self.cfg_int("harbour_lock_battery", 1) == 1)),
            ],
            Page::Audio => {
                let (p, m) = self.sys.volume().unwrap_or((0, false));
                vec![
                    row("Volume", Val::Slider(p as u32)),
                    row("Mute", Val::Toggle(m)),
                    row("System sounds", Val::Toggle(self.cfg_int("harbour_ui_sound", 1) == 1)),
                    row("Sounds volume", Val::Text(UI_SOUND_LEVELS[self.cfg_int("harbour_ui_sound_level", 1).clamp(0, 2) as usize].to_string())),
                ]
            }
            Page::Network => vec![
                row("Wi-Fi", Val::Toggle(self.wifi.radio)),
                row("Network", Val::Text(self.wifi.ssid.clone().unwrap_or_else(|| "Not connected".into()))),
                row("IP address", Val::Text(self.wifi.ip.clone().unwrap_or_else(|| "-".into()))),
                row("Signal", Val::Text(self.wifi.signal.map(|s| format!("{s}%")).unwrap_or_else(|| "-".into()))),
                row("Ethernet", Val::Text(self.eth_ip.clone().unwrap_or_else(|| "Not connected".into()))),
                row("Scan Wi-Fi", Val::Arrow),
            ],
            Page::WifiList => self
                .aps
                .iter()
                .map(|a| {
                    let state = if a.in_use {
                        "Connected".to_string()
                    } else if a.saved {
                        "Saved".to_string()
                    } else if a.secure {
                        format!("Locked {}%", a.signal)
                    } else {
                        format!("Open {}%", a.signal)
                    };
                    row(&a.ssid, Val::Text(state))
                })
                .collect(),
            Page::WifiPassword(_) => vec![],
            Page::Bluetooth => vec![
                row("Bluetooth", Val::Toggle(self.sys.bt_powered())),
                row("Scan devices", Val::Arrow),
            ],
            Page::BtList => self
                .bt
                .iter()
                .map(|d| {
                    let s = if d.connected {
                        "Connected"
                    } else if d.paired {
                        "Paired"
                    } else {
                        ""
                    };
                    row(&d.name, Val::Text(s.to_string()))
                })
                .collect(),
            Page::ExtPort => vec![
                row("Grove 5V out", on_off(self.sys.ext_port(ExtPort::Grove5V))),
                row("Cap 5V out", on_off(self.sys.ext_port(ExtPort::Ext5V))),
            ],
            Page::System => {
                let b = self.sys.battery();
                let batt = match b.percent {
                    Some(p) => format!("{p}% {}", b.status),
                    None => "N/A".into(),
                };
                vec![
                    row("Info", Val::None),
                    row("Battery", Val::Text(batt)),
                    row("Voltage", Val::Text(b.voltage_mv.map(|v| format!("{:.2} V", v as f32 / 1000.0)).unwrap_or_else(|| "-".into()))),
                    row("Time", Val::Text(self.sys.clock_text())),
                    row("Time zone", Val::Text(self.sys.timezone())),
                    row("Clock sync", Val::Text(match self.sys.ntp_synced() {
                        Some(true) => "Yes".into(),
                        Some(false) => "No".into(),
                        None => "-".into(),
                    })),
                    row("Software", Val::None),
                    row("Check updates", match (self.updates, self.pending.is_some()) {
                        (Some(0), _) => Val::Text("Up to date".into()),
                        (Some(n), _) => Val::Text(format!("{n} available")),
                        (None, _) => Val::Arrow,
                    }),
                    row("App Store", Val::Arrow),
                    row("Official launcher", Val::Arrow),
                    row("About", Val::Arrow),
                    row("Shutdown", Val::None),
                    row("Restart", Val::Arrow),
                    row("Power off", Val::Arrow),
                ]
            }
            Page::Apps => self
                .apps
                .iter()
                .map(|a| row(&a.name, Val::Check(self.visible(&a.id))))
                .collect(),
            Page::AppInfo(i) => match self.apps.get(*i) {
                Some(a) => {
                    let mut v = vec![
                        row("Show on Home", Val::Toggle(self.visible(&a.id))),
                        row("Kind", Val::Text(if a.user { "Your app" } else if a.desktop.is_some() { "Installed" } else { "Built-in" }.into())),
                        row("Package", Val::Text(self.app_pkg.clone().unwrap_or_else(|| "-".into()))),
                    ];
                    if a.user || a.desktop.is_some() && self.app_pkg.is_some() {
                        v.push(row("Uninstall...", Val::Arrow));
                    }
                    v
                }
                None => Vec::new(),
            },
            Page::About => {
                let s = self.sys.sysinfo();
                let b = self.sys.battery();
                let used = s.mem_total_kb.saturating_sub(s.mem_avail_kb) / 1024;
                let dash = |t: String| Val::Text(if t.is_empty() { "-".into() } else { t });
                let mut v = vec![
                    row("Device", Val::None),
                    row("Harbour", Val::Text(env!("CARGO_PKG_VERSION").to_string())),
                    row("OS", dash(s.os)),
                    row("Model", dash(s.model)),
                    row("CPU", dash(s.cpu)),
                    row("Host", dash(s.hostname)),
                    row("Kernel", dash(s.kernel)),
                    row("Uptime", Val::Text(uptime_label(s.uptime_s))),
                    row("Memory", Val::Text(format!("{} / {} MB", used, s.mem_total_kb / 1024))),
                    row("Disk", Val::Text(format!("{:.1} / {:.1} GB", s.disk_free_mb as f32 / 1024.0, s.disk_total_mb as f32 / 1024.0))),
                    row("Temperature", Val::Text(s.temp_c.map(|t| format!("{t:.1} C")).unwrap_or_else(|| "-".into()))),
                    row("Load", Val::Text(format!("{:.2}", s.load1))),
                    row("Battery", Val::Text(b.percent.map(|p| format!("{p}% {}", b.status)).unwrap_or_else(|| "-".into()))),
                    row("Wi-Fi IP", Val::Text(self.wifi.ip.clone().unwrap_or_else(|| "-".into()))),
                    row("Ethernet IP", Val::Text(self.eth_ip.clone().unwrap_or_else(|| "-".into()))),
                    row("Credits", Val::None),
                ];
                v.extend(CREDITS.iter().map(|(k, t)| row(k, Val::Text(t.to_string()))));
                v
            }
            Page::Agent => vec![
                row("Enabled", Val::Toggle(self.agent.enabled)),
                row("Provider", Val::Text(if self.agent.provider.is_empty() { "-".into() } else { self.agent.provider.clone() })),
                row("Model", Val::Text(if self.agent.model.is_empty() { "-".into() } else { self.agent.model.clone() })),
                row("Status", Val::Text(self.agent.status.clone())),
                row("Connect", Val::Arrow),
            ],
            Page::AgentConnect => vec![
                row("Wi-Fi IP", Val::Text(self.wifi.ip.clone().unwrap_or_else(|| "-".into()))),
                row("Ethernet IP", Val::Text(self.eth_ip.clone().unwrap_or_else(|| "-".into()))),
                row("Setup guide", Val::Text("docs/AGENT.md (QR code)".into())),
            ],
            Page::Confirm(_) => vec![],
        }
    }

    // ---- input ----------------------------------------------------------------

    pub fn handle_text(&mut self, text: &str) {
        if matches!(self.page, Page::WifiPassword(_)) && self.pending.is_none() {
            for ch in text.chars() {
                if !ch.is_control() && self.password.len() < 63 {
                    self.password.push(ch);
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: Keycode, mods: Mod) -> Event {
        // While a worker runs, only Esc (stop waiting / go back) is accepted.
        if self.pending.is_some() && key != Keycode::Escape {
            return Event::None;
        }
        if let Page::WifiPassword(ssid) = self.page.clone() {
            return self.password_key(key, &ssid);
        }
        if let Some((row, hi)) = self.popup {
            return self.popup_key(crate::platform::keys::arrow_alias(key), row, hi);
        }
        // Every other settings page takes no text, so F/X/Z/C work as arrows.
        let key = crate::platform::keys::arrow_alias(key);
        if let Page::Confirm(kind) = self.page {
            return self.confirm_key(key, kind);
        }
        let n = self.rows().len();
        match key {
            Keycode::Escape => {
                if !self.pop() {
                    self.flush_cfg();
                    return Event::Close;
                }
            }
            Keycode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.settle_cursor(-1);
            }
            Keycode::Down => {
                if self.cursor + 1 < n {
                    self.cursor += 1;
                }
                self.settle_cursor(1);
            }
            Keycode::Home => {
                self.cursor = 0;
                self.settle_cursor(1);
            }
            Keycode::End => {
                self.cursor = n.saturating_sub(1);
                self.settle_cursor(-1);
            }
            Keycode::PageUp => {
                self.cursor = self.cursor.saturating_sub(8);
                self.settle_cursor(1);
            }
            Keycode::PageDown => {
                self.cursor = (self.cursor + 8).min(n.saturating_sub(1));
                self.settle_cursor(-1);
            }
            // Left changes a slider / pop-up value; everywhere else it goes back.
            Keycode::Left if self.left_adjusts() => return self.adjust(-1),
            Keycode::Left => {
                if self.page != Page::Root {
                    self.pop();
                }
            }
            Keycode::Right if self.page == Page::Root => return self.activate(),
            Keycode::Right => return self.adjust(1),
            // Space ticks an app's box in the Apps list; Return opens its page.
            Keycode::Space if self.page == Page::Apps => return self.toggle_app(self.cursor),
            Keycode::Return | Keycode::KpEnter | Keycode::Space => return self.activate(),
            Keycode::LeftBracket if self.page == Page::Apps => return self.move_app(-1),
            Keycode::RightBracket if self.page == Page::Apps => return self.move_app(1),
            _ => {
                let _ = mods;
            }
        }
        Event::None
    }

    /// Items and current index of a pop-up row on the current page.
    fn choice_items(&self, row: usize) -> Option<(Vec<String>, usize)> {
        let spec = choice_spec(&self.page, row)?;
        Some((spec.labels.clone(), self.choice_index(&spec)))
    }

    /// Index of the stored value among a pop-up's values (the default's index when it is none of them).
    fn choice_index(&self, spec: &ChoiceSpec) -> usize {
        let cur = self.cfg_int(spec.key, spec.default);
        spec.values.iter().position(|v| *v == cur).or_else(|| spec.values.iter().position(|v| *v == spec.default)).unwrap_or(0)
    }

    /// Store a pop-up value and tell the host what changed.
    fn set_choice(&mut self, key: &'static str, v: i64) -> Event {
        match key {
            "harbour_lang" => {
                self.set_lang(v);
                Event::ThemeChanged
            }
            "dark_time" => {
                self.cfg_set(key, v);
                Event::ScreenChanged
            }
            "harbour_theme" | "harbour_statusbar" => {
                self.cfg_set(key, v);
                self.flush_cfg();
                Event::ThemeChanged
            }
            _ => {
                self.cfg_set(key, v);
                Event::None
            }
        }
    }

    fn open_popup(&mut self) {
        if let Some((_, cur)) = self.choice_items(self.cursor) {
            self.popup = Some((self.cursor, cur));
        }
    }

    /// Apply item `item` of the pop-up on `row`.
    fn pick_choice(&mut self, row: usize, item: usize) -> Event {
        let Some(spec) = choice_spec(&self.page, row) else { return Event::None };
        if item >= spec.values.len() || item == self.choice_index(&spec) {
            return Event::None;
        }
        self.set_choice(spec.key, spec.values[item])
    }

    fn popup_key(&mut self, key: Keycode, row: usize, hi: usize) -> Event {
        let n = self.choice_items(row).map(|c| c.0.len()).unwrap_or(0);
        match key {
            Keycode::Escape => self.popup = None,
            Keycode::Up => self.popup = Some((row, hi.saturating_sub(1))),
            Keycode::Down => self.popup = Some((row, (hi + 1).min(n.saturating_sub(1)))),
            Keycode::Return | Keycode::KpEnter | Keycode::Space => {
                self.popup = None;
                return self.pick_choice(row, hi);
            }
            _ => {}
        }
        Event::None
    }

    /// Left changes the value of a slider or a pop-up row; elsewhere it goes back.
    fn left_adjusts(&self) -> bool {
        choice_spec(&self.page, self.cursor).is_some() || matches!((&self.page, self.cursor), (Page::Display, 1) | (Page::Audio, 0))
    }

    /// True where Tab is this screen's own key (the Wi-Fi password page: show / hide), so the host's
    /// double-Tab Agent gesture stays out of the way.
    pub fn wants_tab(&self) -> bool {
        matches!(self.page, Page::WifiPassword(_))
    }

    fn password_key(&mut self, key: Keycode, ssid: &str) -> Event {
        match key {
            Keycode::Escape => {
                self.pop();
            }
            Keycode::Backspace => {
                self.password.pop();
            }
            Keycode::Tab => self.show_password = !self.show_password,
            Keycode::Return | Keycode::KpEnter => {
                if self.password.len() < 8 {
                    self.note("Password needs 8+ characters");
                } else {
                    let (s, pw) = (ssid.to_string(), self.password.clone());
                    self.spawn("Connecting", move |sys| {
                        let r = sys.wifi_connect(&s, Some(&pw));
                        Done::Connect(r, s)
                    });
                }
            }
            _ => {}
        }
        Event::None
    }

    fn confirm_key(&mut self, key: Keycode, kind: Confirm) -> Event {
        match key {
            Keycode::Escape | Keycode::N => {
                self.pop();
            }
            Keycode::Return | Keycode::KpEnter | Keycode::Y => match kind {
                Confirm::Reboot => self.spawn("Restarting", |sys| Done::Power(sys.reboot())),
                Confirm::Poweroff => self.spawn("Powering off", |sys| Done::Power(sys.poweroff())),
                Confirm::Official => {
                    self.spawn("Switching", |sys| Done::Official(sys.switch_to_official_launcher()));
                    return Event::Exit;
                }
                Confirm::RemoveApp(i) => {
                    let path = self.apps.get(i).and_then(|a| a.desktop.clone());
                    let res = match (path, crate::launcher::fileops::trash_dir()) {
                        (Some(p), Some(t)) => crate::launcher::fileops::trash(&p, &t).map(|_| ()),
                        _ => Err("No Trash folder".into()),
                    };
                    match res {
                        Ok(()) => {
                            self.note("Moved to the Trash");
                            // Back past the app's page to the list.
                            self.pop();
                            self.pop();
                            self.cursor = i.saturating_sub(1);
                            return Event::AppsChanged;
                        }
                        Err(e) => {
                            self.note(e);
                            self.pop();
                        }
                    }
                }
            },
            _ => {}
        }
        Event::None
    }

    fn adjust(&mut self, dir: i32) -> Event {
        self.flash = Some(Instant::now());
        let idx = self.cursor;
        match (self.page.clone(), idx) {
            (Page::Display, 1) => {
                let cur = self.brightness_pct() as i32;
                self.set_brightness_pct((cur + dir * 5).clamp(BRIGHTNESS_MIN as i32, 100) as u32);
            }
            (Page::Audio, 0) => {
                let (p, _) = self.sys.volume().unwrap_or((0, false));
                self.set_volume_pct((p as i32 + dir * 5).clamp(0, 100) as u8);
            }
            (Page::Audio, 1 | 2) | (Page::Theme, 1) | (Page::Lock, 3 | 4) | (Page::Agent, 0) => return self.activate(),
            (Page::Agent, 1) => return self.cycle_agent_provider(dir),
            (Page::Network, 0) | (Page::Bluetooth, 0) | (Page::ExtPort, _) => return self.activate(),
            (page, row) => {
                if let Some(spec) = choice_spec(&page, row) {
                    let i = self.choice_index(&spec) as i32;
                    let next = (i + dir).clamp(0, spec.values.len() as i32 - 1) as usize;
                    if next as i32 != i {
                        return self.set_choice(spec.key, spec.values[next]);
                    }
                }
            }
        }
        Event::None
    }

    fn set_brightness_pct(&mut self, next: u32) {
        let max = self.sys.brightness().map(|(_, m)| m).unwrap_or(100);
        if let Err(e) = self.sys.set_brightness((next * max / 100).max(1)) {
            self.note(e);
        } else {
            self.cfg_set("brightness", next as i64);
        }
    }

    fn set_volume_pct(&mut self, next: u8) {
        self.background(move |s| {
            let _ = s.set_volume(next);
        });
        self.set_volume_local(next);
    }

    /// Mock/real backends both apply asynchronously; keep the local view in sync
    /// for the slider by applying synchronously too (cheap sysfs/IPC path).
    fn set_volume_local(&mut self, v: u8) {
        self.cfg_set("volume", v as i64);
    }

    fn move_app(&mut self, dir: i32) -> Event {
        let i = self.cursor;
        let j = i as i32 + dir;
        if j < 0 || j as usize >= self.apps.len() {
            return Event::None;
        }
        // Harbour's own apps keep their place after the installed ones: only like moves with like.
        let builtin = |a: &AppRef| a.exec.starts_with(crate::launcher::packages::EXEC_SCHEME);
        if builtin(&self.apps[i]) || builtin(&self.apps[j as usize]) {
            return Event::None;
        }
        self.apps.swap(i, j as usize);
        self.cursor = j as usize;
        let ids: Vec<String> = self.apps.iter().filter(|a| !builtin(a)).map(|a| a.id.clone()).collect();
        if let Err(e) = registry::save_order(&self.order_path, &ids) {
            self.note(format!("save failed: {e}"));
        }
        Event::AppsChanged
    }

    fn activate(&mut self) -> Event {
        self.flash = Some(Instant::now());
        let idx = self.cursor;
        match (self.page.clone(), idx) {
            (Page::Root, i) => {
                if let Some(p) = SECTION_PAGES.get(i) {
                    self.push(p.clone());
                }
            }
            (page, row) if choice_spec(&page, row).is_some() => self.open_popup(),
            (Page::Display, 3) => {
                if !ime_dict_installed() {
                    return Event::Install(crate::launcher::installer::InstallReq::ime_dict());
                }
            }
            (Page::Theme, 1) => {
                let on = self.cfg_int("harbour_theme_invert", 0) == 1;
                self.cfg_set("harbour_theme_invert", (!on) as i64);
                self.flush_cfg();
                return Event::ThemeChanged;
            }
            (Page::Lock, row @ (3 | 4)) => {
                let key = if row == 3 { "harbour_lock_date" } else { "harbour_lock_battery" };
                let on = self.cfg_int(key, 1) == 1;
                self.cfg_set(key, (!on) as i64);
            }
            (Page::Audio, 1) => {
                let (_, m) = self.sys.volume().unwrap_or((0, false));
                let _ = self.sys.set_mute(!m);
            }
            (Page::Audio, 2) => {
                let on = self.cfg_int("harbour_ui_sound", 1) == 1;
                self.cfg_set("harbour_ui_sound", (!on) as i64);
            }
            (Page::Network, 0) => {
                let on = !self.wifi.radio;
                let _ = self.sys.wifi_set_radio(on);
                self.wifi.radio = on;
                self.enter_page();
            }
            (Page::Network, 5) => self.push(Page::WifiList),
            (Page::WifiList, i) => {
                if let Some(ap) = self.aps.get(i).cloned() {
                    if ap.in_use {
                        self.note("Already connected");
                    } else if ap.secure && !ap.saved {
                        self.push(Page::WifiPassword(ap.ssid));
                    } else {
                        let s = ap.ssid.clone();
                        self.spawn("Connecting", move |sys| {
                            let r = sys.wifi_connect(&s, None);
                            Done::Connect(r, s)
                        });
                    }
                }
            }
            (Page::Bluetooth, 0) => {
                let on = !self.sys.bt_powered();
                if let Err(e) = self.sys.bt_set_power(on) {
                    self.note(first_line(&e));
                }
            }
            (Page::Bluetooth, 1) => {
                if self.sys.bt_powered() {
                    self.push(Page::BtList);
                } else {
                    self.note("Turn Bluetooth on first");
                }
            }
            (Page::BtList, i) => {
                if let Some(d) = self.bt.get(i).cloned() {
                    let addr = d.addr.clone();
                    if d.connected {
                        self.spawn("Disconnecting", move |sys| {
                            Done::BtOp(sys.bt_disconnect(&addr), "Disconnected".into())
                        });
                    } else {
                        self.spawn("Connecting", move |sys| Done::BtOp(sys.bt_connect(&addr), "Connected".into()));
                    }
                }
            }
            (Page::ExtPort, i) => {
                let port = if i == 0 { ExtPort::Grove5V } else { ExtPort::Ext5V };
                match self.sys.ext_port(port) {
                    Some(cur) => {
                        if let Err(e) = self.sys.set_ext_port(port, !cur) {
                            self.note(first_line(&e));
                        }
                    }
                    None => self.note("Not available"),
                }
            }
            (Page::System, 12) => self.push(Page::Confirm(Confirm::Reboot)),
            (Page::System, 13) => self.push(Page::Confirm(Confirm::Poweroff)),
            (Page::Apps, i) => {
                if let Some(a) = self.apps.get(i).cloned() {
                    self.app_pkg = a.desktop.as_deref().and_then(owning_package);
                    self.push(Page::AppInfo(i));
                }
            }
            (Page::AppInfo(i), 0) => return self.toggle_app(i),
            (Page::AppInfo(i), 3) if self.apps.get(i).map_or(false, |a| a.user) => self.push(Page::Confirm(Confirm::RemoveApp(i))),
            (Page::AppInfo(i), 3) => {
                if let (Some(a), Some(pkg)) = (self.apps.get(i), self.app_pkg.clone()) {
                    let why = format!("{} and its files leave this device.", a.name);
                    return Event::Install(crate::launcher::installer::InstallReq::apt_remove(&[pkg.as_str()], &why));
                }
            }
            (Page::System, 7) => {
                self.updates = None;
                self.spawn("Checking", |sys| Done::Count(sys.upgradable_count()));
            }
            (Page::System, 8) => {
                let exec = self
                    .apps
                    .iter()
                    .find(|a| a.exec.ends_with("AppStore"))
                    .map(|a| a.exec.clone())
                    .unwrap_or_else(|| "/usr/share/APPLaunch/bin/M5CardputerZero-AppStore".to_string());
                return Event::Launch(exec);
            }
            (Page::System, 9) => self.push(Page::Confirm(Confirm::Official)),
            (Page::System, 10) => self.push(Page::About),
            (Page::Agent, 0) => {
                let on = !self.agent.enabled;
                self.save_agent_state(|st| st.enabled = on);
                return Event::AgentChanged;
            }
            (Page::Agent, 1) => return self.cycle_agent_provider(1),
            (Page::Agent, 4) => self.push(Page::AgentConnect),
            _ => {}
        }
        Event::None
    }

    /// Switch to the next / previous configured provider (wraps nowhere: stops at the ends).
    fn cycle_agent_provider(&mut self, dir: i32) -> Event {
        let names = self.agent.names.clone();
        let Some(i) = names.iter().position(|n| *n == self.agent.provider) else { return Event::None };
        let next = (i as i32 + dir).rem_euclid(names.len() as i32) as usize;
        if next == i {
            return Event::None;
        }
        let name = names[next].clone();
        self.save_agent_state(|st| {
            st.provider = Some(name);
            st.model = None;
        });
        Event::AgentChanged
    }

    // ---- drawing ------------------------------------------------------------------

    /// Everything the screen shows for one page: also used as the snapshot the
    /// slide transition keeps of the page being left.
    fn view(&self) -> View {
        let in_root = self.page == Page::Root;
        let section = if in_root { Some(self.cursor) } else { self.stack.first().map(|s| s.1) }
            .unwrap_or(0)
            .min(SECTIONS.len() - 1);
        let (rows, cursor, scroll) = if in_root {
            let shown = &SECTION_PAGES[section];
            let r = if *shown == Page::Network && !self.wifi_loaded {
                vec![row("Open to check the connection", Val::None)]
            } else {
                self.rows_of(shown)
            };
            (r, usize::MAX, 0)
        } else {
            (self.rows(), self.cursor, self.scroll)
        };
        View {
            page: self.page.clone(),
            rows,
            cursor,
            scroll,
            section,
            in_root,
            password: self.password.clone(),
            show_password: self.show_password,
            sb_drag: self.sb_drag.is_some(),
            popup: self.popup.and_then(|(row, hi)| self.choice_items(row).map(|(items, cur)| (row, items.iter().map(|i| i18n::tr_s(i)).collect(), cur, hi))),
        }
    }

    /// Pages change instantly: the slide transition was dropped (it only looked like a jerk on
    /// the SPI panel). The drawing code for it is still here, but nothing starts it.
    fn start_slide(&mut self, _dir: i32) {}

    /// True while a page transition is running (the host should keep redrawing).
    pub fn animating(&self) -> bool {
        self.slide.is_some()
    }

    fn pressed(&self) -> bool {
        self.drag.is_some() || self.flash.map(|t| t.elapsed() < Duration::from_millis(140)).unwrap_or(false)
    }

    pub fn draw(&mut self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        // Like the framed screens (docs/DESIGN.md): a 1 px frame from y 17 down to y 168, square
        // corners; the sidebar and the page fill it.
        ui::fill_rect(c, chrome::app_rect(), tm.get_color("home_bg"));
        crate::grid::draw_app_frame(c, crate::grid::app_frame().y(), tm.get_color("foreground"), Some(tm.get_color("widget_face_active")));
        let content = crate::grid::app_frame_inner();
        // Notes sit in the framed apps' content rectangle, 1 px clear of the frame.
        let inner = crate::grid::app_content();
        self.hits.clear();

        let status = match (&self.pending, &self.note) {
            (Some(p), _) => format!("{}...", i18n::tr_s(p.label)),
            (None, Some((n, _))) => i18n::tr_s(n),
            _ => String::new(),
        };

        // Keep the cursor row on screen.
        let visible = ((content.height() as i32 - 2) / PANEL_ROW_H).max(1) as usize;
        if self.page != Page::Root {
            if self.cursor < self.scroll {
                self.scroll = self.cursor;
            } else if self.cursor >= self.scroll + visible {
                self.scroll = self.cursor + 1 - visible;
            }
        }
        let cur = self.view();
        let pressed = self.pressed();

        if self.slide.as_ref().map(|s| s.start.elapsed() >= Duration::from_millis(SLIDE_MS)).unwrap_or(false) {
            self.slide = None;
        }
        let mut hits = Vec::new();
        match &self.slide {
            Some(s) => {
                let t = (s.start.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS as f32).min(1.0);
                let e = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
                let both = !cur.full_width() && !s.from.full_width();
                let region = if both { pane_rect(&content) } else { content };
                if both {
                    draw_sidebar(c, fs, tm, &content, cur.section, cur.in_root, None);
                }
                let w = region.width() as f32;
                let (ox_new, ox_old) = if s.dir > 0 {
                    ((w * (1.0 - e)) as i32, -(w * e) as i32)
                } else {
                    (-(w * (1.0 - e)) as i32, (w * e) as i32)
                };
                c.set_clip_rect(region);
                draw_view(c, fs, tm, region, both, &s.from, ox_old, false, None);
                draw_view(c, fs, tm, region, both, &cur, ox_new, pressed, None);
                c.set_clip_rect(None);
            }
            None => draw_view(c, fs, tm, content, false, &cur, 0, pressed, Some(&mut hits)),
        }
        self.hits.extend(hits);
        chrome::draw_command_bar(c, fs, tm, &inner, &status, "");
    }

    // ---- pointer (called by the host once it receives mouse input) ----------------

    /// What is under the pointer, using the rectangles of the last drawn frame.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains_point((x, y))).map(|(_, h)| h.clone())
    }

    fn back(&mut self) -> Event {
        if self.pop() {
            Event::None
        } else {
            self.flush_cfg();
            Event::Close
        }
    }

    /// Press at (x, y). `clicks` is 1 for a single click, 2 for a double click.
    pub fn click(&mut self, x: i32, y: i32, clicks: u8) -> Event {
        self.drag_x0 = Some(x);
        if let Some((row, _)) = self.popup {
            // An open list takes the click: pick an entry, or dismiss it.
            self.popup = None;
            if let Some(Hit::PopupItem(i)) = self.hit(x, y) {
                return self.pick_choice(row, i);
            }
            return Event::None;
        }
        let hit = self.hit(x, y);
        // The section list shows a preview of the selected section's controls. Using one of them
        // enters that section first (the preview has the same layout), then acts on that row.
        if self.page == Page::Root && matches!(hit, Some(Hit::Row(_) | Hit::Check(_) | Hit::Slider(_) | Hit::Choice(_))) {
            self.activate();
            if self.page == Page::Root {
                return Event::None;
            }
        }
        let n = self.rows().len();
        match hit {
            None => Event::None,
            Some(Hit::Back) => self.back(),
            Some(Hit::Sidebar(i)) => {
                if self.page == Page::Root {
                    self.cursor = i;
                    if clicks >= 2 {
                        return self.activate();
                    }
                } else if self.stack.first().map(|s| s.1) != Some(i) {
                    // Jump straight to another panel.
                    self.stack.clear();
                    self.page = Page::Root;
                    self.cursor = i;
                    self.scroll = 0;
                    self.activate();
                    self.slide = None;
                }
                Event::None
            }
            Some(Hit::Row(i)) => {
                if i >= n {
                    return Event::None;
                }
                self.cursor = i;
                let button = matches!(self.rows()[i].val, Val::Arrow);
                if clicks >= 2 || (button && Self::panel_page(&self.page)) {
                    self.activate()
                } else {
                    Event::None
                }
            }
            Some(Hit::Check(i)) => {
                if i < n {
                    self.cursor = i;
                    self.flash = Some(Instant::now());
                    return self.activate();
                }
                Event::None
            }
            Some(Hit::Slider(i)) => {
                if i < n {
                    self.cursor = i;
                    self.drag_x0 = None;
                    if let Some((r, _)) = self.hits.iter().find(|(_, h)| *h == Hit::Slider(i)) {
                        self.drag = Some((i, *r));
                    }
                    self.drag(x, y);
                }
                Event::None
            }
            Some(Hit::Choice(i)) => {
                if i < n {
                    self.cursor = i;
                    self.open_popup();
                }
                Event::None
            }
            Some(Hit::PopupItem(_)) => Event::None,
            Some(Hit::Scroll { total, visible }) => {
                self.drag_x0 = None;
                if let Some((hit, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Scroll { .. })).cloned() {
                    let sb = ScrollBar::new(sb_rect(hit), total, visible, self.scroll);
                    // The hit area is a pixel wider than the bar on both sides.
                    let x = x.clamp(sb.rect.x(), sb.rect.right() - 1);
                    if let Some((to, thumb)) = sb.press(x, y) {
                        if thumb {
                            self.sb_drag = Some((sb.rect, total, visible));
                        } else {
                            self.scroll_to(to, visible);
                        }
                    }
                }
                Event::None
            }
            Some(Hit::Ok) => self.handle_key(Keycode::Return, Mod::NOMOD),
            Some(Hit::Cancel) => self.handle_key(Keycode::Escape, Mod::NOMOD),
        }
    }

    /// Pointer moved without a button: an open pop-up highlights the item under it.
    pub fn hover(&mut self, x: i32, y: i32) {
        if let Some((row, hi)) = self.popup {
            if let Some(Hit::PopupItem(i)) = self.hit(x, y) {
                if i != hi {
                    self.popup = Some((row, i));
                }
            }
        }
    }

    /// Pointer moved with the button held: drags a slider thumb.
    pub fn drag(&mut self, x: i32, y: i32) {
        if let Some((track, total, visible)) = self.sb_drag {
            let to = ScrollBar::new(track, total, visible, self.scroll).offset_for_y(y);
            self.scroll_to(to, visible);
            return;
        }
        if let Some((row, r)) = self.drag {
            let pct = ((x - r.x() - 3) * 100 / (r.width() as i32 - 7).max(1)).clamp(0, 100) as u32;
            match (&self.page, row) {
                (Page::Display, 1) => self.set_brightness_pct(pct.max(BRIGHTNESS_MIN)),
                (Page::Audio, 0) => self.set_volume_pct(pct as u8),
                _ => {}
            }
        }
    }

    /// Button released. A left drag of 40+ px across a page goes back.
    pub fn release(&mut self, x: i32, _y: i32) -> Event {
        if self.sb_drag.take().is_some() {
            return Event::None;
        }
        if self.drag.take().is_some() {
            self.flush_cfg();
            return Event::None;
        }
        if let Some(x0) = self.drag_x0.take() {
            if x - x0 <= -40 && self.page != Page::Root {
                return self.back();
            }
        }
        Event::None
    }

    /// Scroll the list so that row `to` is the first one shown; the selection is pulled into view
    /// (otherwise drawing would scroll straight back to it).
    fn scroll_to(&mut self, to: usize, visible: usize) {
        if self.page == Page::Root {
            return;
        }
        let n = self.rows().len();
        self.scroll = to.min(n.saturating_sub(visible));
        self.cursor = self.cursor.clamp(self.scroll, (self.scroll + visible).saturating_sub(1).min(n.saturating_sub(1)));
    }

    /// Vertical wheel: moves the selection (positive = down).
    pub fn wheel(&mut self, dy: i32) -> Event {
        let key = if dy > 0 { Keycode::Down } else { Keycode::Up };
        let mut ev = Event::None;
        for _ in 0..dy.unsigned_abs().min(4) {
            ev = self.handle_key(key, Mod::NOMOD);
        }
        ev
    }

    /// Horizontal wheel: negative goes back, positive opens the selected panel.
    pub fn wheel_x(&mut self, dx: i32) -> Event {
        if dx < 0 && self.page != Page::Root {
            self.back()
        } else if dx > 0 && self.page == Page::Root {
            self.activate()
        } else {
            Event::None
        }
    }

    /// Pages drawn like a classic Control Panel (roomy rows, real controls).
    fn panel_page(page: &Page) -> bool {
        matches!(
            page,
            Page::Display | Page::Theme | Page::Lock | Page::Audio | Page::Network | Page::Bluetooth | Page::ExtPort | Page::System | Page::About | Page::Agent | Page::AgentConnect
        )
    }

}

type Col = sdl2::pixels::Color;

/// One page as drawn: a snapshot so the slide transition can keep drawing the page it left.
#[derive(Clone)]
struct View {
    page: Page,
    rows: Vec<Row>,
    /// `usize::MAX` = nothing selected (the preview shown beside the section list).
    cursor: usize,
    scroll: usize,
    section: usize,
    in_root: bool,
    password: String,
    show_password: bool,
    /// The scroll bar thumb is being dragged (drawn pressed).
    sb_drag: bool,
    /// (row, items, current item, highlighted item) of the open pop-up.
    popup: Option<(usize, Vec<String>, usize, usize)>,
}

impl View {
    fn full_width(&self) -> bool {
        matches!(self.page, Page::WifiPassword(_) | Page::Confirm(_))
    }
}

struct Slide {
    from: View,
    start: Instant,
    /// +1: going deeper (new page comes from the right); -1: going back.
    dir: i32,
}

/// What a point on screen belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// The window's close box: back one level, or close at the top.
    Back,
    Sidebar(usize),
    Row(usize),
    Check(usize),
    Slider(usize),
    /// A pop-up button: opens its list of choices.
    Choice(usize),
    /// An entry of the open pop-up list.
    PopupItem(usize),
    /// The scroll bar column (only registered when the page scrolls): total and visible rows.
    Scroll { total: usize, visible: usize },
    /// Dialog buttons (password page, confirmations).
    Ok,
    Cancel,
}

/// A pop-up row: the config key it stores, the values it offers, their labels and the default.
struct ChoiceSpec {
    key: &'static str,
    values: Vec<i64>,
    labels: Vec<String>,
    default: i64,
}

/// The pop-up rows of every page (`None`: not a pop-up row).
/// Settings > Sound > Sounds volume: the shell's own clicks (`sound::LEVELS`).
const UI_SOUND_LEVELS: [&str; 3] = ["Quiet", "Medium", "Strong"];

fn choice_spec(page: &Page, row: usize) -> Option<ChoiceSpec> {
    // On a grey or 1-bit panel the theme is the panel's own: nothing to choose.
    if *page == Page::Theme && row == 0 && panel_theme().is_some() {
        return None;
    }
    let spec = |key, values: Vec<i64>, labels: Vec<String>, default| Some(ChoiceSpec { key, values, labels, default });
    let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let index = |n: usize| (0..n as i64).collect::<Vec<_>>();
    match (page, row) {
        (Page::Display, 0) => spec("harbour_lang", index(2), names(&i18n::LANG_NAMES), 0),
        (Page::Display, 2) => spec("dark_time", DARK_TIMES.to_vec(), DARK_TIMES.iter().map(|v| dark_label(*v)).collect(), 30),
        (Page::Theme, 0) => spec("harbour_theme", index(THEMES.len()), THEMES.iter().map(|t| t.0.to_string()).collect(), 0),
        (Page::Theme, 2) => spec("harbour_statusbar", index(BAR_STYLES.len()), names(&BAR_STYLES), 0),
        (Page::Lock, 0) => spec("harbour_lock_style", index(LOCK_STYLES.len()), names(&LOCK_STYLES), 0),
        (Page::Lock, 1) => spec("harbour_lock_auto", AUTO_LOCK_TIMES.to_vec(), AUTO_LOCK_TIMES.iter().map(|v| dark_label(*v)).collect(), 0),
        (Page::Audio, 3) => spec("harbour_ui_sound_level", index(UI_SOUND_LEVELS.len()), names(&UI_SOUND_LEVELS), 1),
        (Page::Lock, 2) => spec("harbour_lock_dark", LOCK_DARK_TIMES.to_vec(), LOCK_DARK_TIMES.iter().map(|v| lock_label(*v)).collect(), 8),
        _ => None,
    }
}

/// The pop-up button of a choice row: 15 px high, so a full-height kanji ("壁掛け時計") keeps a
/// pixel clear of the frame, and the shadow below it a pixel clear of the row's rule.
fn choice_rect(area: &Rect) -> Rect {
    let right = area.x() + area.width() as i32 - 8;
    Rect::new(right - 100, area.y() + (area.height() as i32 - 15) / 2, 100, 15)
}

fn pane_rect(content: &Rect) -> Rect {
    Rect::new(content.x() + SB_W, content.y(), content.width() - SB_W as u32, content.height())
}

/// 11x11 check box with a check mark when on; inverted while pressed.
fn draw_checkbox(c: &mut Canvas<Window>, x: i32, y: i32, on: bool, pressed: bool, ink: Col, paper: Col, sunken: Col) {
    let r = Rect::new(x, y, 11, 11);
    ui::fill_rect(c, r, if pressed { ink } else { sunken });
    ui::draw_rect(c, r, ink);
    if on {
        let fg = if pressed { paper } else { ink };
        // A check mark (an X reads as "off" or "cancelled"): short stroke down, long stroke up.
        c.set_draw_color(fg);
        for (dx, dy) in [(2, 4), (3, 5), (4, 6), (5, 5), (6, 4), (7, 3), (8, 2)] {
            let _ = c.draw_point((x + dx, y + dy));
            let _ = c.draw_point((x + dx, y + dy + 1));
        }
    }
}

/// Slider with a recessed groove, tick marks and a block thumb (Monitors panel).
fn draw_panel_slider(c: &mut Canvas<Window>, tm: &ThemeManager, r: Rect, v: u32, selected: bool, pressed: bool) {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let line = tm.get_color("line");
    let ty = r.y() + 4;
    let groove = Rect::new(r.x(), ty, r.width(), 5);
    ui::fill_rect(c, groove, tm.get_color("surface_sunken"));
    ui::draw_rect(c, groove, ink);
    // Dithered fill up to the thumb.
    let travel = r.width() as i32 - 7;
    let tx = r.x() + travel * v.min(100) as i32 / 100;
    if tx > r.x() + 2 {
        dither::fill(c, Rect::new(r.x() + 1, ty + 1, (tx - r.x() - 1) as u32, 3), ink, Pattern::Eighth);
    }
    for i in 0..=4 {
        let x = r.x() + (r.width() as i32 - 1) * i / 4;
        ui::line(c, x, ty + 7, x, ty + 9, line);
    }
    let thumb = Rect::new(tx, r.y(), 7, 11);
    ui::fill_rect(c, thumb, if pressed { ink } else { paper });
    if selected && !pressed {
        dither::fill(c, Rect::new(tx + 1, r.y() + 1, 5, 9), ink, Pattern::Quarter);
    }
    ui::draw_rect(c, thumb, ink);
    ui::line(c, tx + 3, r.y() + 2, tx + 3, r.y() + 8, if pressed { paper } else { ink });
}

/// One control-panel row: label (inverted when selected) and a real control.
/// Hit rectangles of the controls are appended to `reg`.
#[allow(clippy::too_many_arguments)]
fn draw_panel_row(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, r: &Row, idx: usize, area: Rect, selected: bool, choice: bool, pressed: bool, reg: &mut Vec<(Rect, Hit)>) {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let faint = crate::launcher::widgets::secondary_ink(tm);
    let sunken = tm.get_color("surface_sunken");
    let (left, right) = (area.x() + 8, area.x() + area.width() as i32 - 8);
    let mid = area.y() + area.height() as i32 / 2;
    let ty = widgets::text_origin(area.y(), area.height() as i32);
    reg.push((area, Hit::Row(idx)));
    // The rule runs the full width of the row (up to the scroll bar column).
    ui::line(c, area.x(), area.y() + area.height() as i32 - 1, area.x() + area.width() as i32 - 1, area.y() + area.height() as i32 - 1, tm.get_color("line"));
    let label_plate = |c: &mut Canvas<Window>, x: i32, text: &str| widgets::draw_label_plate(c, fs, tm, area, x, text, selected, ink);
    let r = &Row { label: i18n::tr_s(&r.label), val: r.val.clone() };
    match &r.val {
        Val::Toggle(on) | Val::Check(on) => {
            let (bx, by) = (left, mid - 5);
            draw_checkbox(c, bx, by, *on, selected && pressed, ink, paper, sunken);
            let label = chrome::fit_text(fs, &r.label, right - left - 20);
            label_plate(c, left + 17, &label);
            reg.push((Rect::new(bx - 2, area.y(), (20 + fs.measure_text(&label)) as u32, area.height()), Hit::Check(idx)));
        }
        Val::Arrow => {
            let w = (fs.measure_text(&r.label) + 20).max(60) as u32;
            // 1 px more padding above and below the label than the old 12 px button.
            let b = Rect::new(left + 3, mid - 7, w, 14);
            // The selected button is the one Enter presses: it wears the default ring.
            let st = BtnState { pressed: selected && pressed, default: selected, ..Default::default() };
            widgets::draw_button(c, fs, tm, b, &r.label, st);
        }
        Val::Slider(p) => {
            label_plate(c, left, &r.label);
            let txt = format!("{p}%");
            let tw = fs.measure_text(&txt);
            fs.draw_text(c, right - tw, ty, &txt, ink);
            // Every slider starts and ends at the same place (Brightness, Volume): the start clears
            // the longer of the two labels, the end leaves room for "100%".
            let sx = left + (fs.measure_text(&r.label) + 12).max(76);
            let track = Rect::new(sx, mid - 5, (right - fs.measure_text("100%") - 8 - sx).max(20) as u32, 11);
            draw_panel_slider(c, tm, track, *p, selected, selected && pressed);
            reg.push((track, Hit::Slider(idx)));
        }
        Val::Text(t) if choice => {
            label_plate(c, left, &r.label);
            let b = choice_rect(&area);
            let dd = Dropdown::new(b, vec![i18n::tr_s(t)]);
            dd.draw_closed(c, fs, tm, 0, BtnState { pressed: selected && pressed, ..Default::default() });
            reg.push((b, Hit::Choice(idx)));
        }
        Val::Text(t) => {
            label_plate(c, left, &r.label);
            let dim = matches!(t.as_str(), "N/A" | "-" | "Not connected");
            let t = i18n::tr_s(t);
            let t = chrome::fit_text(fs, &t, area.width() as i32 - fs.measure_text(&r.label) - 28);
            fs.draw_text(c, right - fs.measure_text(&t), ty, &t, if dim { faint } else { ink });
        }
        // Section heading inside a long page (About).
        Val::None => {
            fs.draw_text(c, left, ty, &r.label, faint);
        }
    }
}

/// Plain list row (Apps, Wi-Fi / Bluetooth lists): inverted bar, zebra stripes.
#[allow(clippy::too_many_arguments)]
fn draw_list_row(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, r: &Row, idx: usize, rect: Rect, selected: bool, reg: &mut Vec<(Rect, Hit)>) {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let (fg, bg) = if selected { (tm.get_color("select_fg"), tm.get_color("select_bg")) } else { (ink, if idx % 2 == 1 { tm.get_color("surface_alt") } else { paper }) };
    ui::fill_rect(c, rect, bg);
    reg.push((rect, Hit::Row(idx)));
    let ty = widgets::text_origin(rect.y(), rect.height() as i32);
    let right = rect.x() + rect.width() as i32 - 6;
    let value_w = match &r.val {
        Val::Check(_) => 11,
        Val::Arrow => fs.measure_text(">"),
        Val::Text(t) => fs.measure_text(&i18n::tr_s(t)),
        _ => 0,
    };
    let label = chrome::fit_text(fs, &r.label, (rect.width() as i32 - 20 - value_w - 8).max(20));
    fs.draw_text(c, rect.x() + 6, ty, &label, fg);
    match &r.val {
        Val::Text(t) => {
            let t = i18n::tr_s(t);
            let t = chrome::fit_text(fs, &t, rect.width() as i32 - 16 - fs.measure_text(&label) - 10);
            fs.draw_text(c, right - fs.measure_text(&t), ty, &t, fg);
        }
        Val::Arrow => fs.draw_text(c, right - value_w, ty, ">", fg),
        Val::Check(on) => {
            let (bx, by) = (right - 11, rect.y() + 2);
            draw_checkbox(c, bx, by, *on, false, ink, paper, tm.get_color("surface_sunken"));
            reg.push((Rect::new(bx - 4, rect.y(), 20, rect.height()), Hit::Check(idx)));
        }
        _ => {}
    }
}

/// Cancel / default-action buttons at the bottom right of a dialog page.
fn dialog_buttons(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, base: &Rect, cancel: &'static str, ok: &'static str, reg: Option<&mut Vec<(Rect, Hit)>>) {
    let (cancel, ok) = (i18n::tr(cancel), i18n::tr(ok));
    let w_ok = (fs.measure_display_text(ok) + 24).max(64);
    let w_cancel = (fs.measure_display_text(cancel) + 24).max(64);
    let y = base.y() + base.height() as i32 - 24 - 12;
    let ok_r = Rect::new(base.x() + base.width() as i32 - 16 - w_ok, y, w_ok as u32, 20);
    let cancel_r = Rect::new(ok_r.x() - 16 - w_cancel, y, w_cancel as u32, 20);
    widgets::draw_button(c, fs, tm, cancel_r, cancel, BtnState::default());
    widgets::draw_button(c, fs, tm, ok_r, ok, BtnState { default: true, ..Default::default() });
    if let Some(r) = reg {
        r.push((cancel_r, Hit::Cancel));
        r.push((ok_r, Hit::Ok));
    }
}

/// Draw one page inside `region`, shifted `ox` px (slide transition).
/// `region_is_pane`: the section list is drawn by the caller and `region` is only
/// the right-hand panel.
#[allow(clippy::too_many_arguments)]
fn draw_view(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, region: Rect, region_is_pane: bool, v: &View, ox: i32, pressed: bool, hits: Option<&mut Vec<(Rect, Hit)>>) {
    let base = Rect::new(region.x() + ox, region.y(), region.width(), region.height());
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let muted = widgets::secondary_ink(tm);
    let mut none = Vec::new();
    let reg: &mut Vec<(Rect, Hit)> = match hits {
        Some(h) => h,
        None => &mut none,
    };
    let on_screen = ox == 0;
    fill_in_frame(c, base, paper);

    match &v.page {
        Page::WifiPassword(ssid) => {
            let net = chrome::fit_text(fs, ssid, base.width() as i32 - 20);
            fs.draw_text(c, base.x() + 12, base.y() + 12, i18n::tr("Password for"), muted);
            fs.draw_text(c, base.x() + 12, base.y() + 28, &net, ink);
            let field = Rect::new(base.x() + 12, base.y() + 48, base.width() - 24, widgets::FIELD_H as u32);
            widgets::draw_field(c, fs, tm, field, &widgets::Field { text: &v.password, caret: Some(v.password.chars().count()), masked: !v.show_password, ..Default::default() });
            dialog_buttons(c, fs, tm, &base, "Cancel", "Connect", on_screen.then_some(&mut *reg));
            return;
        }
        Page::Confirm(kind) => {
            let (l1, l2) = match kind {
                Confirm::Reboot => ("Restart the Cardputer?", "Unsaved work in apps is lost."),
                Confirm::Poweroff => ("Power off the Cardputer?", "Press the power button to start."),
                Confirm::Official => ("Switch to the official launcher?", "Harbour will close."),
                Confirm::RemoveApp(_) => ("Remove this app from Harbour?", "Its .desktop file goes to the Trash."),
            };
            fs.draw_text(c, base.x() + 12, base.y() + 24, i18n::tr(l1), ink);
            fs.draw_text(c, base.x() + 12, base.y() + 40, i18n::tr(l2), crate::launcher::widgets::secondary_ink(tm));
            let ok = match kind {
                Confirm::Reboot => "Restart",
                Confirm::Poweroff => "Power Off",
                Confirm::Official => "Switch",
                Confirm::RemoveApp(_) => "Remove",
            };
            dialog_buttons(c, fs, tm, &base, "Cancel", ok, on_screen.then_some(&mut *reg));
            return;
        }
        _ => {}
    }

    // Section list (unless the caller already drew it) and the panel to its right.
    let pane = if region_is_pane {
        base
    } else {
        draw_sidebar(c, fs, tm, &base, v.section, v.in_root, if on_screen { Some(&mut *reg) } else { None });
        // While the cursor is in the sidebar it opens out to show every name whole (Japanese names
        // are wider): the page starts after it, so no label, check box or button is covered.
        let sb = if v.in_root { sidebar_full_w(fs).min(base.width() as i32 / 2) } else { SB_W };
        Rect::new(base.x() + sb, base.y(), (base.width() as i32 - sb).max(1) as u32, base.height())
    };
    let shown = if v.in_root { SECTION_PAGES[v.section].clone() } else { v.page.clone() };
    if shown == Page::AgentConnect {
        draw_connect(c, fs, tm, &pane, &v.rows);
        return;
    }
    let panel = Settings::panel_page(&shown);
    let rh = PANEL_ROW_H;
    let top = pane.y() + 2;
    let visible = ((pane.height() as i32 - 2) / rh).max(1) as usize;
    // The scroll bar column is always there (a full-height thumb when everything fits), so the rows
    // and their rules keep the same width on every page.
    let list_w = pane.width() as i32 - (SCROLL_W + 2);
    let mut local = Vec::new();

    if v.rows.is_empty() {
        fs.draw_text(c, pane.x() + 12, top + 2, i18n::tr("Nothing here"), crate::launcher::widgets::secondary_ink(tm));
    }
    for (n, r) in v.rows.iter().skip(v.scroll).take(visible).enumerate() {
        let idx = v.scroll + n;
        let y = top + n as i32 * rh;
        let selected = idx == v.cursor;
        let area = Rect::new(pane.x(), y, list_w as u32, rh as u32);
        if panel {
            let choice = choice_spec(&shown, idx).is_some();
            draw_panel_row(c, fs, tm, r, idx, area, selected, choice, pressed, &mut local);
        } else {
            draw_list_row(c, fs, tm, r, idx, Rect::new(pane.x() + 2, y, (list_w - 4) as u32, rh as u32 - 1), selected, &mut local);
        }
    }
    {
        let x = pane.x() + pane.width() as i32 - SCROLL_W - 1;
        // The column runs on below the last row, down to y 166 (1 px clear of the bottom line).
        let bottom = pane.y() + pane.height() as i32 - 1;
        let track = Rect::new(x, top, SCROLL_W as u32, (bottom - top).max(visible as i32 * rh) as u32);
        let sb = ScrollBar::new(track, v.rows.len(), visible, v.scroll);
        sb.draw(c, tm, v.sb_drag.then_some(SbPart::Thumb));
        if on_screen && !v.in_root && sb.needed() {
            local.push((Rect::new(x - 1, top, SCROLL_W as u32 + 2, track.height()), Hit::Scroll { total: v.rows.len(), visible }));
        }
    }
    if let (Some((row, items, cur, hi)), true) = (&v.popup, on_screen) {
        if *row >= v.scroll && *row < v.scroll + visible {
            let area = Rect::new(pane.x(), top + (*row - v.scroll) as i32 * rh, list_w as u32, rh as u32);
            let dd = Dropdown::new(choice_rect(&area), items.clone());
            dd.draw_popup(c, fs, tm, *cur, Some(*hi));
            for i in 0..items.len() {
                local.push((dd.item_rect(i), Hit::PopupItem(i)));
            }
        }
    }
    if on_screen {
        reg.extend(local);
    }
    // The cursor is in the sidebar: open it out over the page (drawn last, its hits first).
    if v.in_root && !region_is_pane && on_screen && v.popup.is_none() {
        let mut side = Vec::new();
        draw_sidebar_expanded(c, fs, tm, &base, v.section, Some(&mut side));
        if !side.is_empty() {
            side.extend(reg.drain(..).filter(|(r, _)| r.x() >= base.x() + sidebar_full_w(fs) + 2));
            *reg = side;
        }
    }
}

/// Fill `r` in `color` where it lies inside the app frame (never on its line).
fn fill_in_frame(c: &mut Canvas<Window>, r: Rect, color: Col) {
    c.set_draw_color(color);
    for y in r.y()..r.bottom() {
        if let Some((x0, x1)) = crate::grid::app_frame_span(y) {
            let (a, b) = (r.x().max(x0), (r.right() - 1).min(x1));
            if a <= b {
                let _ = c.fill_rect(Rect::new(a, y, (b - a + 1) as u32, 1));
            }
        }
    }
}

/// The scroll bar under a `Hit::Scroll` area (the area is one pixel wider on both sides).
fn sb_rect(hit: Rect) -> Rect {
    Rect::new(hit.x() + 1, hit.y(), hit.width().saturating_sub(2), hit.height())
}

const ICONS: [[&str; 12]; 10] = [
    // Display
    ["............", ".XXXXXXXXXX.", ".X........X.", ".X........X.", ".X........X.", ".X........X.", ".X........X.", ".XXXXXXXXXX.", "....XXXX....", "...XXXXXX...", "............", "............"],
    // Theme: a contrast disc (one half ink)
    ["............", "...XXXXX....", "..X..XXXX...", ".X...XXXXX..", ".X...XXXXX..", ".X...XXXXX..", ".X...XXXXX..", ".X...XXXXX..", "..X..XXXX...", "...XXXXX....", "............", "............"],
    // Lock Screen: a padlock
    ["............", "....XXXX....", "...X....X...", "...X....X...", "...X....X...", "..XXXXXXXX..", "..XXXXXXXX..", "..XXX..XXX..", "..XXX..XXX..", "..XXXXXXXX..", "..XXXXXXXX..", "............"],
    // Sound
    ["............", "....X.......", "...XX..X....", "XXXXX...X...", "XXXXX.X.X...", "XXXXX.X.X...", "XXXXX.X.X...", "XXXXX...X...", "...XX..X....", "....X.......", "............", "............"],
    // Network
    ["..........X.", "..........X.", "........X.X.", "........X.X.", "......X.X.X.", "......X.X.X.", "....X.X.X.X.", "....X.X.X.X.", "..X.X.X.X.X.", "..X.X.X.X.X.", "XXXXXXXXXXXX", "............"],
    // Bluetooth
    [".....X......", ".....XX.....", "..X..X.X....", "...X.X..X...", "....XX.X....", ".....X......", "....XX.X....", "...X.X..X...", "..X..X.X....", ".....XX.....", ".....X......", "............"],
    // ExtPort
    ["...X....X...", "...X....X...", "...X....X...", "..XXXXXXXX..", "..X......X..", "..X......X..", "..XX....XX..", "...XXXXXX...", ".....XX.....", ".....XX.....", ".....XX.....", "............"],
    // System
    ["..X..X..X...", ".XXXXXXXXXX.", "..X......X..", "XXX......XXX", "..X..XX..X..", "XXX..XX..XXX", "..X......X..", "XXX......XXX", ".XXXXXXXXXX.", "..X..X..X...", "............", "............"],
    // Apps
    ["XXXXX.XXXXX.", "X...X.X...X.", "X...X.X...X.", "X...X.X...X.", "XXXXX.XXXXX.", "............", "XXXXX.XXXXX.", "X...X.X...X.", "X...X.X...X.", "X...X.X...X.", "XXXXX.XXXXX.", "............"],
    // Agent: a speech bubble with three dots
    ["............", ".XXXXXXXXXX.", ".X........X.", ".X.X..X.X.X.", ".X........X.", ".XXXXXXXXXX.", "..XXX.......", "...XX.......", "............", "............", "............", "............"],
];

/// Offset that centres an icon's inked pixels in its 12x12 cell, so icons drawn with different
/// margins still line up in the sidebar.
fn icon_offset(i: usize) -> (i32, i32) {
    let (mut x0, mut x1, mut y0, mut y1) = (12i32, -1i32, 12i32, -1i32);
    for (ry, line) in ICONS[i].iter().enumerate() {
        for (rx, b) in line.bytes().enumerate() {
            if b == b'X' {
                x0 = x0.min(rx as i32);
                x1 = x1.max(rx as i32);
                y0 = y0.min(ry as i32);
                y1 = y1.max(ry as i32);
            }
        }
    }
    if x1 < 0 {
        return (0, 0);
    }
    ((12 - (x1 - x0 + 1)) / 2 - x0, (12 - (y1 - y0 + 1)) / 2 - y0)
}

fn draw_icon(c: &mut Canvas<Window>, x: i32, y: i32, i: usize, col: Col) {
    c.set_draw_color(col);
    let (ox, oy) = icon_offset(i);
    for (ry, line) in ICONS[i].iter().enumerate() {
        for (rx, b) in line.bytes().enumerate() {
            if b == b'X' {
                let _ = c.draw_point((x + ox + rx as i32, y + oy + ry as i32));
            }
        }
    }
}

/// Left-hand list of panels. Inverted while it has the focus; outlined while a
/// panel on the right is being edited. Hit rectangles go to `reg` when given.
fn draw_sidebar(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, area: &Rect, current: usize, focused: bool, reg: Option<&mut Vec<(Rect, Hit)>>) {
    draw_sidebar_w(c, fs, tm, area, current, focused, reg, SB_W);
}

/// Width the sidebar needs to show every section name whole (icon column + text + margins).
fn sidebar_full_w(fs: &FontSystem) -> i32 {
    let text = SECTIONS.iter().map(|n| fs.measure_text(i18n::tr_side(n))).max().unwrap_or(0);
    (20 + text + 10).max(SB_W)
}

/// While the cursor is in the sidebar, it opens out over the page so no section name is cut
/// ("エージェント" instead of "エージ..."), with a dithered shadow on its right edge.
fn draw_sidebar_expanded(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, area: &Rect, current: usize, reg: Option<&mut Vec<(Rect, Hit)>>) {
    let w = sidebar_full_w(fs);
    if w <= SB_W {
        return;
    }
    // The shadow stops a pixel above the frame's bottom line.
    crate::dither::fill(c, Rect::new(area.x() + w, area.y() + 2, 2, (area.bottom() - 1 - (area.y() + 2)).max(0) as u32), tm.get_color("widget_shadow"), Pattern::Half);
    draw_sidebar_w(c, fs, tm, area, current, true, reg, w);
}

#[allow(clippy::too_many_arguments)]
fn draw_sidebar_w(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, area: &Rect, current: usize, focused: bool, mut reg: Option<&mut Vec<(Rect, Hit)>>, sb_w: i32) {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    fill_in_frame(c, Rect::new(area.x(), area.y(), (sb_w - 1) as u32, area.height()), tm.get_color("surface_alt"));
    let edge = if sb_w > SB_W { ink } else { tm.get_color("line") };
    ui::line(c, area.x() + sb_w - 1, area.y(), area.x() + sb_w - 1, area.y() + area.height() as i32 - 1, edge);
    for (i, name) in SECTIONS.iter().enumerate() {
        // Ten sections of 14 px leave a 2 px gap above (eight of 18 px would fill the 144 px).
        let top = if SECTIONS.len() as i32 * SIDE_ROW_H + 2 > area.height() as i32 { 0 } else { 2 };
        let y = area.y() + top + i as i32 * SIDE_ROW_H;
        let r = Rect::new(area.x() + 2, y, (sb_w - 6) as u32, SIDE_ROW_H as u32);
        let sel = i == current;
        if sel && focused {
            widgets::draw_row_plate(c, tm, r);
        } else if sel {
            // The section whose page is open: a page-coloured plate with an ink bar on its left. No
            // frame: a 14 px row has no room for one that clears a 12 px kanji by a pixel.
            let plate = Rect::new(r.x(), r.y(), r.width(), r.height() - 1);
            ui::fill_rect(c, plate, paper);
            ui::fill_rect(c, Rect::new(r.x(), r.y(), 2, plate.height()), ink);
        }
        let fg = if sel && focused { tm.get_color("select_fg") } else { ink };
        draw_icon(c, r.x() + 4, y + (SIDE_ROW_H - 12) / 2, i, fg);
        widgets::draw_row_text(c, fs, tm, r, r.x() + 20, &chrome::fit_text(fs, i18n::tr_side(name), sb_w - 6 - 18), sel && focused, ink);
        if let Some(h) = reg.as_deref_mut() {
            h.push((Rect::new(area.x(), if top == 0 { y } else { y - 1 }, sb_w as u32, SIDE_ROW_H as u32), Hit::Sidebar(i)));
        }
    }
}

/// The Agent > Connect page: the device addresses on the left, a QR code of the setup guide on the
/// right (1-bit black on white with its quiet zone, whatever the theme).
fn draw_connect(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, pane: &Rect, rows: &[Row]) {
    static QR: std::sync::OnceLock<Option<qr::Qr>> = std::sync::OnceLock::new();
    let code = QR.get_or_init(|| qr::encode(AGENT_DOC_URL.as_bytes()).ok());
    let (ink, faint) = (tm.get_color("foreground"), widgets::secondary_ink(tm));
    let side = code.as_ref().map(|q| qr::pixel_size(q, 3)).unwrap_or(0);
    let qx = pane.x() + pane.width() as i32 - side - 2;
    if let Some(q) = code {
        qr::draw(c, q, qx, pane.y() + (pane.height() as i32 - side) / 2, 3);
    }
    let w = (qx - pane.x() - 12).max(40);
    let text = |c: &mut Canvas<Window>, y: i32, t: &str, col| {
        let row = Rect::new(pane.x(), y, w as u32 + 12, 16);
        widgets::draw_row_text(c, fs, tm, row, pane.x() + 8, &chrome::fit_text(fs, t, w), false, col);
    };
    let mut y = pane.y() + 6;
    for t in ["Scan for the", "Agent setup guide."] {
        text(c, y, t, ink);
        y += 16;
    }
    y += 8;
    for r in rows.iter().take(2) {
        let v = match &r.val {
            Val::Text(t) => t.clone(),
            _ => String::new(),
        };
        text(c, y, &r.label, faint);
        text(c, y + 14, &v, ink);
        y += 34;
    }
}

fn lock_label(secs: i64) -> String {
    if secs == 0 {
        "As screen off".into()
    } else {
        dark_label(secs)
    }
}

fn dark_label(secs: i64) -> String {
    match secs {
        0 => "Never".into(),
        s if s < 60 => format!("{s} sec"),
        s => format!("{} min", s / 60),
    }
}

/// The optional Japanese input dictionary is in place (see `installer::Job::ImeDict`).
fn ime_dict_installed() -> bool {
    crate::launcher::ime_glue::data_dir().join(crate::ime::DICT_FILE).is_file()
}

fn uptime_label(s: u64) -> String {
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;
    use std::thread::sleep;

    fn settings() -> (Settings, Arc<MockBackend>, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "harbour-settings-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = Arc::new(Mutex::new(Config::load(dir.join("config.json"))));
        let mut mock = MockBackend::new();
        mock.delay = Duration::from_millis(5);
        let mock = Arc::new(mock);
        let apps = vec![
            AppRef { id: "a.desktop".into(), name: "Alpha".into(), exec: "/bin/a".into(), desktop: Some("/apps/alpha.desktop".into()), user: false },
            AppRef { id: "b.desktop".into(), name: "Beta".into(), exec: "/bin/b".into(), desktop: None, user: false },
        ];
        let mut s = Settings::new(mock.clone(), cfg, dir.join("order.txt"), apps);
        s.set_agent_paths(dir.join("agent-data"), dir.join("agent.yaml"));
        (s, mock, dir)
    }

    fn press(s: &mut Settings, k: Keycode) -> Event {
        s.handle_key(k, Mod::NOMOD)
    }

    fn wait(s: &mut Settings) {
        for _ in 0..200 {
            s.tick();
            if !s.is_busy() {
                return;
            }
            sleep(Duration::from_millis(5));
        }
        panic!("worker did not finish");
    }

    /// Open a section from the section list.
    fn goto(s: &mut Settings, page: Page) {
        s.cursor = SECTION_PAGES.iter().position(|p| *p == page).expect("a section");
        press(s, Keycode::Return);
    }

    #[test]
    fn root_lists_the_official_sections() {
        let (s, _, _) = settings();
        let labels: Vec<_> = s.rows().into_iter().map(|r| r.label).collect();
        assert_eq!(labels, ["Display", "Theme", "Lock Screen", "Sound", "Network", "Bluetooth", "Devices", "System", "Apps", "Agent"]);
    }

    #[test]
    fn agent_page_shows_names_only_and_toggles_enabled() {
        let (mut s, _, dir) = settings();
        std::fs::write(
            dir.join("agent.yaml"),
            "default: groq\nproviders:\n  groq:\n    kind: openai\n    base_url: https://secret.example/v1\n    model: m-1\n    api_key: sk-SECRET\n  anthropic:\n    kind: anthropic\n    model: claude-x\n    api_key_env: K\n",
        )
        .unwrap();
        s.set_agent_paths(dir.join("agent-data"), dir.join("agent.yaml"));
        goto(&mut s, Page::Agent);
        assert_eq!(s.page, Page::Agent);
        let rows = s.rows();
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Enabled", "Provider", "Model", "Status", "Connect"]);
        assert_eq!(rows[0].val, Val::Toggle(true));
        assert_eq!(rows[1].val, Val::Text("groq".into()));
        assert_eq!(rows[2].val, Val::Text("m-1".into()));
        // Nothing secret reaches the page text (also what the Agent tool describe() would see).
        let d = s.describe();
        assert!(!d.contains("SECRET") && !d.contains("secret.example") && !d.contains("https://"), "{d}");
        // Enter on Enabled switches it off, persists, and tells the host.
        s.cursor = 0;
        assert_eq!(press(&mut s, Keycode::Return), Event::AgentChanged);
        assert!(!agent_history::load_state(&dir.join("agent-data")).enabled);
        assert_eq!(s.rows()[0].val, Val::Toggle(false));
        // Left / Right on Provider cycles the configured names and stores the choice.
        s.cursor = 1;
        assert_eq!(press(&mut s, Keycode::Right), Event::AgentChanged);
        assert_eq!(s.rows()[1].val, Val::Text("anthropic".into()));
        assert_eq!(agent_history::load_state(&dir.join("agent-data")).provider.as_deref(), Some(s.agent.provider.as_str()));
    }

    #[test]
    fn agent_status_shows_the_last_call_and_a_loose_config_warning() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Agent);
        assert_eq!(s.rows()[3].val, Val::Text("Mock (no network)".into()), "no config: the mock");
        let data = dir.join("agent-data");
        agent_history::save_state(&data, &agent_history::State { last_status: Some("Error: HTTP 401".into()), ..Default::default() });
        s.refresh_agent();
        assert_eq!(s.rows()[3].val, Val::Text("Error: HTTP 401".into()));
    }

    #[test]
    fn open_agent_lands_on_the_agent_page_and_back_goes_to_the_list() {
        let (mut s, _, _) = settings();
        s.open_agent();
        assert_eq!(s.page, Page::Agent);
        press(&mut s, Keycode::Escape);
        assert_eq!(s.page, Page::Root);
    }

    #[test]
    fn connect_page_lists_addresses_and_opens_from_the_agent_page() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::Agent);
        s.cursor = 4;
        press(&mut s, Keycode::Return);
        assert_eq!(s.page, Page::AgentConnect);
        wait(&mut s);
        let labels: Vec<_> = s.rows().into_iter().map(|r| r.label).collect();
        assert_eq!(labels, ["Wi-Fi IP", "Ethernet IP", "Setup guide"]);
        assert!(qr::encode(AGENT_DOC_URL.as_bytes()).is_ok(), "the guide link fits a QR code");
        press(&mut s, Keycode::Escape);
        assert_eq!(s.page, Page::Agent);
    }

    #[test]
    fn brightness_is_applied_and_persisted_with_a_floor() {
        let (mut s, mock, dir) = settings();
        goto(&mut s, Page::Display);
        s.cursor = 1;
        for _ in 0..30 {
            press(&mut s, Keycode::Left);
        }
        assert_eq!(mock.brightness(), Some((5, 100)), "never goes fully dark");
        press(&mut s, Keycode::Right);
        assert_eq!(mock.brightness(), Some((10, 100)));
        s.flush();
        let saved = Config::load(dir.join("config.json"));
        assert_eq!(saved.get_int("brightness", 0), 10);
    }

    #[test]
    fn screen_off_cycles_the_official_choices() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::Display);
        s.cursor = 2;
        assert_eq!(press(&mut s, Keycode::Right), Event::ScreenChanged);
        assert_eq!(s.cfg_int("dark_time", -1), 60);
        press(&mut s, Keycode::Right);
        assert_eq!(s.cfg_int("dark_time", -1), 300);
        press(&mut s, Keycode::Right);
        assert_eq!(s.cfg_int("dark_time", -1), 300, "clamped at the last option");
        for _ in 0..9 {
            press(&mut s, Keycode::Left);
        }
        assert_eq!(s.cfg_int("dark_time", -1), 0);
    }

    #[test]
    fn theme_row_steps_through_all_themes() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Theme);
        s.cursor = 0;
        assert_eq!(s.rows()[0].val, Val::Text("Amber".into()), "Amber is the default");
        assert_eq!(press(&mut s, Keycode::Left), Event::None, "already the first theme");
        for want in ["Sky", "Mobile", "Terminal", "Platinum"] {
            assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
            assert_eq!(s.rows()[0].val, Val::Text(want.into()));
        }
        assert_eq!(press(&mut s, Keycode::Right), Event::None, "already the last theme");
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_theme", 0), 4);
        assert_eq!(press(&mut s, Keycode::Left), Event::ThemeChanged);
    }

    #[test]
    fn status_bar_colour_is_a_popup_row_with_its_own_config_key() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Theme);
        s.cursor = 2;
        assert_eq!(s.rows()[2].val, Val::Text("Default".into()));
        assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
        assert_eq!(s.rows()[2].val, Val::Text("Black".into()));
        assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_statusbar", 0), 2);
        assert_eq!(press(&mut s, Keycode::Right), Event::None, "White is the last choice");
        press(&mut s, Keycode::Return);
        assert!(s.popup.is_some(), "Return opens the list");
    }

    #[test]
    fn invert_switches_to_the_inverted_theme_file() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Theme);
        s.cursor = 1;
        assert_eq!(s.rows()[1].val, Val::Toggle(false));
        assert_eq!(press(&mut s, Keycode::Return), Event::ThemeChanged);
        assert_eq!(s.rows()[1].val, Val::Toggle(true));
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_theme_invert", 0), 1);
        assert_eq!(theme_path(1, true), "assets/themes/theme-paper-invert.yaml");
        assert_eq!(theme_path(1, false), "assets/themes/theme-paper.yaml");
        assert_eq!(theme_path(99, false), "assets/themes/theme-platinum.yaml", "clamped");
    }

    #[test]
    fn every_theme_file_loads_with_distinct_ink_and_desktop() {
        use crate::theme::{Theme, ThemeManager};
        for (name, path) in THEMES {
            let t = Theme::load_from_yaml(&format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap_or_else(|e| panic!("{name}: {e}"));
            let tm = ThemeManager::new(t);
            let (ink, desk, paper) = (tm.get_color("foreground"), tm.get_color("background"), tm.get_color("widget_face_active"));
            let lum = |c: sdl2::pixels::Color| 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
            assert!((lum(ink) - lum(paper)).abs() > 100.0, "{name}: text must contrast with the windows");
            assert!((lum(ink) - lum(desk)).abs() > 60.0, "{name}: icons must contrast with the desktop");
        }
    }

    #[test]
    fn extport_toggles_the_5v_rails() {
        let (mut s, mock, _) = settings();
        goto(&mut s, Page::ExtPort);
        assert_eq!(mock.ext_port(ExtPort::Grove5V), Some(false));
        press(&mut s, Keycode::Return);
        assert_eq!(mock.ext_port(ExtPort::Grove5V), Some(true));
        s.cursor = 1;
        press(&mut s, Keycode::Right);
        assert_eq!(mock.ext_port(ExtPort::Ext5V), Some(true));
    }

    #[test]
    fn mute_toggle() {
        let (mut s, mock, _) = settings();
        goto(&mut s, Page::Audio);
        s.cursor = 1;
        press(&mut s, Keycode::Return);
        assert_eq!(mock.volume().map(|v| v.1), Some(true));
    }

    #[test]
    fn fzxc_move_the_cursor_without_fn() {
        let (mut s, _, _) = settings();
        press(&mut s, Keycode::X);
        press(&mut s, Keycode::X);
        assert_eq!(s.cursor, 2);
        press(&mut s, Keycode::F);
        assert_eq!(s.cursor, 1);
        // Left/right adjust values: C raises brightness, Z lowers it.
        press(&mut s, Keycode::F);
        press(&mut s, Keycode::Return); // Display
        press(&mut s, Keycode::X); // Brightness (Language is the first row)
        let before = s.rows()[1].val.clone();
        press(&mut s, Keycode::Z);
        assert_ne!(s.rows()[1].val, before);
    }

    #[test]
    fn escape_goes_back_then_closes() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        assert_eq!(s.title(), "Settings: System");
        assert_eq!(press(&mut s, Keycode::Escape), Event::None);
        assert_eq!(s.title(), "Settings");
        assert_eq!(s.cursor, 7, "cursor is restored on the parent page");
        assert_eq!(press(&mut s, Keycode::Escape), Event::Close);
    }

    #[test]
    fn wifi_scan_then_connect_to_a_saved_network() {
        let (mut s, mock, _) = settings();
        goto(&mut s, Page::Network); // Network
        wait(&mut s);
        assert_eq!(s.rows()[1].val, Val::Text("Harbour Cafe".into()));
        s.cursor = 5;
        press(&mut s, Keycode::Return); // scan
        wait(&mut s);
        let rows = s.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].val, Val::Text("Connected".into()));
        // Open network connects directly.
        s.cursor = 1;
        press(&mut s, Keycode::Return);
        wait(&mut s);
        assert_eq!(mock.wifi_status().ssid.as_deref(), Some("Pier 9 Guest"));
    }

    #[test]
    fn secured_network_asks_for_a_password() {
        let (mut s, mock, _) = settings();
        goto(&mut s, Page::Network);
        wait(&mut s);
        s.cursor = 5;
        press(&mut s, Keycode::Return);
        wait(&mut s);
        s.cursor = 2; // Lighthouse 2G (secured, unsaved)
        press(&mut s, Keycode::Return);
        assert!(matches!(s.page, Page::WifiPassword(_)));
        s.handle_text("short");
        press(&mut s, Keycode::Return);
        assert!(!s.is_busy(), "too short to try");
        s.password.clear();
        s.handle_text("wrongpassword");
        press(&mut s, Keycode::Return);
        wait(&mut s);
        assert!(matches!(s.page, Page::WifiPassword(_)), "stays for another try");
        s.password.clear();
        s.handle_text("anchor123");
        press(&mut s, Keycode::Return);
        wait(&mut s);
        assert_eq!(s.title(), "Settings: Network");
        assert_eq!(mock.wifi_status().ssid.as_deref(), Some("Lighthouse 2G"));
    }

    #[test]
    fn apps_visibility_uses_the_official_config_keys_and_order_is_saved() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Apps);
        assert_eq!(s.rows()[0].val, Val::Check(true));
        assert_eq!(press(&mut s, Keycode::Space), Event::AppsChanged);
        assert_eq!(s.rows()[0].val, Val::Check(false));
        let saved = Config::load(dir.join("config.json"));
        assert_eq!(saved.get_int(&desktop_config_key("a.desktop"), 1), 0);
        // Move Alpha down.
        assert_eq!(press(&mut s, Keycode::RightBracket), Event::AppsChanged);
        assert_eq!(s.rows()[1].label, "Alpha");
        assert_eq!(registry::load_order(&dir.join("order.txt")), ["b.desktop", "a.desktop"]);
    }

    #[test]
    fn a_users_own_app_is_removed_after_a_confirmation() {
        let (mut s, _, _) = settings();
        s.apps[0].user = true;
        goto(&mut s, Page::Apps);
        press(&mut s, Keycode::Return);
        assert_eq!(s.rows()[1].val, Val::Text("Your app".into()));
        assert_eq!(s.rows().len(), 4, "a user's app can always be removed");
        s.cursor = 3;
        press(&mut s, Keycode::Return);
        assert!(matches!(s.page, Page::Confirm(Confirm::RemoveApp(0))));
        // Cancel goes back to the app's page.
        press(&mut s, Keycode::Escape);
        assert_eq!(s.page, Page::AppInfo(0));
    }

    #[test]
    fn dpkg_owner_is_read() {
        assert_eq!(parse_dpkg_owner("zclaw: /usr/share/APPLaunch/applications/zclaw.desktop\n"), Some("zclaw".into()));
        assert_eq!(parse_dpkg_owner("libfoo1:arm64: /usr/lib/x\n"), Some("libfoo1".into()));
        assert_eq!(parse_dpkg_owner("a, b: /etc/x\n"), Some("a".into()));
        assert_eq!(parse_dpkg_owner("diversion by x from: /y\nreal: /y\n"), Some("real".into()));
        assert_eq!(parse_dpkg_owner(""), None);
    }

    #[test]
    fn an_app_page_shows_hides_and_offers_uninstall_for_installed_apps() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::Apps);
        // Return opens Alpha's page (an installed app).
        assert_eq!(press(&mut s, Keycode::Return), Event::None);
        assert_eq!(s.page, Page::AppInfo(0));
        let labels: Vec<String> = s.rows().into_iter().map(|r| r.label).collect();
        assert_eq!(&labels[..3], ["Show on Home", "Kind", "Package"]);
        assert_eq!(press(&mut s, Keycode::Return), Event::AppsChanged, "row 0 toggles");
        assert_eq!(s.rows()[0].val, Val::Toggle(false));
        if s.rows().len() == 4 {
            s.cursor = 3;
            match press(&mut s, Keycode::Return) {
                Event::Install(r) => assert!(r.removes()),
                e => panic!("{e:?}"),
            }
        }
        // A built-in app has no Uninstall row.
        press(&mut s, Keycode::Escape);
        s.cursor = 1;
        press(&mut s, Keycode::Return);
        assert_eq!(s.page, Page::AppInfo(1));
        assert_eq!(s.rows().len(), 3);
        assert_eq!(s.rows()[1].val, Val::Text("Built-in".into()));
    }

    #[test]
    fn system_page_headings_are_skipped_by_the_cursor() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        assert_eq!(s.cursor, 1, "opens below the Info heading");
        s.cursor = 5; // Clock sync
        press(&mut s, Keycode::Down);
        assert_eq!(s.rows()[s.cursor].label, "Check updates", "the Software heading is stepped over");
        press(&mut s, Keycode::Up);
        assert_eq!(s.rows()[s.cursor].label, "Clock sync");
        press(&mut s, Keycode::End);
        assert_eq!(s.rows()[s.cursor].label, "Power off");
        press(&mut s, Keycode::Home);
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn restart_requires_confirmation() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 12;
        press(&mut s, Keycode::Return);
        assert!(matches!(s.page, Page::Confirm(Confirm::Reboot)));
        press(&mut s, Keycode::Escape);
        assert_eq!(s.title(), "Settings: System");
    }

    #[test]
    fn app_store_launches_the_official_binary() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 8;
        assert_eq!(
            press(&mut s, Keycode::Return),
            Event::Launch("/usr/share/APPLaunch/bin/M5CardputerZero-AppStore".into())
        );
    }

    #[test]
    fn official_switch_exits_after_confirmation() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 9;
        press(&mut s, Keycode::Return);
        assert_eq!(press(&mut s, Keycode::Return), Event::Exit);
    }

    #[test]
    fn section_icons_are_centred_in_their_cell() {
        for i in 0..ICONS.len() {
            let (ox, oy) = icon_offset(i);
            let pts: Vec<(i32, i32)> = ICONS[i].iter().enumerate().flat_map(|(y, l)| l.bytes().enumerate().filter(|(_, b)| *b == b'X').map(move |(x, _)| (x as i32, y as i32))).collect();
            let (x0, x1) = (pts.iter().map(|p| p.0).min().unwrap() + ox, pts.iter().map(|p| p.0).max().unwrap() + ox);
            let (y0, y1) = (pts.iter().map(|p| p.1).min().unwrap() + oy, pts.iter().map(|p| p.1).max().unwrap() + oy);
            assert!(x0 >= 0 && x1 <= 11 && y0 >= 0 && y1 <= 11, "icon {i} stays in its cell");
            assert!((x0 - (11 - x1)).abs() <= 1 && (y0 - (11 - y1)).abs() <= 1, "icon {i} is centred");
        }
    }

    #[test]
    fn section_icons_are_12_by_12() {
        for (i, icon) in ICONS.iter().enumerate() {
            for line in icon {
                assert_eq!(line.len(), 12, "icon {i}");
            }
        }
        assert_eq!(SECTIONS.len(), SECTION_PAGES.len());
    }

    #[test]
    fn left_goes_back_except_on_adjustable_rows() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::Network); // Network: Wi-Fi toggle row
        assert_eq!(s.title(), "Settings: Network");
        wait(&mut s);
        assert_eq!(press(&mut s, Keycode::Left), Event::None);
        assert_eq!(s.title(), "Settings", "Left leaves a page with nothing to adjust");
        assert_eq!(press(&mut s, Keycode::Left), Event::None, "Left at the top does nothing");
        goto(&mut s, Page::Display); // Display: brightness slider
        press(&mut s, Keycode::Left);
        assert_eq!(s.title(), "Settings: Display", "Left lowers the slider instead");
        press(&mut s, Keycode::Escape);
        assert_eq!(s.title(), "Settings");
    }

    #[test]
    fn pages_change_instantly_without_a_slide() {
        let (mut s, _, _) = settings();
        assert!(!s.animating());
        goto(&mut s, Page::Display);
        assert!(!s.animating(), "entering a page does not slide");
        press(&mut s, Keycode::Escape);
        assert!(!s.animating(), "going back does not slide either");
    }

    #[test]
    fn scroll_bar_click_page_and_thumb_drag() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 10;
        press(&mut s, Keycode::Return); // About: more rows than fit
        let n = s.rows().len();
        let visible = 6;
        assert!(n > visible);
        // The hit area is a pixel wider than the 4 px bar on both sides.
        let track = Rect::new(299, 20, 6, 96);
        s.hits = vec![(track, Hit::Scroll { total: n, visible })];
        s.cursor = 0;
        // Below the thumb: a page down; the selection is pulled into the window.
        s.click(301, 110, 1);
        assert_eq!(s.scroll, visible.min(n - visible));
        assert!(s.cursor >= s.scroll && s.cursor < s.scroll + visible);
        // Above the thumb: back up a page.
        s.release(0, 0);
        s.click(299, 26, 1);
        assert_eq!(s.scroll, 0);
        // The arrow boxes step one row.
        s.release(0, 0);
        s.click(301, 114, 1);
        assert_eq!(s.scroll, 1);
        s.release(0, 0);
        s.click(301, 21, 1);
        assert_eq!(s.scroll, 0);
        // On the thumb: dragging follows the pointer; releasing ends it.
        s.release(0, 0);
        let ty = ScrollBar::new(sb_rect(track), n, visible, s.scroll).thumb().y();
        s.click(301, ty + 2, 1);
        assert!(s.sb_drag.is_some());
        s.drag(301, 1000);
        assert_eq!(s.scroll, n - visible);
        s.drag(301, -50);
        assert_eq!(s.scroll, 0);
        s.release(301, 0);
        assert!(s.sb_drag.is_none());
    }

    #[test]
    fn pointer_clicks_use_the_drawn_hit_rectangles() {
        let (mut s, mock, _) = settings();
        s.hits = vec![
            (Rect::new(0, 0, 80, 16), Hit::Sidebar(0)),
            (Rect::new(0, 16, 80, 16), Hit::Sidebar(6)),
        ];
        assert_eq!(s.hit(5, 20), Some(Hit::Sidebar(6)));
        assert_eq!(s.hit(200, 20), None);
        s.click(5, 20, 1);
        assert_eq!(s.cursor, 6, "single click selects");
        assert_eq!(s.page, Page::Root);
        s.click(5, 20, 2);
        assert_eq!(s.title(), "Settings: Devices", "double click opens");
        // A check box click toggles.
        s.hits = vec![(Rect::new(0, 0, 40, 16), Hit::Check(0))];
        s.click(3, 3, 1);
        assert_eq!(mock.ext_port(ExtPort::Grove5V), Some(true));
        // Dragging a slider thumb sets the value; releasing ends the drag.
        s.page = Page::Display;
        s.stack.clear();
        s.cursor = 1;
        s.hits = vec![(Rect::new(100, 0, 107, 11), Hit::Slider(1))];
        s.click(100 + 3 + 50, 5, 1);
        assert!(s.drag.is_some());
        assert!((48..=52).contains(&s.brightness_pct()), "got {}", s.brightness_pct());
        s.drag(100 + 3 + 100, 5);
        assert_eq!(s.brightness_pct(), 100, "past the right end clamps");
        s.release(0, 0);
        assert!(s.drag.is_none());
    }

    #[test]
    fn dragging_left_across_a_page_goes_back() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.hits = vec![(Rect::new(0, 0, 300, 100), Hit::Row(0))];
        s.click(200, 10, 1);
        s.release(140, 10);
        assert_eq!(s.title(), "Settings");
        assert_eq!(s.wheel_x(-1), Event::None);
        assert_eq!(s.wheel(1), Event::None);
        assert_eq!(s.cursor, 8);
    }

    #[test]
    fn enter_opens_a_popup_that_up_down_enter_and_esc_drive() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Theme);
        s.cursor = 0; // Theme
        press(&mut s, Keycode::Return);
        assert_eq!(s.popup, Some((0, 0)), "opens on the current value");
        press(&mut s, Keycode::Down);
        press(&mut s, Keycode::Down);
        assert_eq!(s.popup, Some((0, 2)));
        assert_eq!(press(&mut s, Keycode::Escape), Event::None);
        assert!(s.popup.is_none() && s.title() == "Settings: Theme", "Esc only closes the list");
        assert_eq!(s.cfg_int("harbour_theme", 0), 0, "nothing was applied");
        press(&mut s, Keycode::Return);
        press(&mut s, Keycode::Down);
        press(&mut s, Keycode::Down);
        assert_eq!(press(&mut s, Keycode::Return), Event::ThemeChanged);
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_theme", 0), 2);
        // Left / right still step the value while it is closed.
        assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
        assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
        assert_eq!(press(&mut s, Keycode::Right), Event::None, "Platinum is the last theme");
        assert_eq!(press(&mut s, Keycode::Left), Event::ThemeChanged);
        assert_eq!(press(&mut s, Keycode::Left), Event::ThemeChanged);
        assert_eq!(s.cfg_int("harbour_theme", 0), 2);
        // Screen off through the list.
        press(&mut s, Keycode::Escape);
        goto(&mut s, Page::Display);
        s.cursor = 2;
        press(&mut s, Keycode::Return);
        press(&mut s, Keycode::Down);
        assert_eq!(press(&mut s, Keycode::Return), Event::ScreenChanged);
    }

    #[test]
    fn controls_in_the_section_preview_work_at_once_and_the_pop_up_follows_the_pointer() {
        let (mut s, _, _) = settings();
        assert_eq!(s.page, Page::Root, "Settings starts on the section list with a preview");
        // Screen off's button in the preview of Display: enters Display and opens its list.
        s.hits = vec![(Rect::new(100, 20, 100, 14), Hit::Choice(2))];
        s.click(120, 25, 1);
        assert_eq!(s.page, Page::Display);
        assert_eq!(s.cursor, 2);
        assert_eq!(s.popup, Some((2, 2)), "opens on 30 sec");
        // Hovering an item highlights it; the wheel and keys move the highlight too.
        s.hits = (0..5).map(|i| (Rect::new(100, 40 + 16 * i, 100, 16), Hit::PopupItem(i as usize))).collect();
        s.hover(110, 40 + 16 * 4 + 2);
        assert_eq!(s.popup, Some((2, 4)));
        s.hover(5, 5);
        assert_eq!(s.popup, Some((2, 4)), "off the list keeps the highlight");
        s.wheel(-1);
        assert_eq!(s.popup, Some((2, 3)));
        // Clicking the highlighted entry picks it.
        assert_eq!(s.click(110, 40 + 16 * 3 + 2, 1), Event::ScreenChanged);
        assert!(s.popup.is_none());
        assert_eq!(s.cfg_int("dark_time", 30), DARK_TIMES[3]);
        // A slider and a check box in the preview of another section behave likewise.
        let (mut s, mock, _) = settings();
        s.cursor = 6; // Devices
        s.hits = vec![(Rect::new(0, 0, 40, 16), Hit::Check(0))];
        s.click(3, 3, 1);
        assert_eq!(s.page, Page::ExtPort);
        assert_eq!(mock.ext_port(ExtPort::Grove5V), Some(true));
    }

    #[test]
    fn clicking_a_popup_item_picks_it_and_clicking_away_cancels() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::Theme);
        s.hits = vec![(Rect::new(100, 20, 100, 14), Hit::Choice(2))];
        s.click(120, 25, 1);
        assert_eq!(s.popup, Some((2, 0)), "opens on the current status bar style (Default)");
        s.hits = vec![(Rect::new(100, 34, 100, 16), Hit::PopupItem(1))];
        assert_eq!(s.click(120, 40, 1), Event::ThemeChanged);
        assert_eq!(s.cfg_int("harbour_statusbar", 0), 1);
        assert!(s.popup.is_none());
        s.cursor = 2;
        s.open_popup();
        assert_eq!(s.click(5, 5, 1), Event::None);
        assert!(s.popup.is_none(), "a click elsewhere dismisses it");
    }

    #[test]
    fn dialog_buttons_are_clickable() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 12;
        press(&mut s, Keycode::Return);
        assert!(matches!(s.page, Page::Confirm(Confirm::Reboot)));
        s.hits = vec![(Rect::new(0, 0, 50, 20), Hit::Cancel)];
        s.click(5, 5, 1);
        assert_eq!(s.title(), "Settings: System");
    }

    #[test]
    fn language_row_switches_and_persists() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Display);
        s.cursor = 0;
        assert_eq!(s.rows()[0].val, Val::Text("English".into()));
        assert_eq!(press(&mut s, Keycode::Right), Event::ThemeChanged);
        assert_eq!(s.rows()[0].val, Val::Text("日本語".into()));
        assert_eq!(i18n::get(), i18n::Lang::Ja);
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_lang", 0), 1);
        press(&mut s, Keycode::Left);
        assert_eq!(i18n::get(), i18n::Lang::En);
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_lang", 0), 0);
    }

    #[test]
    fn lock_screen_off_is_a_popup_row_with_its_own_config_key() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Lock);
        assert_eq!(s.rows()[2].val, Val::Text("8 sec".into()), "default 8 s");
        s.cursor = 2;
        press(&mut s, Keycode::Return);
        assert_eq!(s.popup, Some((2, 2)), "opens on 8 s");
        press(&mut s, Keycode::Up);
        press(&mut s, Keycode::Up);
        assert_eq!(press(&mut s, Keycode::Return), Event::None);
        assert_eq!(s.rows()[2].val, Val::Text("As screen off".into()));
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_lock_dark", -1), -1, "debounced");
        s.flush();
        assert_eq!(Config::load(dir.join("config.json")).get_int("harbour_lock_dark", -1), 0);
        press(&mut s, Keycode::Right);
        assert_eq!(s.cfg_int("harbour_lock_dark", -1), 5);
        for _ in 0..9 {
            press(&mut s, Keycode::Right);
        }
        assert_eq!(s.cfg_int("harbour_lock_dark", -1), 60, "clamped at the last choice");
        assert_eq!(lock_label(60), "1 min");
    }

    #[test]
    fn display_theme_and_lock_screen_pages_hold_their_rows() {
        let (mut s, _, _) = settings();
        let labels = |s: &Settings| s.rows().into_iter().map(|r| r.label).collect::<Vec<_>>();
        goto(&mut s, Page::Display);
        assert_eq!(labels(&s), ["Language", "Brightness", "Screen off", "Japanese dictionary"]);
        press(&mut s, Keycode::Escape);
        goto(&mut s, Page::Theme);
        assert_eq!(labels(&s), ["Theme", "Invert", "Status bar"]);
        press(&mut s, Keycode::Escape);
        goto(&mut s, Page::Lock);
        assert_eq!(labels(&s), ["Style", "Auto lock", "Lock screen off", "Show date", "Show battery"]);
        // Every pop-up row offers its current value; Left adjusts exactly the slider and pop-up rows.
        for (page, rows) in [(Page::Display, 3), (Page::Theme, 3), (Page::Lock, 5)] {
            for r in 0..rows {
                if let Some(spec) = choice_spec(&page, r) {
                    assert_eq!(spec.values.len(), spec.labels.len());
                    assert!(spec.values.contains(&spec.default), "{page:?} row {r}: the default is a choice");
                }
            }
        }
        assert!(choice_spec(&Page::Display, 1).is_none(), "Brightness is a slider");
        assert!(choice_spec(&Page::Theme, 1).is_none(), "Invert is a check box");
    }

    #[test]
    fn lock_screen_style_auto_lock_and_shown_items_persist() {
        let (mut s, _, dir) = settings();
        goto(&mut s, Page::Lock);
        assert_eq!(s.rows()[0].val, Val::Text("Wall clock".into()), "the analog wall clock is the default");
        s.cursor = 0;
        for want in ["World Clock", "ASCII art", "7-segment"] {
            assert_eq!(press(&mut s, Keycode::Right), Event::None);
            assert_eq!(s.rows()[0].val, Val::Text(want.into()));
        }
        press(&mut s, Keycode::Right);
        assert_eq!(s.cfg_int("harbour_lock_style", 0), 3, "clamped at the last style");
        // Auto lock: off, then 1 / 5 / 15 min of idle time.
        s.cursor = 1;
        assert_eq!(s.rows()[1].val, Val::Text("Never".into()));
        press(&mut s, Keycode::Return);
        assert_eq!(s.popup, Some((1, 0)));
        press(&mut s, Keycode::Down);
        press(&mut s, Keycode::Down);
        press(&mut s, Keycode::Return);
        assert_eq!(s.cfg_int("harbour_lock_auto", 0), 300);
        assert_eq!(s.rows()[1].val, Val::Text("5 min".into()));
        // Date and battery are shown by default; Return and Left / Right switch them.
        s.cursor = 3;
        assert_eq!(s.rows()[3].val, Val::Toggle(true));
        press(&mut s, Keycode::Return);
        assert_eq!(s.rows()[3].val, Val::Toggle(false));
        s.cursor = 4;
        press(&mut s, Keycode::Right);
        assert_eq!(s.rows()[4].val, Val::Toggle(false));
        assert_eq!(press(&mut s, Keycode::Escape), Event::None, "back to the list, flushing the config");
        let saved = Config::load(dir.join("config.json"));
        assert_eq!(
            (saved.get_int("harbour_lock_style", -1), saved.get_int("harbour_lock_auto", -1), saved.get_int("harbour_lock_date", -1), saved.get_int("harbour_lock_battery", -1)),
            (3, 300, 0, 0)
        );
        assert_eq!(s.rows()[2].val, Val::Text("7-segment".into()), "the section list shows the style");
    }

    #[test]
    fn about_shows_device_info_and_credits_and_scrolls() {
        let (mut s, _, _) = settings();
        goto(&mut s, Page::System);
        s.cursor = 10;
        press(&mut s, Keycode::Return);
        assert_eq!(s.title(), "About Harbour");
        wait(&mut s); // IP addresses are fetched on entry
        let rows = s.rows();
        let find = |k: &str| rows.iter().find(|r| r.label == k).map(|r| r.val.clone());
        assert_eq!(find("OS"), Some(Val::Text("Debian GNU/Linux 13 (trixie)".into())));
        assert_eq!(find("Model"), Some(Val::Text("Raspberry Pi Compute Module 0".into())));
        assert_eq!(find("Wi-Fi IP"), Some(Val::Text("192.168.3.94".into())));
        assert_eq!(find("Map"), Some(Val::Text("Natural Earth".into())));
        assert!(rows.len() > 8, "longer than a screen, so it scrolls");
        for _ in 0..rows.len() {
            press(&mut s, Keycode::Down);
        }
        assert_eq!(s.cursor, rows.len() - 1);
        assert_eq!(s.wheel(-1), Event::None);
        assert_eq!(s.cursor, rows.len() - 2);
    }

    #[test]
    fn labels() {
        assert_eq!(dark_label(0), "Never");
        assert_eq!(dark_label(10), "10 sec");
        assert_eq!(dark_label(300), "5 min");
        assert_eq!(uptime_label(3725), "1h 2m");
    }
}
