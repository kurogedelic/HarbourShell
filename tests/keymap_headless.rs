//! Host key scheme, end to end: the launcher runs headless on the mock backend with a script, and
//! `expect:` steps check the shell's state (screen, Agent panel, menu, dialog, lock). A failed
//! expect makes the run exit with status 3. See docs/DESIGN.md "Keys".

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("harbour-keymap-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["data", "agent", "shots", "ime"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    std::fs::write(d.join("config.json"), "{}").unwrap();
    d
}

/// Run one script; panics with the launcher's own report when an expect step fails.
fn run(name: &str, script: &str) {
    let d = scratch(name);
    let out = Command::new(env!("CARGO_BIN_EXE_harbour-shell"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARBOUR_ROOT")
        .env_remove("HARBOUR_SCRIPT_FILE")
        .env("HARBOUR_HEADLESS", "1")
        .env("HARBOUR_MOCK", "1")
        .env("HARBOUR_NO_EVDEV", "1")
        .env("HARBOUR_CONFIG", d.join("config.json"))
        .env("HARBOUR_DATA", d.join("data"))
        .env("HARBOUR_AGENT_DIR", d.join("agent"))
        .env("HARBOUR_IME_DIR", d.join("ime"))
        .env("HARBOUR_SHOT_DIR", d.join("shots"))
        .env("HARBOUR_SCRIPT", script)
        .output()
        .expect("run harbour-shell");
    let err = String::from_utf8_lossy(&out.stderr);
    let report: Vec<&str> = err.lines().filter(|l| l.starts_with("script:") || l.starts_with("state:")).collect();
    let _ = std::fs::remove_dir_all(&d);
    assert!(out.status.success(), "{name}: exit {:?}\n{}", out.status.code(), report.join("\n"));
    assert!(!report.iter().any(|l| l.contains("unknown key") || l.contains("no tile")), "{name}: bad script\n{}", report.join("\n"));
}

/// Open an app from Home (the tile by package id) and wait for it to be up.
fn open(app: &str) -> String {
    format!("goto:{app},Return,wait:900,expect:screen={app}")
}

#[test]
fn agent_panel_opens_with_tab_tab_and_closes_with_esc_or_the_same_gesture() {
    run(
        "agent-home",
        "Tab,Tab,wait:100,expect:agent=1,\
         Escape,wait:100,expect:agent=0,\
         Tab,Tab,wait:100,expect:agent=1,Tab,Tab,wait:100,expect:agent=0,\
         alt+Tab,expect:agent=1,alt+Tab,expect:agent=0,\
         menu,Left,Down,Down,Down,Return,wait:100,expect:agent=1&menu=0,\
         menu,Escape,expect:menu=0&agent=1,Escape,expect:agent=0,\
         alt+Tab,hold:Escape:1100,wait:100,expect:agent=0&screen=home",
    );
}

#[test]
fn double_tap_timing_and_interruptions() {
    run(
        "agent-timing",
        // Too slow (past the 300 ms window), another key in between, a long press: none of them pair up.
        "Tab,wait:450,Tab,wait:450,expect:agent=0,\
         Tab,Right,Tab,wait:450,expect:agent=0,\
         Tab,hold:Tab:400,wait:450,expect:agent=0,\
         Tab,Tab,wait:100,expect:agent=1,Escape",
    );
}

#[test]
fn tab_long_locks_and_unlocks_esc_long_leaves() {
    run(
        "holds",
        &format!(
            "hold:Tab:1200,wait:50,expect:locked=1,Escape,expect:locked=1,hold:Tab:1200,wait:50,expect:locked=0,\
             {},hold:Escape:500,wait:50,expect:screen=calendar,hold:Escape:1100,wait:100,expect:screen=home",
            open("calendar")
        ),
    );
}

#[test]
fn where_tab_belongs_to_the_screen_tab_tab_is_two_tabs_and_alt_tab_opens_the_agent() {
    for app in ["terminal", "texteditor", "monitor", "timer", "voicememo", "interceptor", "hardware", "infoscope", "browser", "radio"] {
        let screen = match app {
            "texteditor" => "editor",
            "hardware" => "devices",
            a => a,
        };
        let open = format!("goto:{app},Return,wait:900,expect:screen={screen}");
        run(
            app,
            &format!(
                "{open},Tab,Tab,wait:450,expect:agent=0&screen={screen},\
                 alt+Tab,expect:agent=1,Tab,Tab,wait:100,expect:agent=0,\
                 alt+Tab,expect:agent=1,Escape,wait:100,expect:agent=0&screen={screen}"
            ),
        );
    }
}

#[test]
fn elsewhere_tab_tab_opens_the_agent_over_the_app() {
    for app in ["calendar", "files", "calculator", "paint"] {
        run(app, &format!("{},Tab,Tab,wait:100,expect:agent=1&screen={app},Escape,wait:100,expect:agent=0&screen={app}", open(app)));
    }
}

#[test]
fn menus_close_with_esc_and_with_the_key_that_opened_them() {
    run(
        "menus",
        "menu,expect:menu=1,Escape,expect:menu=0,\
         menu,menu,expect:menu=0,\
         ctrl+Tab,expect:menu=1,ctrl+Tab,expect:menu=0,\
         menu,Left,expect:menu=1,Escape,expect:menu=0,\
         menu,Left,menu,expect:menu=0",
    );
}

#[test]
fn dialogs_close_with_esc() {
    // Home: New Folder prompt; Text Editor: Save As panel, and the Save changes? card that hold Esc shows.
    run(
        "dialogs",
        "menu,Return,wait:100,expect:modal=1,Escape,wait:100,expect:modal=0&screen=home,\
         goto:texteditor,Return,wait:900,expect:screen=editor,\
         menu,Down,Down,Down,Return,wait:100,expect:modal=1,Escape,wait:100,expect:modal=0&screen=editor,\
         text:hello,hold:Escape:1100,wait:100,expect:modal=1,Escape,wait:100,expect:modal=0&screen=editor",
    );
}

#[test]
fn a_short_esc_never_leaves_an_app_from_its_top_level() {
    for app in ["settings", "music", "timer", "infoscope"] {
        run(app, &format!("{},Escape,Escape,Escape,wait:100,expect:screen={app}", open(app)));
    }
}

#[test]
fn ime_composition_takes_the_first_esc_then_the_panel_closes() {
    run(
        "ime",
        "Tab,Tab,wait:100,expect:agent=1,ctrl+Space,text:ka,Escape,expect:agent=1,Escape,expect:agent=0,ctrl+Space",
    );
}

#[test]
fn tab_bars_switch_with_tab_and_a_held_tab_does_not_step_through_them() {
    run(
        "tabbar",
        &format!(
            "{},expect:has=stopwatch,Tab,expect:has=countdown,shift+Tab,expect:has=stopwatch,\
             down:Tab,rep:Tab,rep:Tab,rep:Tab,wait:1100,up:Tab,wait:50,expect:locked=1&has=countdown,hold:Tab:1200,expect:locked=0",
            open("timer")
        ),
    );
}

