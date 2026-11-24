//! A Home folder in a window of its own (on a desk). The desk keeps its icons; the window shows
//! the folder's apps the way Home does, and opening one asks the host to launch it in its own
//! window ([`take_launch`]).

use std::sync::Mutex;

use sdl2::keyboard::{Keycode, Mod};
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::launcher::app::{AppAction, Ctx, LauncherApp, Sizing};
use crate::launcher::home::{Home, HomeAction, Kind, Target};
use crate::launcher::widgets::PointerEvent;

/// An app a folder window asked to open; the host picks it up next frame.
static LAUNCH: Mutex<Option<(String, Target)>> = Mutex::new(None);

/// The app a folder window asked to open (its key, for Recent, and what to launch).
pub fn take_launch() -> Option<(String, Target)> {
    LAUNCH.lock().ok()?.take()
}

pub struct FolderWindow {
    name: String,
    home: Home,
}

impl FolderWindow {
    /// Folder `idx` of `home`, as it is now.
    pub fn new(home: &Home, idx: usize) -> Self {
        let name = home.folders.list.get(idx).map(|f| f.name.clone()).unwrap_or_default();
        FolderWindow { name, home: home.folder_view(idx) }
    }

    fn launch(&self, i: usize) -> AppAction {
        if let Some(item) = self.home.items.get(i).filter(|it| it.kind == Kind::App) {
            if let Ok(mut l) = LAUNCH.lock() {
                *l = Some((item.key(), item.target.clone()));
            }
        }
        AppAction::None
    }
}

impl LauncherApp for FolderWindow {
    fn takes_text(&self) -> bool {
        false
    }

    fn desk_size(&self) -> Option<(i32, i32)> {
        Some(self.home.folder_window_size())
    }

    fn title(&self) -> String {
        self.name.clone()
    }

    fn sizing(&self) -> Sizing {
        Sizing::Flexible
    }

    fn esc_closes(&self) -> bool {
        true
    }

    fn key(&mut self, key: Keycode, _mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if matches!(key, Keycode::Escape | Keycode::Backspace) {
            return AppAction::Quit;
        }
        match self.home.handle_key(key) {
            HomeAction::Open(i) => self.launch(i),
            _ => AppAction::None,
        }
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        match ev {
            PointerEvent::Down { x, y, button: 0, clicks } => match self.home.hit(x, y) {
                Some(i) => {
                    self.home.selected = i;
                    if clicks >= 2 {
                        return self.launch(i);
                    }
                    AppAction::None
                }
                None => AppAction::None,
            },
            PointerEvent::Wheel { dy, .. } => {
                let key = if dy > 0 { Keycode::Up } else { Keycode::Down };
                self.home.handle_key(key);
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn describe(&self) -> String {
        format!("The folder {} with {} apps", self.name, self.home.items.len())
    }

    fn draw(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
        self.home.draw(c, assets, ctx.fs, ctx.tm);
    }
}
