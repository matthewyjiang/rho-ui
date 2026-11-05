//! rho-ui's own settings: UI scale and fonts. They live in
//! `$XDG_CONFIG_HOME/rho-ui/settings` (by default `~/.config/rho-ui/settings`),
//! one `key = value` per line, and apply to GPUI Kit's theme.

use std::path::PathBuf;

use gpui_kit::component::Theme;
use gpui_kit::{px, App, Global, SharedString};

use crate::store;

/// The UI font when none is set.
pub(crate) const DEFAULT_UI_FONT: &str = "Noto Sans";
/// The code font when none is set.
pub(crate) const DEFAULT_CODE_FONT: &str = "JetBrainsMono Nerd Font";

/// GPUI Kit's base sizes, which the scale multiplies. The UI size is also
/// the rem, so every rem-based length follows it.
const BASE_UI_SIZE: f32 = 16.;
const BASE_CODE_SIZE: f32 = 13.;

/// How large text and controls are, in percent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UiScale(u16);

impl UiScale {
    /// The scales on offer.
    pub(crate) const CHOICES: [UiScale; 7] = [
        UiScale(80),
        UiScale(90),
        UiScale(100),
        UiScale(110),
        UiScale(125),
        UiScale(150),
        UiScale(175),
    ];

    /// One of [`Self::CHOICES`], if `percent` is.
    pub(crate) fn from_percent(percent: u16) -> Option<Self> {
        Self::CHOICES.into_iter().find(|scale| scale.0 == percent)
    }

    pub(crate) fn percent(self) -> u16 {
        self.0
    }

    fn factor(self) -> f32 {
        f32::from(self.0) / 100.
    }
}

impl Default for UiScale {
    fn default() -> Self {
        UiScale(100)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) ui_scale: UiScale,
    pub(crate) ui_font: SharedString,
    pub(crate) code_font: SharedString,
    /// Where [`Self::save`] writes; `None` keeps changes in memory (tests).
    file: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_scale: UiScale::default(),
            ui_font: DEFAULT_UI_FONT.into(),
            code_font: DEFAULT_CODE_FONT.into(),
            file: None,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Reads the settings file. Missing or unreadable values keep their
    /// defaults.
    pub(crate) fn load() -> Self {
        let file = store::xdg_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("settings"));
        let mut settings = file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse(&text))
            .unwrap_or_default();
        settings.file = file;
        settings
    }

    /// Writes the settings file. Best effort: a failed write only loses the
    /// change at the next start.
    pub(crate) fn save(&self) {
        let Some(path) = &self.file else { return };
        let text = format!(
            "ui_scale = {}\nui_font = {}\ncode_font = {}\n",
            self.ui_scale.percent(),
            self.ui_font,
            self.code_font
        );
        store::write_atomic(path, &text);
    }

    /// Makes `self` the current settings and applies them to the theme and
    /// every window.
    pub(crate) fn apply(self, cx: &mut App) {
        let installed = cx.text_system().all_font_names();
        // A font that isn't installed falls back to the default, and if that
        // is missing too, to GPUI Kit's own choice: a missing family can
        // break text layout.
        let pick = |wanted: &SharedString, default: &str| {
            [wanted.as_ref(), default]
                .into_iter()
                .find(|family| installed.iter().any(|name| name == family))
                .map(SharedString::from)
        };
        let ui_font = pick(&self.ui_font, DEFAULT_UI_FONT);
        let code_font = pick(&self.code_font, DEFAULT_CODE_FONT);
        let factor = self.ui_scale.factor();
        // `update`, not `global_mut`: it also copies fonts and sizes into
        // the Base theme that Markdown code blocks read, and refreshes every
        // window.
        Theme::update(cx, |theme| {
            theme.font_size = px(BASE_UI_SIZE * factor);
            theme.mono_font_size = px(BASE_CODE_SIZE * factor);
            if let Some(family) = ui_font {
                theme.font_family = family;
            }
            if let Some(family) = code_font {
                theme.mono_font_family = family;
            }
        });
        cx.set_global(self);
    }
}

/// Reads `key = value` lines. Unknown keys and bad values are skipped.
fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "ui_scale" => {
                if let Some(scale) = value.parse().ok().and_then(UiScale::from_percent) {
                    settings.ui_scale = scale;
                }
            }
            "ui_font" if !value.is_empty() => settings.ui_font = value.to_owned().into(),
            "code_font" if !value.is_empty() => settings.code_font = value.to_owned().into(),
            _ => {}
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_keeps_defaults_for_missing_or_bad_values() {
        let settings = parse("ui_scale = 125\ncode_font = Fira Code\nui_font =\nbogus = 1\n");
        assert_eq!(settings.ui_scale, UiScale(125));
        assert_eq!(settings.code_font, "Fira Code");
        assert_eq!(settings.ui_font, DEFAULT_UI_FONT);
        assert_eq!(parse("ui_scale = 133").ui_scale, UiScale::default());
    }
}
