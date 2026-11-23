//! App registry: discovers `.desktop` entries in the APPLaunch applications
//! directory (plus the preinstalled manifest) and orders them the way the
//! official launcher does (product apps first, then by modification time).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::desktop_entry::{self, DesktopEntry};

pub const DEFAULT_APP_DIR: &str = "/usr/share/APPLaunch/applications";

#[derive(Clone, Debug)]
pub struct AppEntry {
    /// `.desktop` file name (stable identity, also the visibility-key source).
    pub id: String,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub exec: String,
    pub terminal: bool,
    pub sysplause: bool,
    pub mtime: Option<SystemTime>,
    /// From the user's own app folder ([`user_app_dir`]): removable without root.
    pub user: bool,
}

/// Product app order used by the official launcher (labels, normalised).
const BUILTIN_ORDER: &[&str] = &[
    "settings", "store", "cli", "python", "zclaw", "ssh", "ippanel", "files", "camera", "rec",
    "music", "piano", "compass", "irremote", "irchat", "calculator", "snake", "tank",
    "keyboardguide", "factorytest", "gps", "lora", "nfc", "capcc1101subgchat",
];

fn normalize(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn builtin_rank(label: &str) -> Option<usize> {
    let n = normalize(label);
    if n == "subgchat" {
        return Some(23);
    }
    BUILTIN_ORDER.iter().position(|b| *b == n)
}

fn resolve_icon(base: &Path, icon: &str) -> Option<PathBuf> {
    if icon.is_empty() {
        return None;
    }
    let p = Path::new(icon);
    let full = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    full.is_file().then_some(full)
}

fn from_entry(id: String, e: DesktopEntry, base: &Path, mtime: Option<SystemTime>) -> AppEntry {
    AppEntry {
        id,
        name: e.name,
        icon: resolve_icon(base, &e.icon),
        exec: e.exec,
        terminal: e.terminal,
        sysplause: e.sysplause,
        mtime,
        user: false,
    }
}

/// `filename<TAB>Name<TAB>Icon<TAB>Exec<TAB>Terminal<TAB>Sysplause` lines.
pub fn parse_preinstalled(contents: &str, base: &Path) -> Vec<AppEntry> {
    let mut out = Vec::new();
    for line in contents.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 || !desktop_entry::filename_valid(f[0]) || f[1].is_empty() || f[3].is_empty() {
            continue;
        }
        let b = |i: usize, d: bool| {
            f.get(i)
                .map(|v| matches!(*v, "true" | "True" | "1"))
                .unwrap_or(d)
        };
        out.push(from_entry(
            f[0].to_string(),
            DesktopEntry {
                name: f[1].to_string(),
                icon: f[2].to_string(),
                exec: f[3].to_string(),
                terminal: b(4, false),
                sysplause: b(5, true),
                name_ja: String::new(),
                aliases: Vec::new(),
            },
            base,
            None,
        ));
    }
    out
}

/// Harbour's own folder for apps the user adds, no root needed: `.desktop` files dropped here show
/// on Home like installed apps (an icon path is relative to this folder) and Settings > Apps can
/// remove them. `HARBOUR_USER_APP_DIR` overrides it; otherwise `$XDG_DATA_HOME/harbour/apps`
/// (`~/.local/share/harbour/apps`).
pub fn user_app_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("HARBOUR_USER_APP_DIR") {
        return Some(PathBuf::from(d));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("harbour/apps"))
}

/// The system apps (`app_dir`), then the user's own (`user_dir`), which win on the same id.
pub fn scan_with_user(app_dir: &Path, user_dir: Option<&Path>) -> Vec<AppEntry> {
    let mut apps = scan(app_dir);
    if let Some(u) = user_dir {
        for mut e in scan_dir(u, u) {
            e.user = true;
            apps.retain(|a| a.id != e.id);
            apps.push(e);
        }
        sort_official(&mut apps);
    }
    apps
}

/// The valid `.desktop` files in `dir`, icons relative to `base`.
fn scan_dir(dir: &Path, base: &Path) -> Vec<AppEntry> {
    let mut apps: Vec<AppEntry> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let Some(name) = ent.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !desktop_entry::filename_valid(&name) {
                continue;
            }
            let path = ent.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(entry) = desktop_entry::parse(&text) else {
                continue;
            };
            let mtime = ent.metadata().and_then(|m| m.modified()).ok();
            apps.push(from_entry(name, entry, base, mtime));
        }
    }
    apps
}

