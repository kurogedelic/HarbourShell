//! Home screen folders: the data model (no drawing, no SDL).
//!
//! A folder has a name and an ordered list of item keys. A key is what `Home` calls an item's
//! `key()`: the `.desktop` id of an external app, or the package id of a built-in (`apps/<id>/`).
//! Files from older versions name built-ins by their display name; `migrate_keys` turns them into ids.
//! An item is in at most one folder; everything not in a folder stays on the top level.
//!
//! Stored as `folders.json` next to `order.txt`:
//! `{"version":1,"folders":[{"name":"Games","items":["snake","nestopia.desktop"]}]}`.

use std::path::Path;

pub const DEFAULT_NAME: &str = "untitled folder";
pub const NAME_MAX: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Folder {
    pub name: String,
    pub items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Folders {
    pub list: Vec<Folder>,
}

/// Where an item goes: into a folder, or back to the top level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    Folder(usize),
    Top,
}

fn clean_name(name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(NAME_MAX).collect();
    let n = n.trim().to_string();
    if n.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        n
    }
}

impl Folders {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Load from `path`. A missing or corrupt file gives no folders; duplicate keys keep their first place.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Self::default() };
        let mut out = Self::default();
        let Some(arr) = v.get("folders").and_then(|f| f.as_array()) else { return out };
        let mut seen: Vec<String> = Vec::new();
        for f in arr {
            let Some(name) = f.get("name").and_then(|n| n.as_str()) else { continue };
            let mut items = Vec::new();
            for k in f.get("items").and_then(|i| i.as_array()).into_iter().flatten() {
                if let Some(k) = k.as_str() {
                    if !k.is_empty() && !seen.iter().any(|s| s == k) {
                        seen.push(k.to_string());
                        items.push(k.to_string());
                    }
                }
            }
            out.list.push(Folder { name: clean_name(name), items });
        }
        out
    }

    pub fn to_json(&self) -> String {
        let folders: Vec<serde_json::Value> = self
            .list
            .iter()
            .map(|f| serde_json::json!({ "name": f.name, "items": f.items }))
            .collect();
        let mut s = serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "folders": folders })).unwrap_or_default();
        s.push('\n');
        s
    }

    /// Atomic write: a temporary file next to the target, then a rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)
    }

    /// Index of the folder holding `key`.
    pub fn folder_of(&self, key: &str) -> Option<usize> {
        self.list.iter().position(|f| f.items.iter().any(|k| k == key))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.folder_of(key).is_some()
    }

    fn unique_name(&self, wanted: &str, except: Option<usize>) -> String {
        let base = clean_name(wanted);
        let taken = |n: &str| self.list.iter().enumerate().any(|(i, f)| Some(i) != except && f.name == n);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| {
                let suffix = format!(" {n}");
                let head: String = base.chars().take(NAME_MAX.saturating_sub(suffix.len())).collect();
                format!("{}{suffix}", head.trim_end())
            })
            .find(|c| !taken(c))
            .unwrap_or(base)
    }

    /// Add an empty folder (a clashing name gets " 2", " 3", ...). Returns its index.
    pub fn new_folder(&mut self, name: &str) -> usize {
        let name = self.unique_name(name, None);
        self.list.push(Folder { name, items: Vec::new() });
        self.list.len() - 1
    }

    pub fn rename(&mut self, idx: usize, name: &str) {
        if idx < self.list.len() {
            let n = self.unique_name(name, Some(idx));
            self.list[idx].name = n;
        }
    }

    /// Delete a folder; its apps return to the top level. Returns the keys it held.
    pub fn delete(&mut self, idx: usize) -> Vec<String> {
        if idx < self.list.len() {
            self.list.remove(idx).items
        } else {
            Vec::new()
        }
    }

    /// Move `key` into a folder (appended at the end) or back to the top level. An item is
    /// removed from its old folder first, so it is never in two.
    pub fn move_item(&mut self, key: &str, dest: Dest) {
        if let Dest::Folder(i) = dest {
            if i >= self.list.len() {
                return;
            }
        }
        for f in &mut self.list {
            f.items.retain(|k| k != key);
        }
        if let Dest::Folder(i) = dest {
            self.list[i].items.push(key.to_string());
        }
    }

    /// Replace old keys with what `map` returns for them (a key stays when it gives None), keeping
    /// each key in one place. True if anything changed.
    pub fn migrate_keys(&mut self, map: impl Fn(&str) -> Option<&'static str>) -> bool {
        let mut changed = false;
        let mut seen: Vec<String> = Vec::new();
        for f in &mut self.list {
            let mut items = Vec::with_capacity(f.items.len());
            for k in f.items.drain(..) {
                let new = map(&k).map(str::to_string);
                changed |= new.is_some();
                let k = new.unwrap_or(k);
                if !seen.contains(&k) {
                    seen.push(k.clone());
                    items.push(k);
                } else {
                    changed = true;
                }
            }
            f.items = items;
        }
        changed
    }

    /// Drop keys that no longer exist (uninstalled apps). True if anything changed.
    pub fn prune(&mut self, known: &[String]) -> bool {
        let mut changed = false;
        for f in &mut self.list {
            let before = f.items.len();
            f.items.retain(|k| known.iter().any(|n| n == k));
            changed |= f.items.len() != before;
        }
        changed
    }

    /// The default grouping: Games, Music and Tools, only from the apps that exist. `apps` is
    /// `(key, name)` of every top-level or foldered item. Existing folders with those names are
    /// reused; apps already in some folder stay where they are. Returns how many were moved.
    pub fn auto_organize(&mut self, apps: &[(String, String)]) -> usize {
        let mut moved = 0;
        for (folder, rule) in AUTO_RULES {
            let mut keys = Vec::new();
            for (key, name) in apps {
                if self.contains(key) {
                    continue;
                }
                let n = normalize(name);
                let hit = match rule {
                    Rule::Names(list) => list.iter().any(|l| normalize(l) == n),
                    Rule::Contains(subs) => subs.iter().any(|s| n.contains(s)),
                };
                if hit {
                    keys.push(key.clone());
                }
            }
            if keys.is_empty() {
                continue;
            }
            let idx = match self.list.iter().position(|f| f.name == *folder) {
                Some(i) => i,
                None => self.new_folder(folder),
            };
            for k in keys {
                self.move_item(&k, Dest::Folder(idx));
                moved += 1;
            }
        }
        moved
    }
}

