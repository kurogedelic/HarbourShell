//! Installing what a feature needs, from inside Harbour.
//!
//! An app returns [`AppAction::Install`](crate::launcher::app::AppAction) with what it needs and
//! why; the host shows one card over everything: what will be installed and why, a password field
//! when apt needs root (the user's own password, for `sudo`), then progress, then the result.
//! The password is kept only while the card is open and goes to `sudo -S` on its stdin; it is never
//! logged or written anywhere.
//!
//! Jobs:
//! * [`Job::Apt`]: Debian packages (`sudo -S apt-get install -y ...`). Names are checked first.
//! * [`Job::ImeDict`]: the optional Japanese input dictionary (`ime_install.sh install --yes`,
//!   into the user's home: no password).
//!
//! * [`Job::Harbour`]: a newer Harbour Shell (the release's `.deb` is downloaded, its SHA-256
//!   checked against the release's own, then `sudo -S apt-get install -y <file>`).
//!
//! With `HARBOUR_MOCK=1` (or off Linux) nothing runs: the card walks through believable progress.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use sdl2::keyboard::Keycode;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::chrome;
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::widgets::{self, contains, draw_button, BtnState, ProgressBar};
use crate::theme::ThemeManager;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Apt(Vec<String>),
    /// Uninstall Debian packages (an installed app).
    AptRemove(Vec<String>),
    ImeDict,
    /// Harbour Shell itself, this version (see `launcher::update`).
    Harbour(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallReq {
    pub job: Job,
    /// One short sentence: who needs it and for what ("Interceptor scans the I2C bus with it.").
    pub why: String,
}

impl InstallReq {
    pub fn apt(pkgs: &[&str], why: &str) -> Self {
        Self { job: Job::Apt(pkgs.iter().map(|s| s.to_string()).collect()), why: why.into() }
    }
    pub fn apt_remove(pkgs: &[&str], why: &str) -> Self {
        Self { job: Job::AptRemove(pkgs.iter().map(|s| s.to_string()).collect()), why: why.into() }
    }
    pub fn removes(&self) -> bool {
        matches!(self.job, Job::AptRemove(_))
    }
    pub fn harbour(version: &str) -> Self {
        Self { job: Job::Harbour(version.to_string()), why: "Newer version. Files and settings stay.".into() }
    }
    pub fn ime_dict() -> Self {
        Self { job: Job::ImeDict, why: "Kanji conversion for Japanese input.".into() }
    }
    /// The name shown in the card's title.
    pub fn what(&self) -> String {
        match &self.job {
            Job::Apt(p) | Job::AptRemove(p) => p.join(", "),
            Job::ImeDict => i18n::tr_s("the Japanese dictionary"),
            Job::Harbour(v) => format!("Harbour Shell {v}"),
        }
    }
}

/// A Debian package name as apt accepts it (lower case, digits, `+ - .`, starting alnum).
pub fn valid_package(name: &str) -> bool {
    let mut it = name.chars();
    matches!(it.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() >= 2
        && name.len() <= 64
        && it.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
}

/// `sudo` arguments for `apt-get <verb>` (install / remove) of `pkgs`: the password comes on stdin,
/// no prompt text, no cached ticket (so a wrong password is always noticed).
pub fn apt_args(verb: &str, pkgs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ["-S", "-k", "-p", "", "apt-get", verb, "-y"].iter().map(|s| s.to_string()).collect();
    if verb == "install" {
        v.push("--no-install-recommends".into());
    }
    v.extend(pkgs.iter().cloned());
    v
}

/// A failed install, said plainly (from the tail of sudo / apt output).
pub fn explain_failure(output: &str) -> String {
    let has = |s: &str| output.contains(s);
    if has("incorrect password") || has("Sorry, try again") || has("no password was provided") {
        "Wrong password".into()
    } else if has("is not in the sudoers") || has("not allowed to") {
        "This user may not install software (sudo)".into()
    } else if has("Could not get lock") || has("is held by process") {
        "Another install is running; try again later".into()
    } else if has("Unable to locate package") {
        "Package not found (try updating the package list)".into()
    } else if has("Temporary failure resolving") || has("Could not resolve") {
        "No network connection".into()
    } else {
        output.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Install failed").to_string()
    }
}

enum Msg {
    Line(String),
    Done(Result<(), String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Ask,
    Running,
    Done(Result<(), String>),
}

/// What the host should do after a key or click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    Stay,
    /// Close the card; `installed` = the job finished successfully.
    Close { installed: bool },
}

pub struct Installer {
    pub req: InstallReq,
    pub stage: Stage,
    password: String,
    /// Highlighted button on the Ask card: 0 = Cancel, 1 = Install.
    sel: usize,
    rx: Option<Receiver<Msg>>,
    pub last_line: String,
    tick: u32,
    mock: bool,
    started: Option<Instant>,
}

impl Installer {
    pub fn new(req: InstallReq) -> Self {
        let mock = cfg!(not(target_os = "linux")) || matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
        Self { req, stage: Stage::Ask, password: String::new(), sel: 1, rx: None, last_line: String::new(), tick: 0, mock, started: None }
    }

    /// apt needs root: ask for the user's password (not when Harbour already runs as root).
    pub fn needs_password(&self) -> bool {
        matches!(self.req.job, Job::Apt(_) | Job::AptRemove(_) | Job::Harbour(_)) && !is_root()
    }

    pub fn text(&mut self, t: &str) {
        if self.stage == Stage::Ask && self.needs_password() {
            self.password.extend(t.chars().filter(|c| !c.is_control()).take(128));
        }
    }

    pub fn key(&mut self, key: Keycode) -> Out {
        match (&self.stage, key) {
            (Stage::Ask, Keycode::Escape) => return self.cancel(),
            (Stage::Ask, Keycode::Backspace) => {
                self.password.pop();
            }
            (Stage::Ask, Keycode::Left) => self.sel = 0,
            (Stage::Ask, Keycode::Right) => self.sel = 1,
            (Stage::Ask, Keycode::Tab) => self.sel = 1 - self.sel,
            (Stage::Ask, Keycode::Return | Keycode::KpEnter) => {
                if self.sel == 0 {
                    return self.cancel();
                }
                self.start();
            }
            (Stage::Done(r), Keycode::Return | Keycode::KpEnter | Keycode::Escape) => {
                let ok = r.is_ok();
                // A wrong password goes back to the question, the field emptied.
                if r.as_ref().err().map_or(false, |e| e == "Wrong password") && key != Keycode::Escape {
                    self.stage = Stage::Ask;
                    self.sel = 1;
                    return Out::Stay;
                }
                return Out::Close { installed: ok };
            }
            _ => {}
        }
        Out::Stay
    }

    fn cancel(&mut self) -> Out {
        self.password.clear();
        Out::Close { installed: false }
    }

    pub fn click(&mut self, x: i32, y: i32) -> Out {
        let r = card_rect();
        match &self.stage {
            Stage::Ask => {
                let [cancel, ok] = ask_buttons(r);
                if contains(cancel, x, y) {
                    return self.cancel();
                }
                if contains(ok, x, y) {
                    self.sel = 1;
                    self.start();
                }
                Out::Stay
            }
            Stage::Done(_) if contains(done_button(r), x, y) => self.key(Keycode::Return),
            _ => Out::Stay,
        }
    }

    fn start(&mut self) {
        if self.needs_password() && self.password.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.stage = Stage::Running;
        self.started = Some(Instant::now());
        self.last_line = i18n::tr_s("Starting...");
        let pw = std::mem::take(&mut self.password);
        let job = self.req.job.clone();
        let mock = self.mock;
        std::thread::spawn(move || {
            let r = if mock { run_mock(&job, &pw, &tx) } else { run(&job, pw, &tx) };
            let _ = tx.send(Msg::Done(r));
        });
    }

    /// Per frame: progress lines and the result. True when the card changed.
    pub fn tick(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        let mut dirty = self.stage == Stage::Running;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Line(l) => self.last_line = l,
                    Msg::Done(r) => {
                        self.stage = Stage::Done(r);
                        self.rx = None;
                        break;
                    }
                }
                dirty = true;
            }
        }
        dirty
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        crate::dither::fill(c, Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32), tm.get_color("widget_shadow"), crate::dither::Pattern::Eighth);
        let r = card_rect();
        widgets::draw_panel(c, tm, r);
        let tx = r.x() + 14;
        let w = r.width() as i32 - 28;
        let title = |c: &mut Canvas<Window>, s: &str| fs.draw_display_text(c, tx, r.y() + 8, &chrome::fit_display_text(fs, s, w), ink);
        let line = |c: &mut Canvas<Window>, i: i32, s: &str, col| fs.draw_text(c, tx, r.y() + 26 + i * 14, &chrome::fit_text(fs, s, w), col);
        match &self.stage {
            Stage::Ask => {
                let q = if self.req.removes() { format!("Uninstall {}?", self.req.what()) } else { format!("Install {}?", self.req.what()) };
                title(c, &i18n::tr_s(&q));
                line(c, 0, &i18n::tr_s(&self.req.why), faint);
                if self.needs_password() {
                    line(c, 1, &i18n::tr_s(if self.req.removes() { "Enter your password to uninstall (sudo)." } else { "Enter your password to install (sudo)." }), faint);
                    let f = field_rect(r);
                    widgets::draw_field(c, fs, tm, f, &widgets::Field { text: &self.password, caret: Some(self.password.chars().count()), masked: true, ..Default::default() });
                } else {
                    line(c, 1, &i18n::tr_s("It is downloaded and set up for you."), faint);
                }
                let [cancel, ok] = ask_buttons(r);
                let ready = !self.needs_password() || !self.password.is_empty();
                draw_button(c, fs, tm, cancel, "Cancel", BtnState { default: self.sel == 0, ..Default::default() });
                draw_button(c, fs, tm, ok, if self.req.removes() { "Uninstall" } else { "Install" }, BtnState { default: self.sel == 1, disabled: !ready, ..Default::default() });
            }
            Stage::Running => {
                let t = if self.req.removes() { format!("Uninstalling {}...", self.req.what()) } else { format!("Installing {}...", self.req.what()) };
                title(c, &i18n::tr_s(&t));
                line(c, 0, &self.last_line, faint);
                let secs = self.started.map_or(0, |t| t.elapsed().as_secs());
                line(c, 1, &i18n::tr_s(&format!("{} s", secs)), faint);
                ProgressBar::new(Rect::new(tx, r.y() + r.height() as i32 - 30, w as u32, 14), None, self.tick).draw(c, tm);
            }
            Stage::Done(res) => {
                match res {
                    Ok(()) if self.req.removes() => {
                        title(c, &i18n::tr_s("Uninstalled"));
                        line(c, 0, &i18n::tr_s(&format!("{} was removed.", self.req.what())), faint);
                    }
                    Ok(()) if matches!(self.req.job, Job::Harbour(_)) => {
                        title(c, &i18n::tr_s("Installed"));
                        line(c, 0, &i18n::tr_s(&format!("{} is installed.", self.req.what())), faint);
                        line(c, 1, &i18n::tr_s("Restart Harbour to use it."), faint);
                    }
                    Ok(()) => {
                        title(c, &i18n::tr_s("Installed"));
                        line(c, 0, &i18n::tr_s(&format!("{} is ready.", self.req.what())), faint);
                    }
                    Err(e) => {
                        title(c, &i18n::tr_s(if self.req.removes() { "Could not uninstall" } else { "Could not install" }));
                        line(c, 0, &i18n::tr_s(e), ink);
                        if e == "Wrong password" {
                            line(c, 1, &i18n::tr_s("Try again with the password you log in with."), faint);
                        }
                    }
                }
                draw_button(c, fs, tm, done_button(r), "OK", BtnState { default: true, ..Default::default() });
            }
        }
    }
}

