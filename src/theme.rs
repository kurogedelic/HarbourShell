use crate::gfx::Color;
use serde::Deserialize;
use std::fs;

const DEFAULT_PALETTE: [&str; 4] = ["#000000", "#3a2108", "#9a5a12", "#ffb040"];

#[derive(Debug, Deserialize)]
pub struct ThemeColors {
    #[serde(default = "default_palette")]
    pub palette: Vec<String>,

    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default)]
    pub secondary: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub muted: Option<String>,
    #[serde(default)]
    pub highlight: Option<String>,
    #[serde(default)]
    pub shadow: Option<String>,
    #[serde(default)]
    pub topbar_bg: Option<String>,

    // --- widget tone（Classic 風ベベル用） ---
    #[serde(default)]
    pub widget_face: Option<String>,
    #[serde(default)]
    pub widget_face_active: Option<String>,
    #[serde(default)]
    pub widget_outline: Option<String>,
    #[serde(default)]
    pub widget_outline_dim: Option<String>,
    #[serde(default)]
    pub widget_highlight: Option<String>,
    #[serde(default)]
    pub widget_shadow: Option<String>,
    #[serde(default)]
    pub titlebar_stripe_a: Option<String>,
    #[serde(default)]
    pub titlebar_stripe_b: Option<String>,

    /// Sub-colours (see docs/DESIGN.md): `home_bg`, `home_bg_alt`, `surface_alt`,
    /// `surface_sunken`, `line`, `text_faint`, `accent_soft`. Missing ones fall
    /// back to the closest base colour, so old theme files keep working.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, String>,
}

fn default_palette() -> Vec<String> {
    DEFAULT_PALETTE
        .iter()
        .map(|color| color.to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
}

impl Theme {
    pub fn load_from_yaml(path: &str) -> Result<Self, String> {
        let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let theme: Theme = serde_yaml::from_str(&content).map_err(|e| e.to_string())?;
        Ok(theme)
    }

    pub fn parse_hex_color(hex: &str) -> Result<Color, String> {
        let hex = hex.trim_start_matches('#');
        if hex.len() != 6 {
            return Err("Invalid hex color format".to_string());
        }

        let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| "Invalid hex color")?;
        let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| "Invalid hex color")?;
        let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| "Invalid hex color")?;

        Ok(Color::RGB(r, g, b))
    }
}

pub struct ThemeManager {
    pub theme: Theme,
}

impl ThemeManager {
    pub fn new(theme: Theme) -> Self {
        ThemeManager { theme }
    }

    /// 色キー名から Color を取得する。未知のキーは白を返す。
    pub fn get_color(&self, color_key: &str) -> Color {
        let colors = &self.theme.colors;

        // Sub-colours: the theme's own value, or a base colour as the fallback.
        let alias = match color_key {
            "home_bg" => Some("background"),
            "home_bg_alt" => Some("shadow"),
            "surface_alt" => Some("widget_face_active"),
            "surface_sunken" => Some("background"),
            "line" => Some("muted"),
            "text_faint" => Some("muted"),
            "accent_soft" => Some("surface_alt"),
            "select_bg" => Some("foreground"),
            "select_fg" => Some("background"),
            "icon_white" => Some("select_bg"),
            "tile_sel_bg" => Some("select_bg"),
            "tile_sel_fg" => Some("select_fg"),
            _ => None,
        };
        if let Some(hex) = colors.extra.get(color_key) {
            if let Ok(c) = Theme::parse_hex_color(hex) {
                return c;
            }
        }
        if let Some(a) = alias {
            return self.get_color(a);
        }
        let fallback_step = match color_key {
            "background" | "widget_face" => Some(0),
            "shadow" | "widget_shadow" | "titlebar_stripe_b" => Some(1),
            "secondary" | "muted" | "topbar_bg" | "widget_face_active" | "widget_outline_dim"
            | "titlebar_stripe_a" => Some(2),
            "foreground" | "primary" | "accent" | "highlight" | "widget_outline"
            | "widget_highlight" => Some(3),
            "palette0" | "tone0" => Some(0),
            "palette1" | "tone1" => Some(1),
            "palette2" | "tone2" => Some(2),
            "palette3" | "tone3" => Some(3),
            _ => None,
        };

        let hex = match color_key {
            "background" => colors.background.as_deref(),
            "foreground" => colors.foreground.as_deref(),
            "primary" => colors.primary.as_deref(),
            "secondary" => colors.secondary.as_deref(),
            "accent" => colors.accent.as_deref(),
            "muted" => colors.muted.as_deref(),
            "highlight" => colors.highlight.as_deref(),
            "shadow" => colors.shadow.as_deref(),
            "topbar_bg" => colors.topbar_bg.as_deref(),
            "widget_face" => colors.widget_face.as_deref(),
            "widget_face_active" => colors.widget_face_active.as_deref(),
            "widget_outline" => colors.widget_outline.as_deref(),
            "widget_outline_dim" => colors.widget_outline_dim.as_deref(),
            "widget_highlight" => colors.widget_highlight.as_deref(),
            "widget_shadow" => colors.widget_shadow.as_deref(),
            "titlebar_stripe_a" => colors.titlebar_stripe_a.as_deref(),
            "titlebar_stripe_b" => colors.titlebar_stripe_b.as_deref(),
            _ => None,
        }
        .or_else(|| fallback_step.and_then(|step| self.palette_hex(step)));

        let Some(hex) = hex else {
            return Color::RGB(255, 255, 255);
        };
        Theme::parse_hex_color(hex).unwrap_or(Color::RGB(255, 255, 255))
    }

