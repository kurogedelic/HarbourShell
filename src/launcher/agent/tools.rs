//! The Harbour API as tools. No shell tool: everything here maps onto something the launcher
//! already does. Tools that need the host's screen state (launch an app, press keys, describe
//! the screen) are returned as a [`HostReq`] for the main loop; the rest run right here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::keys::{Keycode, Mod};
use serde_json::{json, Value};

use super::settings;
use super::types::{ToolCall, ToolSpec};
use crate::platform::keys::Input;
use crate::system::config::Config;
use crate::system::Backend;

pub const MAX_KEYS: usize = 20;
pub const MAX_FILE_BYTES: u64 = 16 * 1024;
pub const MAX_RESULT_CHARS: usize = 4000;
pub const MAX_ROUNDS: usize = 8;

/// A tool the main loop has to run (it owns the screen).
#[derive(Clone, Debug, PartialEq)]
pub enum HostReq {
    ListApps,
    Launch(String),
    CurrentScreen,
    PressKeys(Vec<Input>),
    ShowMessage(String),
}

pub enum Exec {
    Done(Result<String, String>),
    Host(HostReq),
}

fn spec(name: &'static str, description: &'static str, props: Value, required: &[&str]) -> ToolSpec {
    ToolSpec { name, description, schema: json!({"type": "object", "properties": props, "required": required}) }
}

pub fn specs() -> Vec<ToolSpec> {
    let s = json!({"type": "string"});
    vec![
        spec("list_apps", "List the apps that can be launched (names).", json!({}), &[]),
        spec("launch_app", "Open an app by name (see list_apps). Replaces the screen behind this chat.", json!({"name": s}), &["name"]),
        spec("current_screen", "Describe, as compact text, what is on the screen behind this chat (app, page, rows).", json!({}), &[]),
        spec(
            "press_keys",
            "Press keys in the app behind this chat, e.g. \"Down Down Return\" or \"Escape\". Names: Up Down Left Right Return Escape Tab Space Backspace Delete Home End PageUp PageDown, single characters, Shift+/Ctrl+ prefixes. At most 20 keys.",
            json!({"keys": s}),
            &["keys"],
        ),
        spec(
            "get_setting",
            "Read a device setting. Names: brightness, volume, mute, theme, invert, statusbar, screen_off, lock_style, auto_lock, lock_screen_off, lock_date, lock_battery. Omit name to read all.",
            json!({"name": s}),
            &[],
        ),
        spec(
            "set_setting",
            "Change a device setting. brightness 5-100, volume 0-100, mute on/off, theme black|sky|mobile|terminal|platinum, invert on/off, statusbar default|black|white, screen_off 0|10|30|60|300 seconds, lock_style wall|world|ascii|7seg, auto_lock 0|60|300|900 seconds, lock_screen_off 0|5|8|15|30|60 seconds, lock_date on/off, lock_battery on/off.",
            json!({"name": s, "value": s}),
            &["name", "value"],
        ),
        spec("read_file", "Read a text file under the user's home directory (at most 16 KB).", json!({"path": s}), &["path"]),
        spec("list_dir", "List a directory under the user's home directory.", json!({"path": s}), &["path"]),
        spec("system_info", "Battery, network addresses, uptime, temperature, memory, storage.", json!({}), &[]),
        spec("show_message", "Show a short message (toast) at the bottom of the screen.", json!({"text": s}), &["text"]),
    ]
}

/// Tools with side effects run behind a confirmation card unless auto-run is on.
pub fn needs_confirm(name: &str) -> bool {
    matches!(name, "set_setting" | "launch_app" | "press_keys")
}

