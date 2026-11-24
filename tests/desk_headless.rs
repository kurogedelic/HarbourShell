//! The desk end to end (docs/DESIGN.md "Desk"): a bigger screen shows windows. The launcher runs
//! headless on the mock backend with a script; `expect:` steps check the focused screen and the
//! number of windows. Script clicks give desk coordinates.

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("harbour-desk-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["data", "agent", "shots", "ime"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    std::fs::write(d.join("config.json"), "{}").unwrap();
    d
}

/// Run one script on a desk of `size`; returns the shots directory's files (name, bytes).
fn run(name: &str, size: &str, script: &str) -> Vec<(String, Vec<u8>)> {
    run_with(name, size, script, None)
}

/// [`run`] with Home folders (`folders.json` text).
fn run_with(name: &str, size: &str, script: &str, folders: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let d = scratch(name);
    if let Some(f) = folders {
        std::fs::write(d.join("folders.json"), f).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_harbour-shell"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARBOUR_ROOT")
        .env_remove("HARBOUR_SCRIPT_FILE")
        .env("HARBOUR_SCREEN", size)
        .env("HARBOUR_HEADLESS", "1")
        .env("HARBOUR_MOCK", "1")
        .env("HARBOUR_NO_EVDEV", "1")
        .env("HARBOUR_CONFIG", d.join("config.json"))
        .env("HARBOUR_DATA", d.join("data"))
        .env("HARBOUR_AGENT_DIR", d.join("agent"))
        .env("HARBOUR_IME_DIR", d.join("ime"))
        .env("HARBOUR_SHOT_DIR", d.join("shots"))
        .env("HARBOUR_SCRIPT", script)
        .env("HARBOUR_ORDER_FILE", d.join("order.txt"))
        .output()
        .expect("run harbour-shell");
    let err = String::from_utf8_lossy(&out.stderr);
    let report: Vec<&str> = err.lines().filter(|l| l.starts_with("script:") || l.starts_with("state:")).collect();
    let shots = std::fs::read_dir(d.join("shots"))
        .map(|it| it.flatten().map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap_or_default())).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&d);
    assert!(out.status.success(), "{name}: exit {:?}\n{}", out.status.code(), report.join("\n"));
    assert!(!report.iter().any(|l| l.contains("unknown key") || l.contains("no tile")), "{name}: bad script\n{}", report.join("\n"));
    shots
}

#[test]
fn apps_open_in_windows_and_going_home_closes_them() {
    // Home is the desk itself (no window); (300, 440) is empty desk.
    run(
        "open-close",
        "800x480",
        "wait:200,expect:screen=home&windows=0,\
         goto:files,Return,wait:900,expect:screen=files&windows=1,\
         click:300:440,wait:100,expect:screen=home&windows=1,\
         goto:calculator,Return,wait:900,expect:screen=calculator&windows=2,\
         ctrl+`,wait:100,expect:screen=files,\
         hold:Escape:1100,wait:900,expect:screen=calculator&windows=1",
    );
}

#[test]
fn the_close_box_and_a_click_on_a_window_behind() {
    // Files opens at (8, 24), Calendar cascades to (24, 40): (12, 100) is Files only. Once Files is
    // in front its close box is at (13..23, 27..37).
    run(
        "close-focus",
        "800x480",
        "wait:200,goto:files,Return,wait:900,click:300:440,goto:calendar,Return,wait:900,expect:screen=calendar&windows=2,\
         click:12:100,wait:100,expect:screen=files,\
         click:300:440,wait:100,expect:screen=home,\
         click:17:31,wait:900,expect:windows=1&screen=calendar",
    );
}

#[test]
fn help_opens_in_a_window_of_its_own_and_esc_closes_it() {
    // Harbour menu (app menu, Left) > Help: the fourth entry while an app is in front.
    run(
        "help",
        "800x480",
        "wait:200,goto:calendar,Return,wait:900,menu,Left,Down,Down,Down,Return,wait:900,expect:windows=2&screen=helpcalendar,\
         Escape,wait:900,expect:windows=1&screen=calendar",
    );
}

#[test]
fn the_shot_is_the_whole_desk() {
    let shots = run("size", "640x360", "wait:200,shot:desk");
    let (_, bmp) = shots.iter().find(|(n, _)| n == "desk.bmp").expect("desk.bmp");
    let w = i32::from_le_bytes(bmp[18..22].try_into().unwrap());
    let h = i32::from_le_bytes(bmp[22..26].try_into().unwrap()).abs();
    assert_eq!((w, h), (640, 360));
}

#[test]
fn the_cardputer_size_has_no_desk() {
    run("no-desk", "320x170", "wait:200,expect:windows=0,goto:files,Return,wait:900,expect:screen=files&windows=0");
}

#[test]
fn a_folder_opens_in_a_window_and_its_apps_in_windows_of_their_own() {
    let folders = r#"{"version":1,"folders":[{"name":"Tools","items":["calculator","paint"]}]}"#;
    run_with(
        "folder",
        "800x480",
        "wait:200,expect:screen=home&windows=0,\
         goto:Tools,Return,wait:900,expect:screen=tools&windows=1,\
         Return,wait:900,expect:screen=calculator&windows=2,\
         hold:Escape:1100,wait:900,expect:screen=tools&windows=1,\
         Escape,wait:900,expect:screen=home&windows=0",
        Some(folders),
    );
    // On the Cardputer's own screen a folder is still a page of Home.
    run_with("folder-small", "320x170", "wait:200,goto:Tools,Return,wait:900,expect:screen=home", Some(folders));
}

#[test]
fn a_window_behind_closes_from_its_own_close_box() {
    // Files opens at the top-left of the free area and Calendar cascades over it: Files' close box
    // (drawn on every window) stays in view at (22, 33) and closes Files without touching Calendar.
    run(
        "back-close",
        "800x480",
        "wait:200,goto:files,Return,wait:900,click:300:470,wait:100,\
         goto:calendar,Return,wait:900,expect:screen=calendar&windows=2,\
         click:22:33,wait:900,expect:screen=calendar&windows=1",
    );
}
