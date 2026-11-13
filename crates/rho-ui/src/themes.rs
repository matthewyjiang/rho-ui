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
        .expect("rho-ui's theme file parses: `kit_reads_every_key_and_color` checks it");
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
    use gpui_kit::component::{try_parse_color, ThemeSet};
    use serde_json::Value;

    use super::*;

    /// GPUI Kit reads a theme leniently: a color key it doesn't know, or a
    /// color it can't parse, falls back to its default without a word. Every
    /// key must survive a round trip through Kit's own schema, and every
    /// color must parse.
    #[test]
    fn kit_reads_every_key_and_color() {
        let ours: Value = serde_json::from_str(THEMES).expect("valid JSON");
        let set: ThemeSet = serde_json::from_str(THEMES).expect("Kit's theme schema");
        let read = serde_json::to_value(&set).expect("Kit's schema serializes");
        let names: Vec<&str> = set.themes.iter().map(|theme| theme.name.as_ref()).collect();
        assert_eq!(names, [DARK, LIGHT]);
        let pairs = ours["themes"].as_array().into_iter().flatten();
        for (theme, kit) in pairs.zip(read["themes"].as_array().into_iter().flatten()) {
            let name = &theme["name"];
            for (key, value) in theme["colors"].as_object().expect("colors") {
                assert!(!kit["colors"][key].is_null(), "{name}: Kit ignores {key}");
                let value = value.as_str().expect("a color string");
                assert!(try_parse_color(value).is_ok(), "{name} {key}: {value}");
            }
        }
    }

    /// Loading a mode's theme puts back any font, size, radius or shadow
    /// its file sets, which would undo the user's settings at each switch;
    /// see [`crate::settings::Settings::apply`].
    #[test]
    fn the_themes_leave_fonts_and_sizes_alone() {
        let set: ThemeSet = serde_json::from_str(THEMES).expect("Kit's theme schema");
        for theme in &set.themes {
            let fonts = (theme.font_size, theme.mono_font_size);
            let families = (&theme.font_family, &theme.mono_font_family);
            assert_eq!(fonts, (None, None), "{}", theme.name);
            assert_eq!(families, (&None, &None), "{}", theme.name);
            let shape = (theme.radius, theme.radius_lg, theme.shadow);
            assert_eq!(shape, (None, None, None), "{}", theme.name);
        }
    }
}
