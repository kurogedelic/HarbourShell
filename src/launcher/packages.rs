//! Built-in app packages: one folder per app, like the official Cardputer Zero apps.
//!
//! ```text
//! apps/<id>/<id>.desktop   [Desktop Entry] Name=Paint / Name[ja]=ペイント / Icon=icon.png / Exec=harbour:paint
//! apps/<id>/icon.png       the Home icon (32 x 32, see docs/ICON-GUIDELINE.md)
//! apps/<id>/res/...        files only this app uses (loaded as `app/<id>/res/...` asset keys)
//! ```
//!
//! The `.desktop` file is read with the official parser ([`desktop_entry::parse`]); `Exec=harbour:<id>`
//! names a built-in (code in this binary: a [`REGISTRY`](crate::launcher::apps::REGISTRY) `make`, or a
//! screen the host draws itself). `Icon=` is relative to the folder. The id (folder name) is the app's
//! stable key for folders, Recent, Settings > Apps and the `goto:` script token; names are only shown.
//!
//! The order on the Home screen stays in code ([`builtins`]): the core tiles, the installed apps, then
//! Harbour's own tools.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use super::desktop_entry;
use super::home::Builtin;

/// Where the packages are, relative to the working directory (next to `assets/`).
pub const APPS_ROOT: &str = "apps";
/// `Exec=` prefix of a built-in app.
pub const EXEC_SCHEME: &str = "harbour:";

/// Host screens (not in `REGISTRY`), in Home order. Settings and Terminal come before the
/// installed apps, the rest after them.
const HOST: &[(&str, Builtin)] = &[
    ("settings", Builtin::Settings),
    ("terminal", Builtin::Terminal),
    ("texteditor", Builtin::TextEditor),
    ("calculator", Builtin::Calculator),
    ("clock", Builtin::Clock),
    ("monitor", Builtin::Monitor),
    ("snake", Builtin::Snake),
];

/// How many of [`HOST`] are shown before the installed apps.
pub const LEADING: usize = 2;

/// Keys older versions saved (the English display name) and the id each one became.
const LEGACY: &[(&str, &str)] = &[
    ("Settings", "settings"),
    ("Terminal", "terminal"),
    ("Text Editor", "texteditor"),
    ("Calculator", "calculator"),
    ("Clock", "clock"),
    ("World Clock", "clock"),
    ("Monitor", "monitor"),
    ("Snake", "snake"),
    ("Files", "files"),
    ("Timer", "timer"),
    ("Music", "music"),
    ("Radio", "radio"),
    ("Recorder", "voicememo"),
    ("Voice Memo", "voicememo"),
    ("Images", "images"),
    ("Camera", "camera"),
    ("Paint", "paint"),
    ("Sheet", "sheet"),
    ("Browser", "browser"),
    ("Calendar", "calendar"),
    ("Interceptor", "interceptor"),
];

impl Builtin {
    /// The package id (folder name) of this built-in.
    pub fn id(self) -> &'static str {
        match self {
            Builtin::App(n) => crate::launcher::apps::REGISTRY.get(n).map(|d| d.id).unwrap_or(""),
            b => HOST.iter().find(|(_, h)| *h == b).map(|(id, _)| *id).unwrap_or(""),
        }
    }
}

/// Every built-in this binary has, in Home order: (id, what it opens).
pub fn builtins() -> Vec<(&'static str, Builtin)> {
    let mut v: Vec<(&'static str, Builtin)> = HOST.to_vec();
    v.extend(crate::launcher::apps::REGISTRY.iter().enumerate().map(|(n, d)| (d.id, Builtin::App(n))));
    v
}

/// The built-in an `Exec=harbour:<id>` names (None for an id this binary does not have).
pub fn builtin_for(id: &str) -> Option<Builtin> {
    builtins().into_iter().find(|(i, _)| *i == id).map(|(_, b)| b)
}

/// The id an old saved key (a display name) stands for; ids and `.desktop` ids pass through.
pub fn migrate_key(key: &str) -> Option<&'static str> {
    LEGACY.iter().find(|(old, _)| *old == key).map(|(_, id)| *id)
}

