//! Screen sleep through the backlight: the level to come back to is kept in a small file while the
//! backlight is down, so a killed or crashed process (or a service restart) can never leave the
//! panel dark. Only sleep goes to 0; the Settings slider keeps its own floor.

use std::path::Path;

use super::Backend;

/// Level used when the backlight is found at 0 with nothing saved (percent of max).
const FALLBACK_PCT: u32 = 50;

/// Remember `level` on disk before the backlight is lowered.
pub fn save(path: &Path, level: u32) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, level.to_string());
}

/// Backlight to 0 (the level must already be saved). Errors (read-only sysfs) are harmless.
pub fn backlight_off(sys: &dyn Backend) {
    let _ = sys.set_brightness(0);
}

/// Put `level` back and forget the saved file.
pub fn restore(sys: &dyn Backend, path: &Path, level: u32) {
    let _ = sys.set_brightness(level);
    let _ = std::fs::remove_file(path);
}

/// At start-up: a leftover file means the last process died while the backlight was down; put its
/// level back. With no file but a backlight at 0 (never intended while awake), use a mid level.
/// Returns the level written, if any.
pub fn recover(sys: &dyn Backend, path: &Path) -> Option<u32> {
    let (cur, max) = sys.brightness()?;
    let saved = std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse::<u32>().ok());
    let level = match saved {
        Some(v) => Some(v.clamp(1, max.max(1))),
        None if cur == 0 => Some((max * FALLBACK_PCT / 100).max(1)),
        None => None,
    };
    let _ = std::fs::remove_file(path);
    let level = level?;
    let _ = sys.set_brightness(level);
    Some(level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("harbour-sleep-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn sleep_saves_the_level_and_goes_dark_then_wake_restores_it() {
        let m = MockBackend::new();
        m.set_brightness(70).unwrap();
        let p = tmp("a");
        let (v, _) = m.brightness().unwrap();
        save(&p, v);
        backlight_off(&m);
        assert_eq!(m.brightness(), Some((0, 100)));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "70");
        restore(&m, &p, v);
        assert_eq!(m.brightness(), Some((70, 100)));
        assert!(!p.exists());
        assert_eq!(m.brightness_log(), vec![70, 0, 70]);
    }

    #[test]
    fn a_stale_saved_level_is_restored_at_startup() {
        let m = MockBackend::new();
        m.set_brightness(0).unwrap();
        let p = tmp("b");
        save(&p, 45);
        assert_eq!(recover(&m, &p), Some(45));
        assert_eq!(m.brightness(), Some((45, 100)));
        assert!(!p.exists());
    }

    #[test]
    fn startup_with_nothing_saved_only_rescues_a_dark_backlight() {
        let m = MockBackend::new();
        let p = tmp("c");
        m.set_brightness(30).unwrap();
        assert_eq!(recover(&m, &p), None);
        assert_eq!(m.brightness(), Some((30, 100)));
        m.set_brightness(0).unwrap();
        assert_eq!(recover(&m, &p), Some(50));
        assert_eq!(m.brightness(), Some((50, 100)));
    }

    #[test]
    fn a_slider_change_while_awake_is_not_touched_by_sleep_state() {
        let m = MockBackend::new();
        m.set_brightness(5).unwrap(); // slider minimum stays visible
        assert_eq!(m.brightness(), Some((5, 100)));
        assert_eq!(recover(&m, &tmp("d")), None);
        assert_eq!(m.brightness(), Some((5, 100)));
    }
}
