//! File operations shared by Files, Text Editor, Sheet and Images.
//!
//! Plain `std::fs`, no UI. Rules (see docs/DESIGN.md "File operations"):
//! * Nothing here ever overwrites: a name clash gets a free name (`a copy.txt`, `a copy 2.txt`).
//! * Names are one path component: no `/`, no `.` / `..`, no control characters, at most 255 bytes.
//! * Symbolic links are never followed: copying a link makes a link, deleting a link removes the link.
//! * `/` and `$HOME` cannot be renamed, moved or deleted; a folder cannot go into itself.
//! * "Delete" moves to the Trash (`~/.local/share/Trash`, with a `.trashinfo`); only things already in
//!   the Trash are deleted for good (with their `.trashinfo`). Restore reads `Path=` back.
//! * Default save folders come from [`user_dir`] (XDG user dirs, else `~/Documents` etc.).

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub const NAME_MAX_BYTES: usize = 255;

/// What an app can do with a file, from its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenKind {
    Text,
    Image,
    Sheet,
    Music,
}

impl OpenKind {
    /// The registered / built-in app that opens this kind.
    pub fn app_name(self) -> &'static str {
        match self {
            OpenKind::Text => "Text Editor",
            OpenKind::Image => "Images",
            OpenKind::Sheet => "Sheet",
            OpenKind::Music => "Music",
        }
    }
}

pub const TEXT_EXT: [&str; 6] = ["txt", "md", "log", "text", "conf", "ini"];
pub const SHEET_EXT: [&str; 1] = ["csv"];
/// Largest file the Text Editor takes.
pub const TEXT_MAX_BYTES: u64 = 512 * 1024;

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn open_kind(p: &Path) -> Option<OpenKind> {
    let e = ext_of(p);
    let e = e.as_str();
    if TEXT_EXT.contains(&e) {
        Some(OpenKind::Text)
    } else if crate::launcher::apps::imageviewer::EXTENSIONS.contains(&e) {
        Some(OpenKind::Image)
    } else if SHEET_EXT.contains(&e) {
        Some(OpenKind::Sheet)
    } else if crate::launcher::apps::music::EXTENSIONS.contains(&e) {
        Some(OpenKind::Music)
    } else {
        None
    }
}

/// The reason a toast gives when no app opens a file.
pub fn cannot_open_reason(p: &Path) -> String {
    match ext_of(p).as_str() {
        "" => "No app opens files without an extension".into(),
        e => format!("No app opens .{e} files"),
    }
}

// ------------------------------------------------------------------------------ names and paths

/// A name for one file or folder, or why it is refused.
pub fn valid_name(name: &str) -> Result<&str, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Type a name".into());
    }
    if n == "." || n == ".." {
        return Err("That name is not allowed".into());
    }
    if n.chars().any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control()) {
        return Err("A name cannot contain / or control characters".into());
    }
    if n.len() > NAME_MAX_BYTES {
        return Err("That name is too long".into());
    }
    Ok(n)
}

/// `dir/name` for a validated name (never leaves `dir`).
pub fn join_safe(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let n = valid_name(name)?;
    let p = dir.join(n);
    debug_assert_eq!(p.parent(), Some(dir));
    Ok(p)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// Refuse to touch `/`, `$HOME` and anything without a file name (`..`, `.`).
fn guard(p: &Path) -> io::Result<()> {
    let bad = |m: &str| Err(io::Error::new(io::ErrorKind::PermissionDenied, m.to_string()));
    if p.file_name().is_none() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return bad("That item cannot be changed");
    }
    if Some(p) == home().as_deref() {
        return bad("The home folder cannot be changed");
    }
    Ok(())
}

/// A free name in `dir`: `name`, else `stem copy.ext`, `stem copy 2.ext`, ...
pub fn unique_name(dir: &Path, name: &str) -> String {
    let exists = |n: &str| fs::symlink_metadata(dir.join(n)).is_ok();
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1.. {
        let cand = if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") };
        if !exists(&cand) {
            return cand;
        }
    }
    unreachable!()
}

// ------------------------------------------------------------------------------ operations

pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let p = join_safe(dir, name)?;
    if fs::symlink_metadata(&p).is_ok() {
        return Err(format!("\"{}\" already exists", valid_name(name)?));
    }
    fs::create_dir(&p).map_err(err)?;
    Ok(p)
}

/// A new empty file (refuses an existing one).
pub fn new_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let p = join_safe(dir, name)?;
    fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists { format!("\"{}\" already exists", valid_name(name).unwrap_or("")) } else { err(e) }
    })?;
    Ok(p)
}

/// Rename inside the same folder. Refuses a name that is taken (no overwrite).
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    guard(path).map_err(err)?;
    let dir = path.parent().ok_or("No folder")?;
    let target = join_safe(dir, new_name)?;
    if target == path {
        return Ok(target);
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!("\"{}\" already exists", valid_name(new_name)?));
    }
    fs::rename(path, &target).map_err(err)?;
    Ok(target)
}

fn is_dir_nofollow(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// Copy a file, a folder tree or a link to `dest_dir` under a free name. Returns the new path.
pub fn copy_to(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let name = src.file_name().ok_or("Nothing to copy")?.to_string_lossy().into_owned();
    if is_dir_nofollow(src) && inside(dest_dir, src) {
        return Err("A folder cannot be copied into itself".into());
    }
    let target = dest_dir.join(unique_name(dest_dir, &name));
    copy_rec(src, &target).map_err(|e| {
        let _ = remove_any(&target);
        err(e)
    })?;
    Ok(target)
}

fn copy_rec(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        let to = fs::read_link(src)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(to, dst)?;
        #[cfg(not(unix))]
        let _ = to;
    } else if md.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_rec(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// `p` is `root` or lies under it (lexically, after resolving what exists).
fn inside(p: &Path, root: &Path) -> bool {
    let p = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    p.starts_with(&root)
}

/// Move to `dest_dir` under a free name (rename, or copy + delete across file systems).
pub fn move_to(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    guard(src).map_err(err)?;
    let name = src.file_name().ok_or("Nothing to move")?.to_string_lossy().into_owned();
    if is_dir_nofollow(src) && inside(dest_dir, src) {
        return Err("A folder cannot be moved into itself".into());
    }
    if src.parent() == Some(dest_dir) {
        return Ok(src.to_path_buf()); // already there
    }
    let target = dest_dir.join(unique_name(dest_dir, &name));
    match fs::rename(src, &target) {
        Ok(()) => Ok(target),
        Err(_) => {
            // Another file system (or a refusal): copy, then remove the original only if the copy is whole.
            copy_rec(src, &target).map_err(|e| {
                let _ = remove_any(&target);
                err(e)
            })?;
            remove_any(src).map_err(err)?;
            Ok(target)
        }
    }
}

/// Delete for good. A link is removed, never what it points to.
pub fn remove_any(p: &Path) -> io::Result<()> {
    guard(p)?;
    if is_dir_nofollow(p) {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

pub fn trash_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| home().map(|h| h.join(".local/share")))?;
    Some(base.join("Trash"))
}

/// True when `p` is the Trash folder or something inside it.
pub fn in_trash(p: &Path, trash: &Path) -> bool {
    p.starts_with(trash)
}

/// Move to `trash/files` (with `trash/info/<name>.trashinfo`). Returns where it went.
pub fn trash(path: &Path, trash: &Path) -> Result<PathBuf, String> {
    guard(path).map_err(err)?;
    if path.starts_with(trash) {
        return Err("Already in the Trash".into());
    }
    let files = trash.join("files");
    let info = trash.join("info");
    fs::create_dir_all(&files).map_err(err)?;
    fs::create_dir_all(&info).map_err(err)?;
    let name = path.file_name().ok_or("Nothing to delete")?.to_string_lossy().into_owned();
    // The name must be free in files/ and its .trashinfo free in info/.
    let mut cand = unique_name(&files, &name);
    while fs::symlink_metadata(info.join(format!("{cand}.trashinfo"))).is_ok() {
        cand = unique_name(&files, &format!("{cand}x"));
    }
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf()) };
    let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let text = format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", percent_encode(&abs.to_string_lossy()));
    fs::write(info.join(format!("{cand}.trashinfo")), text).map_err(err)?;
    match move_to_exact(path, &files.join(&cand)) {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = fs::remove_file(info.join(format!("{cand}.trashinfo")));
            Err(e)
        }
    }
}