/// One built-in app package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub id: String,
    pub name: String,
    /// `Name[ja]`; None keeps the English name (proper nouns).
    pub name_ja: Option<String>,
    pub dir: PathBuf,
    pub icon: Option<PathBuf>,
    /// `X-Harbour-Aliases`: names of installed apps that wear this app's icon (`cli` -> Terminal).
    pub aliases: Vec<String>,
    pub builtin: Builtin,
}

impl Package {
    /// The name in the current display language.
    pub fn shown_name(&self) -> String {
        self.name_in(crate::launcher::i18n::get())
    }

    /// The name in `lang`.
    pub fn name_in(&self, lang: crate::launcher::i18n::Lang) -> String {
        match (lang, &self.name_ja) {
            (crate::launcher::i18n::Lang::Ja, Some(ja)) => ja.clone(),
            _ => self.name.clone(),
        }
    }

    /// Does `name` (case and punctuation ignored) name this app: its id, either name, an alias
    /// or an old saved name?
    pub fn answers_to(&self, name: &str) -> bool {
        if self.name_ja.as_deref().map(|j| j == name.trim()).unwrap_or(false) {
            return true;
        }
        let n = normalize(name);
        if n.is_empty() {
            return false;
        }
        normalize(&self.id) == n
            || normalize(&self.name) == n
            || self.aliases.iter().any(|a| normalize(a) == n)
            || LEGACY.iter().any(|(old, id)| *id == self.id && normalize(old) == n)
    }
}

/// Asset key of a package's icon.
pub fn icon_key(id: &str) -> String {
    format!("app/{id}/icon")
}

/// Asset key prefix of a package's `res/` folder (`app/<id>/res`; a file `res/tools/a.png` is
/// `app/<id>/res/tools/a`).
pub fn res_key(id: &str) -> String {
    format!("app/{id}/res")
}

pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Read one package from its `.desktop` text. `dir` is its folder (the `Icon=` base).
pub fn parse(id: &str, text: &str, dir: &Path) -> Option<Package> {
    let e = desktop_entry::parse(text)?;
    let target = e.exec.trim().strip_prefix(EXEC_SCHEME)?;
    let builtin = builtin_for(target)?;
    let icon = (!e.icon.is_empty()).then(|| {
        let p = Path::new(&e.icon);
        if p.is_absolute() { p.to_path_buf() } else { dir.join(p) }
    });
    Some(Package {
        id: id.to_string(),
        name: e.name,
        name_ja: (!e.name_ja.is_empty()).then_some(e.name_ja),
        dir: dir.to_path_buf(),
        icon: icon.filter(|p| p.is_file()),
        aliases: e.aliases,
        builtin,
    })
}

/// Every package under `root` (`<root>/<id>/<id>.desktop`) whose built-in this binary has, in Home
/// order. Folders without a valid entry are skipped, so are ids the binary has no built-in for.
/// Settings is always there: without its package it gets a
/// plain entry, so a broken install can still be fixed from the device.
pub fn scan(root: &Path) -> Vec<Package> {
    let mut out: Vec<Package> = Vec::new();
    for (id, b) in builtins() {
        let dir = root.join(id);
        let found = std::fs::read_to_string(dir.join(format!("{id}.desktop")))
            .ok()
            .and_then(|t| parse(id, &t, &dir))
            .filter(|p| p.builtin == b);
        match found {
            Some(p) => out.push(p),
            None if b == Builtin::Settings => out.push(Package {
                id: id.to_string(),
                name: "Settings".into(),
                name_ja: Some("設定".into()),
                dir,
                icon: None,
                aliases: Vec::new(),
                builtin: b,
            }),
            None => eprintln!("[apps] no package for {id} in {}", root.display()),
        }
    }
    out
}

static CATALOG: RwLock<Vec<Package>> = RwLock::new(Vec::new());

/// Make `pkgs` the packages names and icons are looked up in.
pub fn install(pkgs: Vec<Package>) {
    if let Ok(mut c) = CATALOG.write() {
        *c = pkgs;
    }
}

/// The installed packages (Home order).
pub fn catalog() -> Vec<Package> {
    CATALOG.read().map(|c| c.clone()).unwrap_or_default()
}