/// Scan `app_dir`. Unreadable or invalid files are skipped.
pub fn scan(app_dir: &Path) -> Vec<AppEntry> {
    let base = app_dir.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut apps = scan_dir(app_dir, &base);
    if let Ok(text) = std::fs::read_to_string(base.join("preinstalled-desktop-apps.tsv")) {
        for e in parse_preinstalled(&text, &base) {
            if !apps.iter().any(|a| a.id == e.id) {
                apps.push(e);
            }
        }
    }
    sort_official(&mut apps);
    apps
}

/// Official order: product apps by rank, then everything else by mtime (oldest first).
pub fn sort_official(apps: &mut [AppEntry]) {
    apps.sort_by(|a, b| {
        let ra = builtin_rank(&a.name);
        let rb = builtin_rank(&b.name);
        match (ra, rb) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => match (a.mtime, b.mtime) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            },
        }
    });
}

/// Apply a user-chosen order (list of `.desktop` ids). Unlisted apps keep their
/// relative position after the listed ones.
pub fn apply_order(apps: &mut Vec<AppEntry>, order: &[String]) {
    let mut sorted: Vec<AppEntry> = Vec::with_capacity(apps.len());
    for id in order {
        if let Some(i) = apps.iter().position(|a| &a.id == id) {
            sorted.push(apps.remove(i));
        }
    }
    sorted.append(apps);
    *apps = sorted;
}

pub fn load_order(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn save_order(path: &Path, ids: &[String]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, ids.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, secs: Option<u64>) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: name.into(),
            icon: None,
            exec: "/bin/x".into(),
            terminal: false,
            sysplause: true,
            mtime: secs.map(|s| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s)),
            user: false,
        }
    }

    #[test]
    fn product_apps_first_then_mtime() {
        let mut v = vec![
            app("z.desktop", "Zeta", Some(10)),
            app("calc.desktop", "Calculator", Some(500)),
            app("a.desktop", "Alpha", Some(5)),
            app("f.desktop", "Files", Some(900)),
        ];
        sort_official(&mut v);
        let names: Vec<_> = v.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Files", "Calculator", "Alpha", "Zeta"]);
    }

    #[test]
    fn rank_normalises_labels() {
        assert_eq!(builtin_rank("Keyboard Guide"), Some(18));
        assert_eq!(builtin_rank("IR-Remote"), Some(13));
        assert_eq!(builtin_rank("Fune"), None);
    }

    #[test]
    fn custom_order_wins_and_unlisted_follow() {
        let mut v = vec![app("a.desktop", "A", None), app("b.desktop", "B", None), app("c.desktop", "C", None)];
        apply_order(&mut v, &["c.desktop".into(), "missing.desktop".into(), "a.desktop".into()]);
        let ids: Vec<_> = v.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["c.desktop", "a.desktop", "b.desktop"]);
    }

    #[test]
    fn preinstalled_manifest() {
        let t = "# header\nzclaw.desktop\tZClaw\tshare/images/claw_100.png\t/usr/share/APPLaunch/bin/ZClaw\tfalse\ttrue\n";
        let v = parse_preinstalled(t, Path::new("/nonexistent"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "ZClaw");
        assert!(!v[0].terminal && v[0].sysplause);
    }

    #[test]
    fn the_users_own_folder_adds_apps_and_wins_on_the_same_id() {
        let root = std::env::temp_dir().join(format!("harbour-userapps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sys = root.join("APPLaunch/applications");
        let user = root.join("harbour/apps");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        let entry = |name: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/bin/true\n");
        std::fs::write(sys.join("a.desktop"), entry("Alpha")).unwrap();
        std::fs::write(sys.join("b.desktop"), entry("Beta")).unwrap();
        std::fs::write(user.join("b.desktop"), entry("Beta (mine)")).unwrap();
        std::fs::write(user.join("c.desktop"), entry("Gamma")).unwrap();
        let apps = scan_with_user(&sys, Some(&user));
        let find = |id: &str| apps.iter().find(|a| a.id == id).unwrap();
        assert_eq!(apps.len(), 3);
        assert!(!find("a.desktop").user);
        assert_eq!(find("b.desktop").name, "Beta (mine)");
        assert!(find("b.desktop").user && find("c.desktop").user);
        assert_eq!(scan_with_user(&sys, None).len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}