//! The Settings the agent may read and change, mapped onto the same config keys and backend
//! calls the Settings app uses (never raw files).

use std::sync::Mutex;

use crate::launcher::settings::{AUTO_LOCK_TIMES, DARK_TIMES, LOCK_DARK_TIMES, THEMES};
use crate::system::config::Config;
use crate::system::Backend;

pub const KEYS: [&str; 12] = [
    "brightness",
    "volume",
    "mute",
    "theme",
    "invert",
    "statusbar",
    "screen_off",
    "lock_style",
    "auto_lock",
    "lock_screen_off",
    "lock_date",
    "lock_battery",
];

pub fn names() -> &'static [&'static str] {
    &KEYS
}

#[derive(Debug)]
pub struct SetOutcome {
    pub message: String,
    /// The theme / fonts must be reloaded by the host.
    pub restyle: bool,
}

fn cfg_get(cfg: &Mutex<Config>, k: &str, d: i64) -> i64 {
    cfg.lock().map(|c| c.get_int(k, d)).unwrap_or(d)
}

fn cfg_put(cfg: &Mutex<Config>, k: &str, v: i64) {
    if let Ok(mut c) = cfg.lock() {
        c.set_int(k, v);
        let _ = c.save();
    }
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.trim().to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(format!("\"{v}\" is not on/off")),
    }
}

fn parse_int(v: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let n: i64 = v.trim().trim_end_matches('%').parse().map_err(|_| format!("\"{v}\" is not a number"))?;
    if n < lo || n > hi {
        return Err(format!("{n} is out of range {lo}..{hi}"));
    }
    Ok(n)
}

fn choice(v: &str, names: &[&str]) -> Result<i64, String> {
    let t = v.trim().to_lowercase();
    let t = if t == "paper" { "sky".to_string() } else { t }; // the old name of Sky
    if let Some(i) = names.iter().position(|n| *n == t) {
        return Ok(i as i64);
    }
    Err(format!("\"{v}\" is not one of: {}", names.join(", ")))
}

// The theme files' names (Amber is `theme-black.yaml`); one per entry of `THEMES`.
const THEME_NAMES: [&str; 5] = ["black", "sky", "mobile", "terminal", "platinum"];
const LOCK_STYLE_NAMES: [&str; 4] = ["wall", "world", "ascii", "7seg"];
const ON_OFF: [&str; 2] = ["off", "on"];
const BAR_NAMES: [&str; 3] = ["default", "black", "white"];

pub fn get(key: &str, sys: &dyn Backend, cfg: &Mutex<Config>) -> Result<String, String> {
    Ok(match key {
        "brightness" => {
            let pct = match sys.brightness() {
                Some((v, m)) if m > 0 => (v * 100 / m) as i64,
                _ => cfg_get(cfg, "brightness", 100),
            };
            format!("{pct} (percent, 5-100)")
        }
        "volume" => format!("{} (percent, 0-100)", sys.volume().map(|v| v.0 as i64).unwrap_or_else(|| cfg_get(cfg, "volume", 30))),
        "mute" => if sys.volume().map_or(false, |v| v.1) { "on" } else { "off" }.into(),
        "theme" => THEME_NAMES[cfg_get(cfg, "harbour_theme", 0).clamp(0, THEMES.len() as i64 - 1) as usize].into(),
        "invert" => if cfg_get(cfg, "harbour_theme_invert", 0) == 1 { "on" } else { "off" }.into(),
        "statusbar" => BAR_NAMES[cfg_get(cfg, "harbour_statusbar", 0).clamp(0, 2) as usize].into(),
        "screen_off" => format!("{} (seconds, 0 = never)", cfg_get(cfg, "dark_time", 30)),
        "lock_screen_off" => format!("{} (seconds, 0 = same as screen_off)", cfg_get(cfg, "harbour_lock_dark", 8)),
        "lock_style" => LOCK_STYLE_NAMES[cfg_get(cfg, "harbour_lock_style", 0).clamp(0, 3) as usize].into(),
        "auto_lock" => format!("{} (seconds idle, 0 = never)", cfg_get(cfg, "harbour_lock_auto", 0)),
        "lock_date" => ON_OFF[(cfg_get(cfg, "harbour_lock_date", 1) == 1) as usize].into(),
        "lock_battery" => ON_OFF[(cfg_get(cfg, "harbour_lock_battery", 1) == 1) as usize].into(),
        _ => return Err(format!("unknown setting \"{key}\"; known: {}", names().join(", "))),
    })
}