fn card_rect() -> Rect {
    let (w, h) = (288u32, 112u32);
    Rect::new((grid::sw() as i32 - w as i32) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h as i32) / 2, w, h)
}

fn field_rect(r: Rect) -> Rect {
    // Under the two text lines, as wide as they are.
    Rect::new(r.x() + 14, r.y() + 56, r.width() - 28, widgets::FIELD_H as u32)
}

fn ask_buttons(r: Rect) -> [Rect; 2] {
    let y = r.y() + r.height() as i32 - 32;
    [Rect::new(r.right() - 14 - 84 - 10 - 84, y, 84, 24), Rect::new(r.right() - 14 - 84, y, 84, 24)]
}

fn done_button(r: Rect) -> Rect {
    Rect::new(r.right() - 14 - 64, r.y() + r.height() as i32 - 32, 64, 24)
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Where `ime_install.sh` is: the installed package, the bundle next to the binary, the source tree.
fn ime_script() -> Option<PathBuf> {
    let mut v = vec![PathBuf::from("/usr/share/harbour-shell/ime_install.sh")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("ime_install.sh"));
            v.push(d.join("../ime_install.sh"));
        }
    }
    v.push(PathBuf::from("packaging/ime_install.sh"));
    v.into_iter().find(|p| p.is_file())
}