/// `trash/info/<name>.trashinfo` for an item directly in `trash/files`, else `None`.
fn info_file(item: &Path, trash: &Path) -> Option<PathBuf> {
    if item.parent() != Some(trash.join("files").as_path()) {
        return None;
    }
    let name = item.file_name()?.to_string_lossy().into_owned();
    Some(trash.join("info").join(format!("{name}.trashinfo")))
}

/// Where a trashed item came from: the `Path=` line of its `.trashinfo`, percent-decoded.
/// A relative path is relative to the folder that holds the Trash (the freedesktop rule).
pub fn trashed_from(item: &Path, trash: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(info_file(item, trash)?).ok()?;
    let raw = text.lines().map(str::trim).find_map(|l| l.strip_prefix("Path="))?;
    let p = PathBuf::from(percent_decode(raw.trim()));
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(if p.is_absolute() { p } else { trash.parent()?.join(p) })
}

/// Delete something in the Trash for good; an item directly in `trash/files` loses its
/// `.trashinfo` too.
pub fn delete_from_trash(item: &Path, trash: &Path) -> Result<(), String> {
    remove_any(item).map_err(err)?;
    if let Some(info) = info_file(item, trash) {
        let _ = fs::remove_file(info);
    }
    Ok(())
}

/// Put a trashed item back where it came from. Never overwrites: a clash gets a free name.
/// A missing original folder is made again. Returns where it went.
pub fn restore(item: &Path, trash: &Path) -> Result<PathBuf, String> {
    let info = info_file(item, trash).ok_or("Only things at the top of the Trash can be restored")?;
    let from = trashed_from(item, trash).ok_or("Where it came from is not known")?;
    let dir = from.parent().ok_or("Where it came from is not known")?;
    let name = from.file_name().ok_or("Where it came from is not known")?.to_string_lossy().into_owned();
    if fs::symlink_metadata(item).is_err() {
        return Err("It is gone".into());
    }
    let target = dir.join(unique_name(dir, &name));
    guard(&target).map_err(err)?;
    fs::create_dir_all(dir).map_err(err)?;
    let p = move_to_exact(item, &target)?;
    let _ = fs::remove_file(info);
    Ok(p)
}

/// Delete everything in the Trash for good (items and their `.trashinfo` files). Returns how many
/// items went. Stops at the first failure.
pub fn empty_trash(trash: &Path) -> Result<usize, String> {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(trash.join("files")) {
        for e in rd.flatten() {
            remove_any(&e.path()).map_err(err)?;
            n += 1;
        }
    }
    if let Ok(rd) = fs::read_dir(trash.join("info")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(".trashinfo") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    Ok(n)
}

fn move_to_exact(src: &Path, target: &Path) -> Result<PathBuf, String> {
    if fs::rename(src, target).is_ok() {
        return Ok(target.to_path_buf());
    }
    copy_rec(src, target).map_err(|e| {
        let _ = remove_any(target);
        err(e)
    })?;
    remove_any(src).map_err(err)?;
    Ok(target.to_path_buf())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------------------ default folders

/// The user's standard folders, where apps save by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserDir {
    Documents,
    Pictures,
    Music,
}

impl UserDir {
    fn key(self) -> &'static str {
        match self {
            UserDir::Documents => "XDG_DOCUMENTS_DIR",
            UserDir::Pictures => "XDG_PICTURES_DIR",
            UserDir::Music => "XDG_MUSIC_DIR",
        }
    }

    fn fallback(self) -> &'static str {
        match self {
            UserDir::Documents => "Documents",
            UserDir::Pictures => "Pictures",
            UserDir::Music => "Music",
        }
    }
}

/// The folder for `kind`: `XDG_*_DIR` from `~/.config/user-dirs.dirs` (or `$XDG_CONFIG_HOME/...`)
/// when it is set there, else `~/Documents`, `~/Pictures`, `~/Music`. The folder may not exist yet.
/// A path for people: the home folder as `~` (`/home/pi/Pictures` -> `~/Pictures`).
pub fn tilde(p: &Path) -> String {
    match home().and_then(|h| p.strip_prefix(&h).ok().map(|r| r.to_path_buf())) {
        Some(r) if r.as_os_str().is_empty() => "~".into(),
        Some(r) => format!("~/{}", r.display()),
        None => p.display().to_string(),
    }
}