pub fn system_prompt(w: i32, h: i32) -> String {
    format!(
        "You are the Agent of Harbour Shell, a home screen for the M5Stack Cardputer Zero: a pocket \
Linux computer (Raspberry Pi CM0, 352 MB RAM) with a {w}x{h} pixel LCD and a 46-key keyboard. \
The user talks to you from a small chat screen, so keep answers short (a few lines of plain text, no markdown). \
You are given tools to use the device: list and launch apps, look at the screen, press keys, read and change settings, \
read files under the home directory, and show messages. Prefer reading (current_screen, get_setting, list_dir) \
before changing things, and say briefly what you did. You have no shell. \
The chat stays on top of the app you control; the user closes it with Esc to see the result. \
Answer in the user's language."
    )
}

fn arg_str<'a>(args: &'a Value, k: &str) -> Result<&'a str, String> {
    args.get(k).and_then(Value::as_str).ok_or_else(|| format!("missing argument \"{k}\""))
}

/// A short human description: "launch Files", "set brightness = 50", "press Down Return".
pub fn describe_call(call: &ToolCall) -> String {
    let a = |k: &str| call.args.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    match call.name.as_str() {
        "launch_app" => format!("launch {}", a("name")),
        "press_keys" => format!("press {}", a("keys")),
        "set_setting" => format!("set {} = {}", a("name"), a("value")),
        "get_setting" => match call.args.get("name").and_then(Value::as_str) {
            Some(n) => format!("get setting {n}"),
            None => "get settings".into(),
        },
        "read_file" => format!("read {}", a("path")),
        "list_dir" => format!("list {}", a("path")),
        "show_message" => format!("message {}", a("text")),
        "list_apps" => "list apps".into(),
        "current_screen" => "look at the screen".into(),
        "system_info" => "system info".into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- keys

fn key_by_name(n: &str) -> Option<(Keycode, Option<char>)> {
    let l = n.to_lowercase();
    let k = match l.as_str() {
        "up" => Keycode::Up,
        "down" => Keycode::Down,
        "left" => Keycode::Left,
        "right" => Keycode::Right,
        "return" | "enter" => Keycode::Return,
        "escape" | "esc" => Keycode::Escape,
        "tab" => Keycode::Tab,
        "space" => return Some((Keycode::Space, Some(' '))),
        "backspace" => Keycode::Backspace,
        "delete" | "del" => Keycode::Delete,
        "home" => Keycode::Home,
        "end" => Keycode::End,
        "pageup" => Keycode::PageUp,
        "pagedown" => Keycode::PageDown,
        _ => {
            let mut it = n.chars();
            let (c, rest) = (it.next()?, it.next());
            if rest.is_some() || !c.is_ascii_graphic() {
                return None;
            }
            let kc = Keycode::from_name(&c.to_ascii_uppercase().to_string()).or_else(|| Keycode::from_name(&c.to_string()))?;
            return Some((kc, Some(c)));
        }
    };
    Some((k, None))
}

/// "Down Down Return" -> input events. Printable characters also produce text input, like the
/// real keyboard does. At most [`MAX_KEYS`] keys.
pub fn parse_keys(s: &str) -> Result<Vec<Input>, String> {
    let toks: Vec<&str> = s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).collect();
    if toks.is_empty() {
        return Err("no keys given".into());
    }
    if toks.len() > MAX_KEYS {
        return Err(format!("too many keys ({}, the limit is {MAX_KEYS} per call)", toks.len()));
    }
    let mut out = vec![];
    for t in toks {
        let mut mods = Mod::NOMOD;
        let mut name = t;
        loop {
            let lower = name.to_lowercase();
            if let Some(r) = lower.strip_prefix("shift+").map(|_| &name[6..]) {
                mods |= Mod::LSHIFTMOD;
                name = r;
            } else if let Some(r) = lower.strip_prefix("ctrl+").map(|_| &name[5..]) {
                mods |= Mod::LCTRLMOD;
                name = r;
            } else {
                break;
            }
        }
        let (key, ch) = key_by_name(name).ok_or_else(|| format!("unknown key \"{t}\""))?;
        out.push(Input::Key { key, mods, repeat: false });
        if let Some(c) = ch {
            if !mods.contains(Mod::LCTRLMOD) {
                let c = if mods.contains(Mod::LSHIFTMOD) { c.to_ascii_uppercase() } else { c };
                out.push(Input::Text(c.to_string()));
            }
        }
        out.push(Input::KeyUp { key });
    }
    Ok(out)
}

// ---------------------------------------------------------------- files

/// A path under `$HOME` (symlinks resolved, so a link cannot lead out). `~` expands.
pub fn home_path(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let raw = raw.trim();
    let p = if let Some(r) = raw.strip_prefix("~/") {
        home.join(r)
    } else if raw == "~" || raw.is_empty() {
        home.to_path_buf()
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        home.join(raw)
    };
    let real = p.canonicalize().map_err(|e| format!("{raw}: {}", e.kind().to_string().to_lowercase()))?;
    let home_real = home.canonicalize().map_err(|e| format!("home: {e}"))?;
    if !real.starts_with(&home_real) {
        return Err("only files under the home directory can be read".into());
    }
    Ok(real)
}

pub fn clip(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max).collect::<String>() + "\n[...cut]";
    }
    s
}

