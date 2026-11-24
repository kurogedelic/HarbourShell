//! Is there a newer Harbour Shell? Once a day (and when Settings asks) the latest release's
//! version is read from GitHub and compared with this build's. A newer one is announced once with
//! a toast and stays as a row in Settings > System, which installs it (see
//! [`installer::Job::Harbour`](crate::launcher::installer::Job)).
//!
//! What goes out: one request for the public "latest release" record of the Harbour Shell
//! repository, with a generic client name and nothing about this device or its user. It is not
//! made at all when "Check for new versions" is off, in mock runs, or in the web build.
//!
//! `HARBOUR_UPDATE_LATEST=1.2.3` stands in for the answer (tests, demos); it works in mock runs too.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The repository the releases come from.
pub const REPO: &str = "kurogedelic/HarbourShell";
/// Seconds between two automatic checks.
pub const EVERY: i64 = 24 * 3600;
/// The answer is at most this big (the record is a few kilobytes).
const LIMIT: usize = 512 * 1024;

/// This build's version.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "1.2.3" (or "v1.2.3") as numbers; None for anything else (pre-releases, junk).
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let v = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some(v)
}

/// Is `latest` a later version than `current`?
pub fn newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// The version in GitHub's "latest release" record (`"tag_name": "v1.2.3"`), without the `v`.
pub fn latest_from_json(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let (a, b, c) = parse_version(tag)?;
    Some(format!("{a}.{b}.{c}"))
}

/// What is remembered between runs (`update.txt` in Harbour's data folder).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// When the last check finished (Unix seconds; 0 = never).
    pub checked: i64,
    /// The latest release seen.
    pub latest: Option<String>,
    /// The version the user has already been told about (one toast per version).
    pub told: Option<String>,
}

impl State {
    pub fn parse(text: &str) -> State {
        let mut s = State::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "checked" => s.checked = v.parse().unwrap_or(0),
                "latest" => s.latest = parse_version(v).map(|_| v.to_string()),
                "told" => s.told = parse_version(v).map(|_| v.to_string()),
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let mut t = format!("checked={}\n", self.checked);
        if let Some(l) = &self.latest {
            t.push_str(&format!("latest={l}\n"));
        }
        if let Some(l) = &self.told {
            t.push_str(&format!("told={l}\n"));
        }
        t
    }