/// Feed every output line (stdout and stderr) to `tx`; returns (success, all output).
fn pump(mut child: std::process::Child, tx: &Sender<Msg>) -> (bool, String) {
    let (ltx, lrx) = channel::<String>();
    let mut readers = Vec::new();
    if let Some(o) = child.stdout.take() {
        let t = ltx.clone();
        readers.push(std::thread::spawn(move || BufReader::new(o).lines().map_while(Result::ok).for_each(|l| drop(t.send(l)))));
    }
    if let Some(e) = child.stderr.take() {
        let t = ltx.clone();
        readers.push(std::thread::spawn(move || BufReader::new(e).lines().map_while(Result::ok).for_each(|l| drop(t.send(l)))));
    }
    drop(ltx);
    let mut all = String::new();
    for l in lrx {
        let l = l.trim().to_string();
        if !l.is_empty() {
            let _ = tx.send(Msg::Line(l.clone()));
            all.push_str(&l);
            all.push('\n');
        }
    }
    for r in readers {
        let _ = r.join();
    }
    (child.wait().map(|s| s.success()).unwrap_or(false), all)
}

fn run(job: &Job, password: String, tx: &Sender<Msg>) -> Result<(), String> {
    match job {
        Job::Apt(pkgs) | Job::AptRemove(pkgs) => {
            if pkgs.is_empty() || !pkgs.iter().all(|p| valid_package(p)) {
                return Err("Not a valid package name".into());
            }
            let verb = if matches!(job, Job::AptRemove(_)) { "remove" } else { "install" };
            sudo_apt(verb, pkgs, password, tx)
        }
        Job::Harbour(ver) => {
            if crate::launcher::update::parse_version(ver).is_none() {
                return Err("Not a version".into());
            }
            let deb = format!("harbour-shell_{ver}_arm64.deb");
            let base = format!("https://github.com/{}/releases/download/v{ver}", crate::launcher::update::REPO);
            let dir = std::env::temp_dir().join(format!("harbour-update-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let res = (|| {
                for name in [deb.clone(), format!("{deb}.sha256")] {
                    let _ = tx.send(Msg::Line(format!("downloading {name}")));
                    let ok = Command::new("curl")
                        .args(["--fail", "--silent", "--show-error", "--location", "--retry", "2", "--max-time", "600", "--proto", "=https", "--proto-redir", "=https", "--user-agent", "HarbourShell", "-o"])
                        .arg(dir.join(&name))
                        .arg(format!("{base}/{name}"))
                        .stdin(Stdio::null())
                        .status()
                        .map_err(|e| format!("curl: {e}"))?
                        .success();
                    if !ok {
                        return Err("Download failed (no network, or the release has no package)".to_string());
                    }
                }
                let _ = tx.send(Msg::Line("checking the download".into()));
                let want = std::fs::read_to_string(dir.join(format!("{deb}.sha256"))).map_err(|e| e.to_string())?;
                let have = Command::new("sha256sum").arg(dir.join(&deb)).output().map_err(|e| format!("sha256sum: {e}"))?;
                let have = String::from_utf8_lossy(&have.stdout).to_string();
                if !checksum_matches(&want, &have) {
                    return Err("The download is damaged or not the release (checksum)".to_string());
                }
                sudo_apt("install", &[dir.join(&deb).to_string_lossy().to_string()], password, tx)
            })();
            let _ = std::fs::remove_dir_all(&dir);
            res
        }
        Job::ImeDict => {
            let script = ime_script().ok_or("ime_install.sh not found")?;
            let child = Command::new("sh").arg(&script).args(["install", "--yes"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
            let (ok, out) = pump(child, tx);
            let dic = crate::launcher::ime_glue::data_dir().join(crate::ime::DICT_FILE);
            if ok && dic.is_file() {
                Ok(())
            } else {
                Err(explain_failure(&out))
            }
        }
    }
}

/// Do the first words of two `sha256sum` lines agree (and look like a SHA-256)?
pub fn checksum_matches(want: &str, have: &str) -> bool {
    let first = |s: &str| s.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let (w, h) = (first(want), first(have));
    w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()) && w == h
}

/// `apt-get <verb> -y <args>` through sudo with the password on its stdin (straight when root).
fn sudo_apt(verb: &str, args: &[String], password: String, tx: &Sender<Msg>) -> Result<(), String> {
    let mut cmd = if is_root() {
        let mut c = Command::new("apt-get");
        c.args(apt_args(verb, args).into_iter().skip(5));
        c
    } else {
        let mut c = Command::new("sudo");
        c.args(apt_args(verb, args));
        c
    };
    let mut child = cmd
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sudo: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let mut pw = password;
        pw.push('\n');
        let _ = si.write_all(pw.as_bytes());
        // Overwrite our copy before it is dropped.
        // SAFETY: zeroing the bytes keeps the String valid UTF-8 (all NULs).
        unsafe { pw.as_bytes_mut().iter_mut().for_each(|b| *b = 0) };
    }
    let (ok, out) = pump(child, tx);
    if ok {
        Ok(())
    } else {
        Err(explain_failure(&out))
    }
}