    fn palette_hex(&self, step: usize) -> Option<&str> {
        self.theme
            .colors
            .palette
            .get(step)
            .map(String::as_str)
            .or_else(|| DEFAULT_PALETTE.get(step).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color_tuple(color: Color) -> (u8, u8, u8) {
        (color.r, color.g, color.b)
    }

    #[test]
    fn semantic_colors_fall_back_to_palette_steps() {
        let theme: Theme = serde_yaml::from_str(
            r##"
name: test
colors:
  palette:
    - "#000000"
    - "#111111"
    - "#222222"
    - "#333333"
"##,
        )
        .unwrap();
        let tm = ThemeManager::new(theme);

        assert_eq!(color_tuple(tm.get_color("background")), (0, 0, 0));
        assert_eq!(color_tuple(tm.get_color("shadow")), (17, 17, 17));
        assert_eq!(color_tuple(tm.get_color("muted")), (34, 34, 34));
        assert_eq!(color_tuple(tm.get_color("foreground")), (51, 51, 51));
        assert_eq!(color_tuple(tm.get_color("palette2")), (34, 34, 34));
    }

    #[test]
    fn sub_colours_fall_back_to_base_colours_and_can_be_overridden() {
        let theme: Theme = serde_yaml::from_str(
            r##"
name: test
colors:
  palette: ["#000000", "#111111", "#222222", "#333333"]
  background: "#010101"
  widget_face_active: "#020202"
  muted: "#030303"
  line: "#0a0b0c"
"##,
        )
        .unwrap();
        let tm = ThemeManager::new(theme);
        assert_eq!(color_tuple(tm.get_color("line")), (10, 11, 12), "explicit sub-colour");
        assert_eq!(color_tuple(tm.get_color("home_bg")), (1, 1, 1), "falls back to the desktop");
        assert_eq!(color_tuple(tm.get_color("surface_alt")), (2, 2, 2), "falls back to the paper");
        assert_eq!(color_tuple(tm.get_color("accent_soft")), (2, 2, 2), "falls back through surface_alt");
        assert_eq!(color_tuple(tm.get_color("text_faint")), (3, 3, 3));
    }

    #[test]
    fn explicit_semantic_color_overrides_palette_step() {
        let theme: Theme = serde_yaml::from_str(
            r##"
name: test
colors:
  palette:
    - "#000000"
    - "#111111"
    - "#222222"
    - "#333333"
  foreground: "#abcdef"
"##,
        )
        .unwrap();
        let tm = ThemeManager::new(theme);

        assert_eq!(color_tuple(tm.get_color("foreground")), (171, 205, 239));
        assert_eq!(color_tuple(tm.get_color("highlight")), (51, 51, 51));
    }
}

#[cfg(test)]
mod launcher_theme_tests {
    use super::*;

    fn lum(c: Color) -> f32 {
        0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32
    }

    #[test]
    fn every_launcher_theme_defines_readable_sub_colours() {
        for name in ["black", "paper", "mobile", "terminal", "black-invert", "paper-invert", "mobile-invert", "terminal-invert"] {
            let path = format!("{}/assets/themes/theme-{name}.yaml", env!("CARGO_MANIFEST_DIR"));
            let tm = ThemeManager::new(Theme::load_from_yaml(&path).unwrap());
            for key in ["home_bg", "home_bg_alt", "surface_alt", "surface_sunken", "line", "text_faint", "accent_soft", "icon_white"] {
                assert!(tm.theme.colors.extra.contains_key(key), "{name} lacks {key}");
            }
            let paper = tm.get_color("widget_face_active");
            let ink = tm.get_color("foreground");
            let d = |a: Color, b: Color| (lum(a) - lum(b)).abs();
            assert!(d(tm.get_color("text_faint"), paper) >= 40.0, "{name}: faint text must stay readable on paper");
            assert!(d(tm.get_color("text_faint"), ink) >= 25.0, "{name}: faint text must differ from ink");
            assert!(d(tm.get_color("line"), paper) >= 40.0, "{name}: soft lines must show on paper");
            let alt = d(tm.get_color("surface_alt"), paper);
            assert!((4.0..=70.0).contains(&alt), "{name}: surface_alt is a subtle step from paper, got {alt}");
            assert!(d(tm.get_color("accent_soft"), paper) >= 10.0, "{name}: hover tone must be visible");
            // The home background is subtler than the desktop's contrast against ink.
            assert!(d(tm.get_color("home_bg"), ink) >= 40.0, "{name}: icons must stand out on the home background");
        }
    }
}
