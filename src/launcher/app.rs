//! The contract every launcher app follows, so that all apps behave alike.
//!
//! * **Esc** (short): go back / close the thing in front (a dialog, a page). It never
//!   leaves the app from the top level (the host ignores a [`AppAction::Quit`] returned
//!   for Esc). **Holding Esc** (0.9 s) leaves the app from anywhere, and so does the
//!   menu's "Quit"; the host does this, apps need no code for it.
//! * **Alt** (tapped alone): the host opens the app menu built from
//!   [`LauncherApp::menu`] plus "Quit". Tab and Esc belong to the app (Tab long =
//!   lock is the host's); Ctrl+Tab also opens the menu.
//! * **Right click** opens the same menu; wheel / drag / double click arrive as
//!   [`PointerEvent`]s.
//! * Arrows: Up/Down/Left/Right, and F/X/Z/C work as arrows without Fn
//!   (use `platform::keys::arrow_alias`) unless the app takes text.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use crate::keys::{Keycode, Mod};
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::font::FontSystem;
use crate::launcher::widgets::{MenuItem, PointerEvent};
use crate::system::Backend;
use crate::theme::ThemeManager;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    None,
    /// Leave the app (back to the home screen).
    Quit,
    /// Show a short message at the bottom of the screen.
    Toast(String),
    /// Run an external program (path + args, validated like a .desktop Exec).
    Launch(String),
    /// Open a file in an app. `app` names it ("Text Editor", "Images", "Sheet", "Music", "Paint"...);
    /// `None` picks one from the extension. The host shows a toast when nothing can open it.
    Open { app: Option<String>, path: PathBuf },
    /// Install something a feature needs (Debian packages, the Japanese dictionary): the host
    /// shows its install card, which asks for the user's password when apt needs it.
    Install(crate::launcher::installer::InstallReq),
}

/// Things an app may need while it runs.
pub struct Ctx<'a> {
    pub fs: &'a FontSystem,
    pub tm: &'a ThemeManager,
    pub sys: &'a dyn Backend,
    pub now: DateTime<Local>,
}

pub trait LauncherApp {
    /// Shown in the window title.
    fn title(&self) -> String;

    /// App-specific menu entries; the host appends "Quit".
    fn menu(&self) -> Vec<MenuItem> {
        Vec::new()
    }

    /// Right click at (x, y): a context menu for what is under the pointer. `None` (the default)
    /// keeps the app menu. Chosen items arrive in [`LauncherApp::menu_action`].
    fn context_menu(&mut self, _x: i32, _y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        None
    }

    fn menu_action(&mut self, _id: &str, _ctx: &Ctx) -> AppAction {
        AppAction::None
    }

    /// A key press (not auto-repeat unless `repeat`). Esc is delivered too.
    fn key(&mut self, key: Keycode, mods: Mod, repeat: bool, ctx: &Ctx) -> AppAction;

    /// Open this file (another app asked via [`AppAction::Open`]). Called right after `make`.
    fn open_path(&mut self, _path: &Path) -> Result<(), String> {
        Err("This app cannot open files".into())
    }

    /// The host wants to leave (menu Quit, holding Esc). Return false to stay, for example to ask
    /// "Save changes?" first; the app then returns [`AppAction::Quit`] once it is safe to go.
    fn request_close(&mut self) -> bool {
        true
    }

    /// Typed text (only apps that take text need this).
    fn text(&mut self, _text: &str, _ctx: &Ctx) -> AppAction {
        AppAction::None
    }

    fn pointer(&mut self, _ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        AppAction::None
    }

    /// This app's keys for Help (Harbour menu > Help): (key, what it does), in English (translated
    /// when shown). Empty = Help shows only the keys every screen shares.
    fn help(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// A compact text description of what the app shows right now (for the Agent's
    /// `current_screen` tool). The default is just the title.
    fn describe(&self) -> String {
        self.title()
    }

    /// Called every frame. Return true when the screen needs repainting
    /// (animation, a ticking clock, finished background work).
    fn tick(&mut self, _ctx: &Ctx) -> bool {
        false
    }

    /// True while the app wants the host's pointer drawing hidden (a game that shows the pointer
    /// position some other way, like the original's HideCursor during play).
    fn hides_pointer(&self) -> bool {
        false
    }

    /// How the app's window may be sized on a desk (a screen bigger than 320 x 170). On the
    /// Cardputer Zero every app is full screen and this is not asked.
    fn sizing(&self) -> Sizing {
        Sizing::Fixed
    }

    /// True for a screen a short Esc closes (Help in its window); an app's Esc-at-top-level Quit is
    /// otherwise ignored (hold Esc or use the menu).
    fn esc_closes(&self) -> bool {
        false
    }

    /// The window size this screen asks for on a desk (surface width and height), when the usual
    /// one does not suit it (a folder with three apps needs a small window).
    fn desk_size(&self) -> Option<(i32, i32)> {
        None
    }

    /// False for apps with nowhere to type (Camera, Monitor, Yacht...): the input method then
    /// leaves their keys alone and its mark stays out of the menu strip.
    fn takes_text(&self) -> bool {
        true
    }

    /// True for apps whose Tab key matters (the host then needs Ctrl+Tab for the menu).
    fn wants_tab(&self) -> bool {
        false
    }

    /// Draw everything below the menu strip (the host draws the strip, the
    /// desktop backdrop, the app menu and toasts).
    fn draw(&mut self, c: &mut Canvas, assets: &mut AssetStore, ctx: &Ctx);
}

/// How an app's window may be sized on a desk ([`crate::launcher::desk`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sizing {
    /// Drawn pixel by pixel for 320 x 170 (it reads the `grid` constants): always that size.
    Fixed,
    /// Lays itself out from the surface (`grid::sw()` / `grid::sh()`): any size from 320 x 170 up.
    Flexible,
}

/// A registered app: how to start it. Its name, Japanese name and icon live in its package,
/// `apps/<id>/<id>.desktop` (`Exec=harbour:<id>`, see `launcher::packages`).
pub struct AppDef {
    /// Stable id: the package folder name, and the key folders, Recent and settings use.
    pub id: &'static str,
    /// The internal (English) name other apps use to ask for it (`AppAction::Open { app }`).
    pub name: &'static str,
    pub make: fn() -> Box<dyn LauncherApp>,
}