fn run_mock(job: &Job, password: &str, tx: &Sender<Msg>) -> Result<(), String> {
    let steps: Vec<String> = match job {
        Job::Apt(p) | Job::AptRemove(p) => {
            if password == "wrong" {
                std::thread::sleep(Duration::from_millis(600));
                return Err("Wrong password".into());
            }
            vec!["Reading package lists...".into(), "Building dependency tree...".into(), format!("Unpacking {} ...", p.join(" ")), format!("Setting up {} ...", p.join(" "))]
        }
        Job::ImeDict => vec!["downloading dictionary00.txt".into(), "downloading dictionary09.txt".into(), "converting...".into()],
        Job::Harbour(v) => {
            if password == "wrong" {
                std::thread::sleep(Duration::from_millis(600));
                return Err("Wrong password".into());
            }
            vec![format!("downloading harbour-shell_{v}_arm64.deb"), "checking the download".into(), format!("Unpacking harbour-shell ({v}) ..."), format!("Setting up harbour-shell ({v}) ...")]
        }
    };
    for s in steps {
        let _ = tx.send(Msg::Line(s));
        std::thread::sleep(Duration::from_millis(450));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_is_accepted_only_with_the_release_checksum() {
        let sum = "a".repeat(64);
        assert!(checksum_matches(&format!("{sum}  harbour-shell_1.0.0_arm64.deb\n"), &format!("{}  /tmp/x.deb\n", sum.to_uppercase())));
        assert!(!checksum_matches(&format!("{sum}  x"), &format!("{}  x", "b".repeat(64))));
        assert!(!checksum_matches("", ""), "an empty answer is not a match");
        assert!(!checksum_matches("Not Found", "Not Found"));
    }

    #[test]
    fn package_names_are_checked() {
        for ok in ["i2c-tools", "gpiod", "libsdl2-2.0-0", "g++", "python3.11"] {
            assert!(valid_package(ok), "{ok}");
        }
        for bad in ["", "a", "I2C-tools", "-y", "foo bar", "x;rm", "../x", "--purge"] {
            assert!(!valid_package(bad), "{bad}");
        }
        assert_eq!(apt_args("install", &["i2c-tools".into()]), ["-S", "-k", "-p", "", "apt-get", "install", "-y", "--no-install-recommends", "i2c-tools"]);
        assert_eq!(apt_args("remove", &["zclaw".into()]), ["-S", "-k", "-p", "", "apt-get", "remove", "-y", "zclaw"]);
    }

    #[test]
    fn failures_are_said_plainly() {
        assert_eq!(explain_failure("[sudo] password for pi: Sorry, try again.\nsudo: 1 incorrect password attempt"), "Wrong password");
        assert_eq!(explain_failure("E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 812 (apt)"), "Another install is running; try again later");
        assert_eq!(explain_failure("E: Unable to locate package nope"), "Package not found (try updating the package list)");
        assert_eq!(explain_failure("pi is not in the sudoers file."), "This user may not install software (sudo)");
        assert_eq!(explain_failure("something\nE: last words\n\n"), "E: last words");
    }

    fn wait_done(i: &mut Installer) {
        let t = Instant::now();
        while i.stage == Stage::Running && t.elapsed() < Duration::from_secs(10) {
            i.tick();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_card_asks_runs_and_reports() {
        let mut i = Installer::new(InstallReq::apt(&["i2c-tools"], "Interceptor scans the I2C bus with it."));
        i.mock = true;
        // No password yet: Install does nothing.
        if i.needs_password() {
            assert_eq!(i.key(Keycode::Return), Out::Stay);
            assert_eq!(i.stage, Stage::Ask);
            i.text("wrong");
        }
        assert_eq!(i.key(Keycode::Return), Out::Stay);
        assert_eq!(i.stage, Stage::Running);
        assert!(i.password.is_empty(), "the password leaves the card when the job starts");
        wait_done(&mut i);
        if i.needs_password() {
            assert_eq!(i.stage, Stage::Done(Err("Wrong password".into())));
            // OK on a wrong password asks again.
            assert_eq!(i.key(Keycode::Return), Out::Stay);
            assert_eq!(i.stage, Stage::Ask);
            i.text("secret");
            i.key(Keycode::Return);
            wait_done(&mut i);
        }
        assert_eq!(i.stage, Stage::Done(Ok(())));
        assert_eq!(i.key(Keycode::Return), Out::Close { installed: true });
    }

    #[test]
    fn escape_cancels_and_forgets_the_password() {
        let mut i = Installer::new(InstallReq::apt(&["gpiod"], "x"));
        i.text("abc");
        assert_eq!(i.key(Keycode::Escape), Out::Close { installed: false });
        assert!(i.password.is_empty());
    }
}