pub fn user_dir(kind: UserDir) -> PathBuf {
    let Some(h) = home() else { return PathBuf::from(kind.fallback()) };
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| h.join(".config"));
    let text = fs::read_to_string(config.join("user-dirs.dirs")).ok();
    user_dir_from(kind, &h, text.as_deref())
}

/// [`user_dir`] from the text of a `user-dirs.dirs` file. Values are `"$HOME/..."` or absolute
/// paths; one that is just `$HOME` means "not set" (the freedesktop rule), so the default is used.
pub fn user_dir_from(kind: UserDir, home: &Path, user_dirs: Option<&str>) -> PathBuf {
    let set = user_dirs.and_then(|t| {
        t.lines().map(str::trim).filter(|l| !l.starts_with('#')).find_map(|l| {
            let v = l.strip_prefix(kind.key())?.trim_start().strip_prefix('=')?.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            let p = if v == "$HOME" || v == "$HOME/" {
                return None;
            } else if let Some(rest) = v.strip_prefix("$HOME/") {
                home.join(rest.trim_end_matches('/'))
            } else if v.starts_with('/') {
                PathBuf::from(v)
            } else {
                return None;
            };
            (p != home).then_some(p)
        })
    });
    set.unwrap_or_else(|| home.join(kind.fallback()))
}

/// The Text Editor's default note: `new` (Documents/note.txt), except when only the `old` one
/// (from before notes lived in Documents) exists, so no note is lost.
pub fn default_note(old: &Path, new: &Path) -> PathBuf {
    if old.is_file() && !new.exists() { old.to_path_buf() } else { new.to_path_buf() }
}

// ------------------------------------------------------------------------------ info

/// What Files' Info card shows about one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    pub name: String,
    /// "Folder", "Link", "Text file"... (English; translate when shown).
    pub kind: &'static str,
    /// Bytes; for a folder, the sum of the files in it (links not followed).
    pub size: u64,
    /// For a folder: the walk stopped at [`INFO_MAX_ENTRIES`], so `size` is a lower bound.
    pub size_capped: bool,
    pub modified: Option<std::time::SystemTime>,
    /// Unix permission bits (`0o755`), 0 where unknown.
    pub mode: u32,
    pub path: PathBuf,
}

/// The most entries Info walks to sum a folder's size (keeps the card quick on a big tree).
pub const INFO_MAX_ENTRIES: usize = 20_000;

/// Facts about `p` (not following a link at `p`).
pub fn item_info(p: &Path) -> Result<ItemInfo, String> {
    let md = fs::symlink_metadata(p).map_err(err)?;
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        "Link"
    } else if ft.is_dir() {
        "Folder"
    } else {
        match open_kind(p) {
            Some(OpenKind::Text) => "Text file",
            Some(OpenKind::Image) => "Picture",
            Some(OpenKind::Sheet) => "Spreadsheet",
            Some(OpenKind::Music) => "Audio",
            None => "File",
        }
    };
    let (size, size_capped) = if ft.is_dir() { dir_size(p, INFO_MAX_ENTRIES) } else { (md.len(), false) };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = 0;
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf()) };
    Ok(ItemInfo {
        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| abs.to_string_lossy().into_owned()),
        kind,
        size,
        size_capped,
        modified: md.modified().ok(),
        mode,
        path: abs,
    })
}

/// Sum of file sizes under `dir`, visiting at most `max` entries; `true` when it stopped early.
/// Links are counted as links, never followed.
pub fn dir_size(dir: &Path, max: usize) -> (u64, bool) {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > max {
                return (total, true);
            }
            let Ok(md) = fs::symlink_metadata(e.path()) else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else {
                total += md.len();
            }
        }
    }
    (total, false)
}

/// `0o755` -> "rwxr-xr-x".
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn err(e: io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => format!("Not allowed: {e}"),
        io::ErrorKind::NotFound => "It is gone".into(),
        _ => e.to_string(),
    }
}