    pub fn load(dir: &Path) -> State {
        std::fs::read_to_string(file(dir)).map(|t| State::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(file(dir), self.to_text());
    }

    /// Is it time for an automatic check?
    pub fn due(&self, now: i64) -> bool {
        now - self.checked >= EVERY || now < self.checked
    }

    /// The newer version there is, if any.
    pub fn available(&self) -> Option<&str> {
        self.latest.as_deref().filter(|l| newer(l, current()))
    }

    /// The version to announce now (newer, and not announced before); marks it announced.
    pub fn announce(&mut self) -> Option<String> {
        let v = self.available()?.to_string();
        if self.told.as_deref() == Some(v.as_str()) {
            return None;
        }
        self.told = Some(v.clone());
        Some(v)
    }
}

fn file(dir: &Path) -> PathBuf {
    dir.join("update.txt")
}

/// The stand-in answer (`HARBOUR_UPDATE_LATEST`), when set to a version.
pub fn pretend() -> Option<String> {
    let v = std::env::var("HARBOUR_UPDATE_LATEST").ok()?;
    parse_version(&v).map(|(a, b, c)| format!("{a}.{b}.{c}"))
}

/// May this run ask GitHub at all? (Not in mock runs, not in the web build.)
pub fn can_ask() -> bool {
    !crate::rt::WEB && !matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"))
}

/// Ask for the latest release's version (blocks: call it on a worker thread).
pub fn fetch_latest() -> Result<String, String> {
    if let Some(v) = pretend() {
        return Ok(v);
    }
    if !can_ask() {
        return Err("Not checked in this run".into());
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        use std::process::{Command, Stdio};
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let out = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--max-time", "20", "--connect-timeout", "8", "--max-redirs", "3", "--max-filesize", "600000", "--proto", "=https", "--proto-redir", "=https", "--user-agent", "HarbourShell", "--"])
            .arg(&url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() || out.stdout.len() > LIMIT {
            return Err("No answer from the release server".into());
        }
        latest_from_json(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "No release found".to_string())
    }
    #[cfg(target_os = "emscripten")]
    Err("Not checked in this run".into())
}

/// The result of a check running in the background, picked up by the host's loop.
static RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);
static RUNNING: Mutex<bool> = Mutex::new(false);

/// Start a check in the background (nothing happens while one is running).
pub fn start() {
    if let Ok(mut r) = RUNNING.lock() {
        if *r {
            return;
        }
        *r = true;
    }
    crate::rt::spawn(|| {
        let res = fetch_latest();
        if let Ok(mut r) = RESULT.lock() {
            *r = Some(res);
        }
        if let Ok(mut r) = RUNNING.lock() {
            *r = false;
        }
    });
}

/// A check the user asked for (Settings): its answer is always said, also "up to date".
static ASKED: Mutex<bool> = Mutex::new(false);
/// The newer version there is (for Settings' row); set by the host from the saved state.
static AVAILABLE: Mutex<Option<String>> = Mutex::new(None);

/// Check now because the user asked.
pub fn ask_now() {
    if let Ok(mut a) = ASKED.lock() {
        *a = true;
    }
    start();
}

/// Was the finished check asked for by the user? (Read once.)
pub fn take_asked() -> bool {
    ASKED.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or(false)
}

pub fn set_available(v: Option<&str>) {
    if let Ok(mut a) = AVAILABLE.lock() {
        *a = v.map(str::to_string);
    }
}

/// The newer version there is, if any (as last checked).
pub fn available() -> Option<String> {
    AVAILABLE.lock().ok()?.clone()
}

/// The finished check, once.
pub fn take_result() -> Option<Result<String, String>> {
    RESULT.lock().ok()?.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(!newer("0.3.0-beta", "0.2.0"), "anything but three numbers is not a release");
        assert!(!newer("junk", "0.2.0"));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    #[test]
    fn the_release_record_gives_the_version() {
        assert_eq!(latest_from_json(r#"{"tag_name": "v0.3.1", "name": "Harbour Shell 0.3.1"}"#), Some("0.3.1".into()));
        assert_eq!(latest_from_json(r#"{"tag_name": "nightly"}"#), None);
        assert_eq!(latest_from_json(r#"{"message": "Not Found"}"#), None);
        assert_eq!(latest_from_json("<html>"), None);
    }

    #[test]
    fn the_state_survives_a_round_trip_and_ignores_junk() {
        let s = State { checked: 1_800_000_000, latest: Some("9.9.9".into()), told: Some("9.9.8".into()) };
        assert_eq!(State::parse(&s.to_text()), s);
        assert_eq!(State::parse("latest=$(rm -rf)\nchecked=x\nother=1\n"), State::default());
    }

    #[test]
    fn a_new_version_is_announced_once() {
        let mut s = State { checked: 0, latest: Some("999.0.0".into()), told: None };
        assert_eq!(s.available(), Some("999.0.0"));
        assert_eq!(s.announce(), Some("999.0.0".into()));
        assert_eq!(s.announce(), None, "not again for the same version");
        s.latest = Some("999.0.1".into());
        assert_eq!(s.announce(), Some("999.0.1".into()));
        s.latest = Some(current().into());
        assert_eq!(s.available(), None, "this version is not news");
        assert_eq!(s.announce(), None);
    }

    #[test]
    fn checks_are_a_day_apart() {
        let s = State { checked: 1000, ..Default::default() };
        assert!(!s.due(1000 + EVERY - 1));
        assert!(s.due(1000 + EVERY));
        assert!(s.due(10), "a clock set back does not block the check for ever");
        assert!(State::default().due(1_800_000_000), "never checked");
    }
}