/// The package for an id, a name or an old saved name.
pub fn find(name: &str) -> Option<Package> {
    let c = CATALOG.read().ok()?;
    c.iter().find(|p| p.id == name).or_else(|| c.iter().find(|p| p.answers_to(name))).cloned()
}

/// The package whose icon an installed app of this name wears (its name or an alias).
pub fn icon_for_name(name: &str) -> Option<Package> {
    let n = normalize(name);
    let c = CATALOG.read().ok()?;
    c.iter().find(|p| normalize(&p.name) == n || normalize(&p.id) == n || p.aliases.iter().any(|a| normalize(a) == n)).cloned()
}

/// Load each package's icon (`app/<id>/icon`) and its `res/` PNGs (`app/<id>/res/...`) into the store.
pub fn load_assets(assets: &mut crate::assets::AssetStore, pkgs: &[Package]) {
    for p in pkgs {
        if let Some(icon) = &p.icon {
            if let Err(e) = assets.load_icon_file(&icon_key(&p.id), icon) {
                eprintln!("[apps] {}: icon {}: {e}", p.id, icon.display());
            }
        }
        let res = p.dir.join("res");
        if res.is_dir() {
            assets.load_tree(&res, &res_key(&p.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_apps() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(APPS_ROOT)
    }

    #[test]
    fn every_builtin_has_a_package_with_an_icon() {
        let pkgs = scan(&repo_apps());
        let ids: Vec<&str> = pkgs.iter().map(|p| p.id.as_str()).collect();
        let want: Vec<&str> = builtins().iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, want, "found from apps/, in Home order");
        for p in &pkgs {
            assert!(p.icon.is_some(), "{} has icon.png", p.id);
            assert_eq!(p.builtin.id(), p.id);
            assert!(p.name.len() <= 12, "{} is too long for a label", p.name);
            if let Some(ja) = &p.name_ja {
                assert!(!ja.ends_with('ー'), "{ja}: no trailing long-vowel mark");
            }
        }
    }

    #[test]
    fn harbour_exec_maps_to_the_builtin() {
        let dir = Path::new("/nonexistent/paint");
        let p = parse("paint", "[Desktop Entry]\nName=Paint\nName[ja]=ペイント\nExec=harbour:paint\nIcon=icon.png\nType=Application\n", dir).unwrap();
        assert_eq!(p.builtin, builtin_for("paint").unwrap());
        assert!(matches!(p.builtin, Builtin::App(_)));
        assert_eq!(p.name_ja.as_deref(), Some("ペイント"));
        let p = parse("calculator", "[Desktop Entry]\nName=Calculator\nExec=harbour:calculator\n", dir).unwrap();
        assert_eq!(p.builtin, Builtin::Calculator);
        assert!(parse("x", "[Desktop Entry]\nName=X\nExec=harbour:nosuchapp\n", dir).is_none(), "unknown built-in");
        assert!(parse("x", "[Desktop Entry]\nName=X\nExec=/usr/bin/x\n", dir).is_none(), "external commands are not packages");
    }

    #[test]
    fn missing_packages_keep_settings() {
        let pkgs = scan(Path::new("/nonexistent"));
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].builtin, Builtin::Settings);
    }

    #[test]
    fn old_display_names_migrate_to_ids() {
        assert_eq!(migrate_key("Text Editor"), Some("texteditor"));
        assert_eq!(migrate_key("Recorder"), Some("voicememo"));
        assert_eq!(migrate_key("Copter"), None, "Stunt Copter is an external app now");
        assert_eq!(migrate_key("fune.desktop"), None);
        assert_eq!(migrate_key("paint"), None, "already an id");
        for (_, id) in LEGACY {
            assert!(builtins().iter().any(|(i, _)| i == id), "{id}");
        }
    }

    #[test]
    fn names_answer_by_id_name_alias_and_old_name() {
        let pkgs = scan(&repo_apps());
        let vm = pkgs.iter().find(|p| p.id == "voicememo").unwrap();
        for n in ["voicememo", "Voice Memo", "Recorder", "rec", "ボイスメモ"] {
            assert!(vm.answers_to(n), "{n}");
        }
        assert!(!vm.answers_to("Paint"));
    }
}
