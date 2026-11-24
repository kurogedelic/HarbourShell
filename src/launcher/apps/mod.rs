//! Apps built on the [`LauncherApp`] contract. To add one: write a module here,
//! add a line to [`REGISTRY`] and a package folder `apps/<id>/` (`<id>.desktop` with
//! `Exec=harbour:<id>`, `icon.png`). The home screen, the app menu, Esc handling
//! and mouse routing then work without further changes.

use crate::launcher::app::AppDef;

pub mod browser;
pub mod camera;
pub mod calendar;
pub mod devices;
pub mod devtree;
pub mod files;
pub mod harbourtalk;
pub mod imageviewer;
pub mod infoscope;
pub mod interceptor;
pub mod monitor;
pub mod music;
pub mod music_eq;
pub mod music_vis;
pub mod radio;
pub mod osc;
pub mod paint;
pub mod paint_model;
pub mod recorder;
pub mod sheet;
pub mod timer;
pub mod tone;
pub mod yacht;

/// Shown on the home screen after the core built-ins, in this order.
pub const REGISTRY: &[AppDef] = &[
    AppDef { id: "files", name: "Files", make: files::make },
    AppDef { id: "timer", name: "Timer", make: timer::make },
    AppDef { id: "music", name: "Music", make: music::make },
    AppDef { id: "radio", name: "Radio", make: radio::make },
    AppDef { id: "voicememo", name: "Recorder", make: recorder::make },
    AppDef { id: "images", name: "Images", make: imageviewer::make },
    AppDef { id: "camera", name: "Camera", make: camera::make },
    AppDef { id: "paint", name: "Paint", make: paint::make },
    AppDef { id: "sheet", name: "Sheet", make: sheet::make },
    AppDef { id: "browser", name: "Browser", make: browser::make },
    AppDef { id: "calendar", name: "Calendar", make: calendar::make },
    AppDef { id: "interceptor", name: "Interceptor", make: interceptor::make },
    AppDef { id: "infoscope", name: "Infoscope", make: infoscope::make },
    AppDef { id: "yacht", name: "Yacht", make: yacht::make },
    AppDef { id: "harbourtalk", name: "HarbourTalk", make: harbourtalk::make },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique_and_short_enough_for_a_label() {
        let mut seen = std::collections::HashSet::new();
        for d in REGISTRY {
            assert!(seen.insert(d.name), "duplicate app {}", d.name);
            assert!(seen.insert(d.id), "duplicate id {}", d.id);
            assert!(d.name.len() <= 12, "{} is too long for an icon label", d.name);
            let _ = (d.make)(); // every app can be constructed without hardware
        }
    }
}