/// Read a file the Text Editor can show: UTF-8 text up to [`TEXT_MAX_BYTES`].
pub fn read_text(path: &Path) -> Result<String, String> {
    let md = fs::metadata(path).map_err(err)?;
    if !md.is_file() {
        return Err("Not a file".into());
    }
    if md.len() > TEXT_MAX_BYTES {
        return Err(format!("Too big for the Text Editor ({} KB max)", TEXT_MAX_BYTES / 1024));
    }
    let bytes = fs::read(path).map_err(err)?;
    String::from_utf8(bytes).map_err(|_| "Not a text file (not UTF-8)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("harbour-fileops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn names_are_one_safe_component() {
        assert_eq!(valid_name("  a.txt "), Ok("a.txt"));
        assert_eq!(valid_name("日本語のファイル.txt"), Ok("日本語のファイル.txt"));
        for bad in ["", "  ", ".", "..", "a/b", "../x", "a\\b", "a\nb", "a\0b"] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
        assert!(valid_name(&"x".repeat(256)).is_err());
        assert!(valid_name(&"あ".repeat(86)).is_err(), "255 bytes, not characters");
        let d = Path::new("/tmp/x");
        assert!(join_safe(d, "../etc").is_err());
        assert_eq!(join_safe(d, "ok").unwrap(), d.join("ok"));
    }

    #[test]
    fn free_names_keep_the_extension() {
        let d = tmp("uniq");
        assert_eq!(unique_name(&d, "a.txt"), "a.txt");
        fs::write(d.join("a.txt"), "1").unwrap();
        assert_eq!(unique_name(&d, "a.txt"), "a copy.txt");
        fs::write(d.join("a copy.txt"), "1").unwrap();
        assert_eq!(unique_name(&d, "a.txt"), "a copy 2.txt");
        fs::write(d.join(".rc"), "").unwrap();
        assert_eq!(unique_name(&d, ".rc"), ".rc copy", "a leading dot is not an extension");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn new_folder_and_new_file_refuse_clashes() {
        let d = tmp("new");
        let f = new_folder(&d, "Docs").unwrap();
        assert!(f.is_dir());
        assert!(new_folder(&d, "Docs").unwrap_err().contains("already exists"));
        assert!(new_folder(&d, "a/b").is_err());
        let n = new_file(&d, "n.txt").unwrap();
        fs::write(&n, "keep").unwrap();
        assert!(new_file(&d, "n.txt").is_err());
        assert_eq!(fs::read_to_string(&n).unwrap(), "keep", "a clash never truncates");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_never_overwrites_and_stays_in_the_folder() {
        let d = tmp("rename");
        fs::write(d.join("a.txt"), "A").unwrap();
        fs::write(d.join("b.txt"), "B").unwrap();
        assert!(rename(&d.join("a.txt"), "b.txt").unwrap_err().contains("already exists"));
        assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "B");
        assert!(rename(&d.join("a.txt"), "../escape.txt").is_err());
        assert!(!d.parent().unwrap().join("escape.txt").exists());
        let p = rename(&d.join("a.txt"), "ノート.txt").unwrap();
        assert_eq!(p, d.join("ノート.txt"));
        assert!(!d.join("a.txt").exists());
        assert_eq!(rename(&p, "ノート.txt").unwrap(), p, "same name is a no-op");
        assert!(rename(Path::new("/"), "x").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_makes_a_free_name_and_copies_folders() {
        let d = tmp("copy");
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/sub/x.txt"), "x").unwrap();
        fs::write(d.join("a.txt"), "A").unwrap();
        let c = copy_to(&d.join("a.txt"), &d).unwrap();
        assert_eq!(c, d.join("a copy.txt"));
        assert_eq!(fs::read_to_string(&c).unwrap(), "A");
        let c2 = copy_to(&d.join("a.txt"), &d).unwrap();
        assert_eq!(c2, d.join("a copy 2.txt"));
        fs::create_dir(d.join("dst")).unwrap();
        let t = copy_to(&d.join("src"), &d.join("dst")).unwrap();
        assert_eq!(fs::read_to_string(t.join("sub/x.txt")).unwrap(), "x");
        assert!(d.join("src/sub/x.txt").exists(), "the original stays");
        assert!(copy_to(&d.join("src"), &d.join("src/sub")).unwrap_err().contains("into itself"));
        assert!(copy_to(&d.join("src"), &d.join("src")).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn move_renames_and_refuses_going_into_itself() {
        let d = tmp("move");
        fs::create_dir_all(d.join("a/inner")).unwrap();
        fs::create_dir(d.join("dst")).unwrap();
        fs::write(d.join("f.txt"), "F").unwrap();
        fs::write(d.join("dst/f.txt"), "OTHER").unwrap();
        let m = move_to(&d.join("f.txt"), &d.join("dst")).unwrap();
        assert_eq!(m, d.join("dst/f copy.txt"), "a clash gets a free name");
        assert_eq!(fs::read_to_string(d.join("dst/f.txt")).unwrap(), "OTHER", "the existing file is untouched");
        assert!(!d.join("f.txt").exists());
        assert!(move_to(&d.join("a"), &d.join("a/inner")).unwrap_err().contains("into itself"));
        assert!(d.join("a/inner").is_dir());
        assert_eq!(move_to(&d.join("dst/f.txt"), &d.join("dst")).unwrap(), d.join("dst/f.txt"), "same folder: nothing to do");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn trash_keeps_the_item_and_a_trashinfo() {
        let d = tmp("trash");
        let t = d.join("Trash");
        fs::write(d.join("a.txt"), "A").unwrap();
        let p = trash(&d.join("a.txt"), &t).unwrap();
        assert_eq!(p, t.join("files/a.txt"));
        assert!(!d.join("a.txt").exists());
        let info = fs::read_to_string(t.join("info/a.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=") && info.contains("a.txt") && info.contains("DeletionDate="));
        // Same name again: both survive.
        fs::write(d.join("a.txt"), "A2").unwrap();
        let p2 = trash(&d.join("a.txt"), &t).unwrap();
        assert_eq!(p2, t.join("files/a copy.txt"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "A");
        assert!(trash(&p, &t).is_err(), "things in the Trash are deleted for good, not trashed again");
        assert!(in_trash(&p, &t) && !in_trash(&d.join("x"), &t));
        // Japanese names are percent-encoded in the info file.
        fs::write(d.join("日本.txt"), "").unwrap();
        trash(&d.join("日本.txt"), &t).unwrap();
        let info = fs::read_to_string(t.join("info/日本.txt.trashinfo")).unwrap();
        assert!(info.contains("%E6%97%A5%E6%9C%AC.txt"), "{info}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn percent_decoding_undoes_the_encoding() {
        for s in ["/home/a b/日本.txt", "/x/100%/y", "/plain"] {
            assert_eq!(percent_decode(&percent_encode(s)), s);
        }
        assert_eq!(percent_decode("/a%20b"), "/a b");
        assert_eq!(percent_decode("/bad%zz%2"), "/bad%zz%2", "broken escapes stay as they are");
    }

    #[test]
    fn restore_puts_it_back_without_overwriting() {
        let d = tmp("restore");
        let t = d.join("Trash");
        fs::create_dir(d.join("docs")).unwrap();
        fs::write(d.join("docs/日本 語.txt"), "first").unwrap();
        let p = trash(&d.join("docs/日本 語.txt"), &t).unwrap();
        assert_eq!(trashed_from(&p, &t).unwrap(), d.join("docs/日本 語.txt"));
        let back = restore(&p, &t).unwrap();
        assert_eq!(back, d.join("docs/日本 語.txt"));
        assert_eq!(fs::read_to_string(&back).unwrap(), "first");
        assert!(!p.exists() && !t.join("info/日本 語.txt.trashinfo").exists(), "item and info both leave the Trash");
        // A clash at home gets a free name; the existing file is untouched.
        let p = trash(&back, &t).unwrap();
        fs::write(d.join("docs/日本 語.txt"), "newer").unwrap();
        let back = restore(&p, &t).unwrap();
        assert_eq!(back, d.join("docs/日本 語 copy.txt"));
        assert_eq!(fs::read_to_string(d.join("docs/日本 語.txt")).unwrap(), "newer");
        // The original folder is gone: it is made again.
        fs::create_dir_all(d.join("gone/sub")).unwrap();
        fs::write(d.join("gone/sub/x.txt"), "x").unwrap();
        let p = trash(&d.join("gone/sub/x.txt"), &t).unwrap();
        fs::remove_dir_all(d.join("gone")).unwrap();
        assert_eq!(restore(&p, &t).unwrap(), d.join("gone/sub/x.txt"));
        // No .trashinfo: refused, nothing moves.
        fs::write(t.join("files/stray"), "s").unwrap();
        assert!(restore(&t.join("files/stray"), &t).is_err());
        assert!(t.join("files/stray").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn deleting_in_the_trash_and_emptying_it_remove_the_info_files() {
        let d = tmp("empty");
        let t = d.join("Trash");
        fs::write(d.join("a.txt"), "A").unwrap();
        fs::create_dir_all(d.join("dir/in")).unwrap();
        fs::write(d.join("dir/in/b"), "B").unwrap();
        let a = trash(&d.join("a.txt"), &t).unwrap();
        delete_from_trash(&a, &t).unwrap();
        assert!(!a.exists() && !t.join("info/a.txt.trashinfo").exists());
        // Deleting inside a trashed folder leaves the folder's info alone.
        let dir = trash(&d.join("dir"), &t).unwrap();
        delete_from_trash(&dir.join("in/b"), &t).unwrap();
        assert!(t.join("info/dir.trashinfo").exists());
        fs::write(d.join("c"), "C").unwrap();
        trash(&d.join("c"), &t).unwrap();
        assert_eq!(empty_trash(&t).unwrap(), 2);
        assert_eq!(fs::read_dir(t.join("files")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(t.join("info")).unwrap().count(), 0);
        assert_eq!(empty_trash(&d.join("NoTrash")).unwrap(), 0, "no Trash yet is not an error");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn user_dirs_honour_the_xdg_file() {
        let h = Path::new("/home/u");
        assert_eq!(user_dir_from(UserDir::Documents, h, None), h.join("Documents"));
        let text = "# comment\nXDG_DOCUMENTS_DIR=\"$HOME/書類\"\nXDG_PICTURES_DIR=\"/data/pics/\"\nXDG_MUSIC_DIR=\"$HOME/\"\n";
        assert_eq!(user_dir_from(UserDir::Documents, h, Some(text)), h.join("書類"));
        assert_eq!(user_dir_from(UserDir::Pictures, h, Some(text)), PathBuf::from("/data/pics/"));
        assert_eq!(user_dir_from(UserDir::Music, h, Some(text)), h.join("Music"), "$HOME alone means not set");
        assert_eq!(user_dir_from(UserDir::Music, h, Some("XDG_MUSIC_DIR=relative")), h.join("Music"));
        assert_eq!(user_dir_from(UserDir::Pictures, h, Some("XDG_PICTURES_DIR_X=\"/no\"")), h.join("Pictures"));
    }

    #[test]
    fn the_old_note_is_kept_until_a_new_one_exists() {
        let d = tmp("note");
        let (old, new) = (d.join("data/note.txt"), d.join("Documents/note.txt"));
        assert_eq!(default_note(&old, &new), new, "fresh start: Documents");
        fs::create_dir_all(d.join("data")).unwrap();
        fs::write(&old, "old notes").unwrap();
        assert_eq!(default_note(&old, &new), old, "only the old note: keep it");
        fs::create_dir_all(d.join("Documents")).unwrap();
        fs::write(&new, "new").unwrap();
        assert_eq!(default_note(&old, &new), new, "both: the new one");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn info_sums_folders_and_describes_items() {
        let d = tmp("info");
        fs::create_dir_all(d.join("f/g")).unwrap();
        fs::write(d.join("f/a.txt"), vec![b'a'; 100]).unwrap();
        fs::write(d.join("f/g/b.png"), vec![b'b'; 50]).unwrap();
        let i = item_info(&d.join("f")).unwrap();
        assert_eq!((i.kind, i.size, i.size_capped, i.name.as_str()), ("Folder", 150, false, "f"));
        assert_eq!(i.path, d.join("f"));
        let (_, capped) = dir_size(&d.join("f"), 2);
        assert!(capped, "the walk stops at the cap");
        let i = item_info(&d.join("f/a.txt")).unwrap();
        assert_eq!((i.kind, i.size), ("Text file", 100));
        assert!(i.modified.is_some());
        assert_eq!(item_info(&d.join("f/g/b.png")).unwrap().kind, "Picture");
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o640), "rw-r-----");
        assert!(item_info(&d.join("nope")).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn protected_paths_cannot_be_changed() {
        assert!(remove_any(Path::new("/")).is_err());
        assert!(remove_any(Path::new("/tmp/x/..")).is_err());
        assert!(trash(Path::new("/"), Path::new("/tmp/never-used-trash")).is_err());
        assert!(move_to(Path::new("/"), Path::new("/tmp")).is_err());
        if let Some(h) = home() {
            assert!(remove_any(&h).is_err(), "never the home folder");
            assert!(rename(&h, "x").is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed() {
        use std::os::unix::fs::symlink;
        let d = tmp("links");
        fs::create_dir_all(d.join("real")).unwrap();
        fs::write(d.join("real/keep.txt"), "keep").unwrap();
        symlink(d.join("real"), d.join("link")).unwrap();
        // Deleting the link leaves the target alone.
        remove_any(&d.join("link")).unwrap();
        assert!(d.join("real/keep.txt").exists());
        // Trashing a link moves the link only.
        symlink(d.join("real"), d.join("link2")).unwrap();
        let t = d.join("Trash");
        trash(&d.join("link2"), &t).unwrap();
        assert!(d.join("real/keep.txt").exists());
        assert!(fs::symlink_metadata(t.join("files/link2")).unwrap().file_type().is_symlink());
        // Copying a folder with a link inside copies the link, not the target's contents.
        symlink("/etc", d.join("real/etc-link")).unwrap();
        let c = copy_to(&d.join("real"), &d).unwrap();
        assert!(fs::symlink_metadata(c.join("etc-link")).unwrap().file_type().is_symlink());
        // A link back to an ancestor does not loop.
        symlink(&d, d.join("real/up")).unwrap();
        let c2 = copy_to(&d.join("real"), &d).unwrap();
        assert!(fs::symlink_metadata(c2.join("up")).unwrap().file_type().is_symlink());
        // A link that points at a folder counts as a link, so it can go "into itself" only as a link.
        symlink(d.join("real"), d.join("self")).unwrap();
        assert!(copy_to(&d.join("self"), &d).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn kinds_follow_the_extension() {
        let k = |s: &str| open_kind(Path::new(s));
        assert_eq!(k("a.TXT"), Some(OpenKind::Text));
        assert_eq!(k("a.md"), Some(OpenKind::Text));
        assert_eq!(k("a.log"), Some(OpenKind::Text));
        assert_eq!(k("a.png"), Some(OpenKind::Image));
        assert_eq!(k("a.JPG"), Some(OpenKind::Image));
        assert_eq!(k("a.csv"), Some(OpenKind::Sheet));
        assert_eq!(k("a.mp3"), Some(OpenKind::Music));
        assert_eq!(k("a.wav"), Some(OpenKind::Music));
        assert_eq!(k("a.bin"), None);
        assert_eq!(k("README"), None);
        assert!(cannot_open_reason(Path::new("a.bin")).contains(".bin"));
        assert!(cannot_open_reason(Path::new("README")).contains("extension"));
    }

    #[test]
    fn text_reading_refuses_binary_and_huge_files() {
        let d = tmp("text");
        fs::write(d.join("ok.txt"), "こんにちは").unwrap();
        assert_eq!(read_text(&d.join("ok.txt")).unwrap(), "こんにちは");
        fs::write(d.join("bin.txt"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(read_text(&d.join("bin.txt")).unwrap_err().contains("UTF-8"));
        fs::write(d.join("big.txt"), vec![b'a'; (TEXT_MAX_BYTES + 1) as usize]).unwrap();
        assert!(read_text(&d.join("big.txt")).unwrap_err().contains("Too big"));
        assert!(read_text(&d).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