pub fn read_file(raw: &str, home: &Path) -> Result<String, String> {
    use std::io::Read;
    let p = home_path(raw, home)?;
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    let mut buf = vec![];
    std::fs::File::open(&p).and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut buf)).map_err(|e| e.to_string())?;
    let mut text = match String::from_utf8(buf.clone()) {
        Ok(t) => t,
        // A cut in the middle of a multi-byte character is fine; real binary data is not.
        Err(e) if e.utf8_error().error_len().is_none() => String::from_utf8_lossy(&buf[..e.utf8_error().valid_up_to()]).to_string(),
        Err(_) => return Err("not a text file".into()),
    };
    if meta.len() > MAX_FILE_BYTES {
        text.push_str(&format!("\n[cut: file is {} bytes]", meta.len()));
    }
    Ok(clip(text, MAX_RESULT_CHARS))
}

pub fn list_dir(raw: &str, home: &Path) -> Result<String, String> {
    let p = home_path(raw, home)?;
    let mut ents: Vec<(bool, String, u64)> = std::fs::read_dir(&p)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| {
            let m = e.metadata();
            (m.as_ref().map_or(false, |m| m.is_dir()), e.file_name().to_string_lossy().to_string(), m.map_or(0, |m| m.len()))
        })
        .filter(|(_, n, _)| !n.starts_with('.'))
        .collect();
    ents.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let total = ents.len();
    let mut lines: Vec<String> = ents.iter().take(60).map(|(d, n, s)| if *d { format!("{n}/") } else { format!("{n} ({s} bytes)") }).collect();
    if total > 60 {
        lines.push(format!("[{} more]", total - 60));
    }
    if lines.is_empty() {
        lines.push("(empty)".into());
    }
    Ok(clip(lines.join("\n"), MAX_RESULT_CHARS))
}

pub fn system_info(sys: &dyn Backend) -> String {
    let b = sys.battery();
    let w = sys.wifi_status();
    let i = sys.sysinfo();
    let mut l = vec![];
    l.push(format!(
        "battery: {}{}",
        b.percent.map_or("unknown".to_string(), |p| format!("{p}%")),
        if b.charging { " (charging)" } else { "" }
    ));
    l.push(match (&w.ssid, &w.ip) {
        (Some(s), ip) => format!("wifi: {s} {}", ip.clone().unwrap_or_default()),
        _ => format!("wifi: {}", if w.radio { "not connected" } else { "off" }),
    });
    l.push(format!("ethernet: {}", sys.eth_ip().unwrap_or_else(|| "none".into())));
    l.push(format!("uptime: {}h {}m", i.uptime_s / 3600, i.uptime_s / 60 % 60));
    if let Some(t) = i.temp_c {
        l.push(format!("temperature: {t:.0} C"));
    }
    l.push(format!("memory: {} MB free of {} MB", i.mem_avail_kb / 1024, i.mem_total_kb / 1024));
    l.push(format!("storage: {} MB free of {} MB", i.disk_free_mb, i.disk_total_mb));
    l.push(format!("model: {}; {}", i.model, i.os));
    l.join("\n")
}

