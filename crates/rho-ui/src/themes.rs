//! rho-ui's own light and dark themes, in `themes/rho-ui.json`, which
//! replace GPUI Kit's stock ones. Which one shows follows
//! [`crate::settings::Appearance`].

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::App;

/// Both themes, in GPUI Kit's theme file format.
const THEMES: &str = include_str!("../themes/rho-ui.json");
const DARK: &str = "rho Dark";
const LIGHT: &str = "rho Light";

/// Registers the themes and makes them the ones each mode loads. Call once,
/// after `gpui_kit::init`.
pub(crate) fn install(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    registry
        .load_themes_from_str(THEMES)
        .expect("rho-ui's theme file parses: a test checks it");
    let theme = |name: &str| {
        let theme = registry.themes().get(name);
        Rc::clone(theme.unwrap_or_else(|| panic!("rho-ui's theme file has {name}")))
    };
    let (light, dark) = (theme(LIGHT), theme(DARK));
    let current = Theme::global_mut(cx);
    current.light_theme = light;
    current.dark_theme = dark;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A color GPUI Kit can't read falls back to its default without a
    /// word, so every one is checked here.
    #[test]
    fn every_theme_color_parses() {
        let file: serde_json::Value = serde_json::from_str(THEMES).expect("valid JSON");
        let themes = file["themes"].as_array().expect("a list of themes");
        let names: Vec<&str> = themes.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, [DARK, LIGHT]);
        for theme in themes {
            let colors = theme["colors"].as_object().expect("colors");
            for (key, value) in colors {
                let value = value.as_str().expect("a color string");
                assert!(
                    gpui_kit::component::try_parse_color(value).is_ok(),
                    "{} {key}: {value}",
                    theme["name"]
                );
            }
        }
    }
}