enum Rule {
    Names(&'static [&'static str]),
    Contains(&'static [&'static str]),
}

const AUTO_RULES: &[(&str, Rule)] = &[
    ("Games", Rule::Names(&["Snake", "StuntCopter", "Stunt Copter"])),
    ("Games", Rule::Contains(&["nestopia", "nes", "game"])),
    (
        "Music",
        Rule::Names(&["Music", "Fune", "AcidZero", "ZeroDexed", "MIDI Scope", "uzume", "Piano", "Recorder", "Yacht"]),
    ),
    (
        "Tools",
        Rule::Names(&[
            "Calculator", "Calendar", "Timer", "Sheet", "Text Editor", "Files", "Monitor", "Interceptor", "Devices", "Hardware", "Terminal",
        ]),
    ),
];

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("harbour-folders-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_folders_get_unique_clean_names() {
        let mut f = Folders::default();
        assert_eq!(f.new_folder("  Games "), 0);
        f.new_folder("Games");
        f.new_folder("");
        assert_eq!(f.list.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Games", "Games 2", DEFAULT_NAME]);
        f.rename(1, "Games"); // renaming to its own current clash keeps a unique name
        assert_eq!(f.list[1].name, "Games 2");
        f.rename(0, "Fun");
        assert_eq!(f.list[0].name, "Fun");
        let long = "x".repeat(60);
        f.rename(2, &long);
        assert_eq!(f.list[2].name.chars().count(), NAME_MAX);
    }

    #[test]
    fn moving_keeps_an_item_in_at_most_one_folder_and_appends() {
        let mut f = Folders::default();
        let a = f.new_folder("A");
        let b = f.new_folder("B");
        f.move_item("x", Dest::Folder(a));
        f.move_item("y", Dest::Folder(a));
        f.move_item("x", Dest::Folder(b));
        assert_eq!(f.list[a].items, keys(&["y"]));
        assert_eq!(f.list[b].items, keys(&["x"]));
        f.move_item("y", Dest::Folder(b));
        assert_eq!(f.list[b].items, keys(&["x", "y"]), "new apps go to the end");
        f.move_item("x", Dest::Top);
        assert!(!f.contains("x"));
        f.move_item("z", Dest::Folder(9));
        assert!(!f.contains("z"), "a missing folder is ignored");
        assert_eq!(f.folder_of("y"), Some(b));
    }

    #[test]
    fn deleting_a_folder_returns_its_apps_to_the_top_level() {
        let mut f = Folders::default();
        let a = f.new_folder("A");
        f.move_item("x", Dest::Folder(a));
        assert_eq!(f.delete(a), keys(&["x"]));
        assert!(f.is_empty() && !f.contains("x"));
        assert!(f.delete(3).is_empty());
    }

    #[test]
    fn persistence_round_trips_atomically() {
        let dir = tmp("rt");
        let path = dir.join("sub/folders.json");
        let mut f = Folders::default();
        let a = f.new_folder("Games");
        f.new_folder("Empty");
        f.move_item("Snake", Dest::Folder(a));
        f.move_item("nes.desktop", Dest::Folder(a));
        f.save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists(), "the temp file is renamed away");
        assert_eq!(Folders::load(&path), f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_corrupt_file_means_no_folders() {
        let dir = tmp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Folders::load(&dir.join("none.json")).is_empty());
        for junk in ["", "not json", "[1,2]", "{\"folders\": 5}", "{\"folders\":[{\"items\":[\"a\"]}]}"] {
            assert!(Folders::parse(junk).is_empty(), "{junk:?}");
        }
        let p = dir.join("f.json");
        std::fs::write(&p, "{ broken").unwrap();
        assert!(Folders::load(&p).is_empty());
        // Wrong element types are skipped, the rest survives.
        let f = Folders::parse(r#"{"folders":[{"name":"A","items":["x",3,null,"y"]},7]}"#);
        assert_eq!(f.list.len(), 1);
        assert_eq!(f.list[0].items, keys(&["x", "y"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicates_in_the_file_are_dropped_keeping_the_first() {
        let f = Folders::parse(r#"{"folders":[{"name":"A","items":["x","x","y"]},{"name":"B","items":["y","z"]}]}"#);
        assert_eq!(f.list[0].items, keys(&["x", "y"]));
        assert_eq!(f.list[1].items, keys(&["z"]));
    }

    #[test]
    fn uninstalled_apps_are_dropped() {
        let mut f = Folders::parse(r#"{"folders":[{"name":"A","items":["x","y","z"]}]}"#);
        assert!(f.prune(&keys(&["x", "z", "other"])));
        assert_eq!(f.list[0].items, keys(&["x", "z"]), "the user's order is kept");
        assert!(!f.prune(&keys(&["x", "z"])));
        assert_eq!(f.list.len(), 1, "an emptied folder stays");
    }

    fn apps(names: &[&str]) -> Vec<(String, String)> {
        names.iter().map(|n| (n.to_string(), n.to_string())).collect()
    }

    #[test]
    fn auto_organize_only_uses_apps_that_exist() {
        let mut f = Folders::default();
        let n = f.auto_organize(&apps(&["Settings", "Snake", "Nestopia", "Piano", "Calculator", "Weather", "Gameboy"]));
        assert_eq!(n, 5);
        let by = |name: &str| f.list.iter().find(|x| x.name == name).map(|x| x.items.clone());
        assert_eq!(by("Games"), Some(keys(&["Snake", "Nestopia", "Gameboy"])));
        assert_eq!(by("Music"), Some(keys(&["Piano"])));
        assert_eq!(by("Tools"), Some(keys(&["Calculator"])));
        assert!(!f.contains("Settings") && !f.contains("Weather"));
        // No matching app = no empty folder; a second run changes nothing.
        let mut g = Folders::default();
        assert_eq!(g.auto_organize(&apps(&["Settings"])), 0);
        assert!(g.is_empty());
        assert_eq!(f.auto_organize(&apps(&["Snake", "Piano"])), 0);
    }

    #[test]
    fn auto_organize_leaves_apps_the_user_already_filed() {
        let mut f = Folders::default();
        let mine = f.new_folder("Mine");
        f.move_item("Piano", Dest::Folder(mine));
        f.auto_organize(&apps(&["Piano", "Fune"]));
        assert_eq!(f.folder_of("Piano"), Some(mine));
        assert_eq!(f.list.iter().find(|x| x.name == "Music").unwrap().items, keys(&["Fune"]));
    }
}