/// What the tools need besides the call.
pub struct ToolEnv<'a> {
    pub sys: &'a dyn Backend,
    pub cfg: &'a Mutex<Config>,
    pub home: &'a Path,
}

/// Run one call. Errors come back as `Done(Err(..))` and go to the model as an error result.
pub fn execute(call: &ToolCall, env: &ToolEnv, restyle: &mut bool) -> Exec {
    let a = &call.args;
    let r: Result<String, String> = match call.name.as_str() {
        "list_apps" => return Exec::Host(HostReq::ListApps),
        "current_screen" => return Exec::Host(HostReq::CurrentScreen),
        "launch_app" => match arg_str(a, "name") {
            Ok(n) => return Exec::Host(HostReq::Launch(n.to_string())),
            Err(e) => Err(e),
        },
        "press_keys" => match arg_str(a, "keys").and_then(parse_keys) {
            Ok(k) => return Exec::Host(HostReq::PressKeys(k)),
            Err(e) => Err(e),
        },
        "show_message" => match arg_str(a, "text") {
            Ok(t) => return Exec::Host(HostReq::ShowMessage(t.chars().take(60).collect())),
            Err(e) => Err(e),
        },
        "get_setting" => match a.get("name").and_then(Value::as_str) {
            Some(n) => settings::get(n, env.sys, env.cfg).map(|v| format!("{n}: {v}")),
            None => Ok(settings::list_all(env.sys, env.cfg)),
        },
        "set_setting" => arg_str(a, "name").and_then(|n| {
            let v = match a.get("value") {
                Some(Value::String(s)) => s.clone(),
                Some(other) if !other.is_null() => other.to_string(),
                _ => return Err("missing argument \"value\"".into()),
            };
            settings::set(n, &v, env.sys, env.cfg).map(|o| {
                *restyle |= o.restyle;
                o.message
            })
        }),
        "read_file" => arg_str(a, "path").and_then(|p| read_file(p, env.home)),
        "list_dir" => arg_str(a, "path").and_then(|p| list_dir(p, env.home)),
        "system_info" => Ok(system_info(env.sys)),
        other => Err(format!("unknown tool \"{other}\"")),
    };
    Exec::Done(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), args }
    }

    fn home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(d.join("docs/a.txt"), "hello 日本語").unwrap();
        std::fs::write(d.join("bin.dat"), [0u8, 159, 146, 150, 0xff]).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn every_tool_has_an_object_schema_and_a_unique_name() {
        let s = specs();
        let mut names: Vec<_> = s.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), 10);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 10);
        for t in &s {
            assert_eq!(t.schema["type"], "object", "{}", t.name);
        }
        assert!(!s.iter().any(|t| t.name.contains("shell") || t.name.contains("exec")));
    }

    #[test]
    fn side_effect_tools_need_confirmation_and_reads_do_not() {
        for n in ["set_setting", "launch_app", "press_keys"] {
            assert!(needs_confirm(n));
        }
        for n in ["list_apps", "current_screen", "get_setting", "read_file", "list_dir", "system_info", "show_message"] {
            assert!(!needs_confirm(n), "{n}");
        }
    }

    #[test]
    fn press_keys_parses_names_modifiers_and_the_cap() {
        let v = parse_keys("Down Down Return").unwrap();
        assert_eq!(v.iter().filter(|i| matches!(i, Input::Key { .. })).count(), 3);
        let v = parse_keys("a Shift+b Ctrl+c").unwrap();
        let texts: Vec<_> = v.iter().filter_map(|i| if let Input::Text(t) = i { Some(t.as_str()) } else { None }).collect();
        assert_eq!(texts, ["a", "B"], "ctrl+c is a shortcut, not text");
        assert!(parse_keys("Frobnicate").unwrap_err().contains("unknown key"));
        assert!(parse_keys("").is_err());
        let ok = vec!["Down"; MAX_KEYS].join(" ");
        assert!(parse_keys(&ok).is_ok());
        let many = vec!["Down"; MAX_KEYS + 1].join(" ");
        assert!(parse_keys(&many).unwrap_err().contains("too many"));
    }

    #[test]
    fn dispatch_routes_host_tools_and_runs_the_rest_locally() {
        let sys = MockBackend::new();
        let h = home("disp");
        let cfgp = h.join("config.json");
        let cfg = Mutex::new(Config::load(cfgp));
        let env = ToolEnv { sys: &sys, cfg: &cfg, home: &h };
        let mut restyle = false;
        assert!(matches!(execute(&call("launch_app", json!({"name": "Files"})), &env, &mut restyle), Exec::Host(HostReq::Launch(n)) if n == "Files"));
        assert!(matches!(execute(&call("press_keys", json!({"keys": "Down"})), &env, &mut restyle), Exec::Host(HostReq::PressKeys(_))));
        assert!(matches!(execute(&call("list_apps", json!({})), &env, &mut restyle), Exec::Host(HostReq::ListApps)));
        let Exec::Done(r) = execute(&call("set_setting", json!({"name": "theme", "value": "sky"})), &env, &mut restyle) else { panic!() };
        assert!(r.unwrap().contains("sky"));
        assert!(restyle);
        let Exec::Done(r) = execute(&call("set_setting", json!({"name": "volume", "value": 40})), &env, &mut restyle) else { panic!() };
        assert!(r.unwrap().contains("40"), "numbers are accepted as values");
        let Exec::Done(r) = execute(&call("get_setting", json!({})), &env, &mut restyle) else { panic!() };
        assert!(r.unwrap().contains("brightness"));
        let Exec::Done(r) = execute(&call("system_info", json!({})), &env, &mut restyle) else { panic!() };
        assert!(r.unwrap().contains("battery"));
        let Exec::Done(r) = execute(&call("launch_app", json!({})), &env, &mut restyle) else { panic!() };
        assert!(r.is_err(), "missing argument");
        let Exec::Done(r) = execute(&call("rm_rf", json!({})), &env, &mut restyle) else { panic!() };
        assert!(r.unwrap_err().contains("unknown tool"));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn files_are_readable_only_under_home() {
        let h = home("files");
        assert_eq!(read_file("docs/a.txt", &h).unwrap(), "hello 日本語");
        assert_eq!(read_file("~/docs/a.txt", &h).unwrap(), "hello 日本語");
        assert!(read_file("/etc/passwd", &h).unwrap_err().contains("home"));
        assert!(read_file("../../../etc/passwd", &h).is_err());
        assert!(read_file("bin.dat", &h).unwrap_err().contains("not a text"));
        assert!(read_file("docs", &h).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", h.join("link")).unwrap();
            assert!(read_file("link/hosts", &h).is_err(), "a symlink cannot lead out of home");
        }
        let l = list_dir(".", &h).unwrap();
        assert!(l.starts_with("docs/"), "{l}");
        assert!(l.contains("bin.dat (5 bytes)"));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn big_files_are_cut() {
        let h = home("big");
        std::fs::write(h.join("big.txt"), "x".repeat(40000)).unwrap();
        let t = read_file("big.txt", &h).unwrap();
        assert!(t.chars().count() < MAX_RESULT_CHARS + 20);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn calls_are_described_briefly() {
        assert_eq!(describe_call(&call("launch_app", json!({"name": "Files"}))), "launch Files");
        assert_eq!(describe_call(&call("set_setting", json!({"name": "volume", "value": "30"}))), "set volume = 30");
    }
}