pub fn set(key: &str, value: &str, sys: &dyn Backend, cfg: &Mutex<Config>) -> Result<SetOutcome, String> {
    let mut restyle = false;
    match key {
        "brightness" => {
            let pct = parse_int(value, 5, 100)?;
            let max = sys.brightness().map(|(_, m)| m).unwrap_or(100);
            sys.set_brightness(((pct as u32) * max / 100).max(1))?;
            cfg_put(cfg, "brightness", pct);
        }
        "volume" => {
            let v = parse_int(value, 0, 100)?;
            sys.set_volume(v as u8)?;
            cfg_put(cfg, "volume", v);
        }
        "mute" => sys.set_mute(parse_bool(value)?)?,
        "theme" => {
            cfg_put(cfg, "harbour_theme", choice(value, &THEME_NAMES).or_else(|_| parse_int(value, 0, THEMES.len() as i64 - 1))?);
            restyle = true;
        }
        "invert" => {
            cfg_put(cfg, "harbour_theme_invert", parse_bool(value)? as i64);
            restyle = true;
        }
        "statusbar" => {
            cfg_put(cfg, "harbour_statusbar", choice(value, &BAR_NAMES)?);
            restyle = true;
        }
        "screen_off" => {
            let s = parse_int(value, 0, 3600)?;
            if !DARK_TIMES.contains(&s) {
                return Err(format!("screen_off must be one of {:?} seconds", DARK_TIMES));
            }
            cfg_put(cfg, "dark_time", s);
        }
        "lock_screen_off" => {
            let s = parse_int(value, 0, 3600)?;
            if !LOCK_DARK_TIMES.contains(&s) {
                return Err(format!("lock_screen_off must be one of {:?} seconds", LOCK_DARK_TIMES));
            }
            cfg_put(cfg, "harbour_lock_dark", s);
        }
        "lock_style" => cfg_put(cfg, "harbour_lock_style", choice(value, &LOCK_STYLE_NAMES)?),
        "auto_lock" => {
            let s = parse_int(value, 0, 3600)?;
            if !AUTO_LOCK_TIMES.contains(&s) {
                return Err(format!("auto_lock must be one of {:?} seconds", AUTO_LOCK_TIMES));
            }
            cfg_put(cfg, "harbour_lock_auto", s);
        }
        "lock_date" => cfg_put(cfg, "harbour_lock_date", parse_bool(value)? as i64),
        "lock_battery" => cfg_put(cfg, "harbour_lock_battery", parse_bool(value)? as i64),
        _ => return Err(format!("unknown setting \"{key}\"; known: {}", names().join(", "))),
    }
    let now = get(key, sys, cfg)?;
    Ok(SetOutcome { message: format!("{key} is now {now}"), restyle })
}

pub fn list_all(sys: &dyn Backend, cfg: &Mutex<Config>) -> String {
    names().iter().map(|k| format!("{k}: {}", get(k, sys, cfg).unwrap_or_default())).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;

    fn cfg(tag: &str) -> (Mutex<Config>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("agent-set-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.json");
        (Mutex::new(Config::load(p.clone())), p)
    }

    #[test]
    fn settings_map_onto_the_settings_app_keys() {
        let (c, p) = cfg("a");
        let sys = MockBackend::new();
        assert!(set("brightness", "50", &sys, &c).unwrap().message.contains("50"));
        assert_eq!(Config::load(p.clone()).get_int("brightness", 0), 50);
        assert!(set("theme", "Sky", &sys, &c).unwrap().restyle);
        assert_eq!(Config::load(p.clone()).get_int("harbour_theme", 9), 1);
        set("invert", "on", &sys, &c).unwrap();
        assert_eq!(Config::load(p.clone()).get_int("harbour_theme_invert", 0), 1);
        set("statusbar", "white", &sys, &c).unwrap();
        set("screen_off", "60", &sys, &c).unwrap();
        set("lock_screen_off", "15", &sys, &c).unwrap();
        assert_eq!(Config::load(p.clone()).get_int("harbour_lock_dark", 0), 15);
        assert_eq!(get("theme", &sys, &c).unwrap(), "sky");
        set("theme", "terminal", &sys, &c).unwrap();
        assert_eq!(get("theme", &sys, &c).unwrap(), "terminal", "every theme has a name");
        // Lock screen settings use the Settings app's harbour_lock_* keys.
        set("lock_style", "ascii", &sys, &c).unwrap();
        set("auto_lock", "300", &sys, &c).unwrap();
        set("lock_date", "off", &sys, &c).unwrap();
        set("lock_battery", "off", &sys, &c).unwrap();
        let saved = Config::load(p.clone());
        assert_eq!(
            (saved.get_int("harbour_lock_style", -1), saved.get_int("harbour_lock_auto", -1), saved.get_int("harbour_lock_date", -1), saved.get_int("harbour_lock_battery", -1)),
            (2, 300, 0, 0)
        );
        assert_eq!(get("lock_style", &sys, &c).unwrap(), "ascii");
        assert!(set("auto_lock", "120", &sys, &c).unwrap_err().contains("one of"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn bad_values_are_refused_in_plain_words() {
        let (c, p) = cfg("b");
        let sys = MockBackend::new();
        assert!(set("brightness", "500", &sys, &c).unwrap_err().contains("out of range"));
        assert!(set("brightness", "x", &sys, &c).unwrap_err().contains("not a number"));
        assert!(set("theme", "neon", &sys, &c).is_err());
        assert!(set("screen_off", "45", &sys, &c).unwrap_err().contains("one of"));
        assert!(set("wifi_password", "x", &sys, &c).unwrap_err().contains("unknown setting"));
        assert!(get("nope", &sys, &c).is_err());
        assert!(list_all(&sys, &c).contains("brightness:"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
