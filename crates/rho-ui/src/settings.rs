//! rho-ui's own settings: light or dark, UI scale, fonts, the machines it
//! connects to, and the forge CLIs' programs on each. They live in
//! `settings` in rho-ui's config folder ([`AppDir::Config`]: by default
//! `~/.config/rho-ui`), one `key = value` per line. In a browser they are
//! that file on the server's machine, as `rho-ui serve` shares it. The
//! look applies to GPUI Kit's theme; the app follows the machines.

#[cfg(not(target_family = "wasm"))]
use std::io;
use std::{fmt::Write as _, path::PathBuf};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{px, App, Global, SharedString, WindowAppearance};
#[cfg(not(target_family = "wasm"))]
use rho_ui_core::platform::{self, AppDir};
use rho_ui_core::server::{
    wire::{SettingsFile, StateWrite},
    StateWriter,
};

use crate::{
    forge::{CliPaths, ForgeCli},
    machines::{MachineId, RemoteMachine, LOCAL},
    store,
};

/// The UI font when none is set.
pub(crate) const DEFAULT_UI_FONT: &str = "Noto Sans";
/// The code font when none is set. A browser has it too: the page loads
/// it (`web.rs`).
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

/// Whether rho-ui shows its light or dark theme.
///
/// Dark by default, not [`Appearance::System`]: on Linux the app starts
/// before the desktop portal says light or dark, so a dark desktop would
/// see a light first frame, and with no portal (bare sway or i3) stay
/// light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Appearance {
    /// The one the desktop asks for, changing when it does.
    System,
    Light,
    #[default]
    Dark,
}

impl Appearance {
    pub(crate) const CHOICES: [Appearance; 3] =
        [Appearance::Dark, Appearance::Light, Appearance::System];

    /// What the settings file calls it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    /// What settings call it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::CHOICES
            .into_iter()
            .find(|choice| choice.name() == name)
    }

    /// The theme mode to show while the desktop asks for `system`.
    pub(crate) fn mode(self, system: WindowAppearance) -> ThemeMode {
        match self {
            Appearance::System => system.into(),
            Appearance::Light => ThemeMode::Light,
            Appearance::Dark => ThemeMode::Dark,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) appearance: Appearance,
    pub(crate) ui_scale: UiScale,
    pub(crate) ui_font: SharedString,
    pub(crate) code_font: SharedString,
    /// What the sidebar calls this machine.
    pub(crate) local_label: SharedString,
    /// Where this machine clones projects; see
    /// [`crate::machines::Machine::clone_folder`].
    pub(crate) local_clone_folder: String,
    /// This machine's forge CLIs' programs; see [`CliPaths`].
    pub(crate) local_cli: CliPaths,
    /// The remote machines, in the order added.
    pub(crate) machines: Vec<RemoteMachine>,
    /// Where [`Self::save`] writes.
    saved: Saved,
}

/// Where [`Settings::save`] writes.
#[derive(Clone, Debug, Default)]
enum Saved {
    /// Nowhere: changes last until rho-ui quits (tests, and a file that
    /// couldn't be read).
    #[default]
    Nowhere,
    /// The settings file.
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "a browser has no files")
    )]
    File(PathBuf),
    /// In a browser, the settings file on the server's machine.
    #[cfg_attr(
        not(target_family = "wasm"),
        expect(dead_code, reason = "only a browser shares them")
    )]
    Server(StateWriter),
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            ui_scale: UiScale::default(),
            ui_font: DEFAULT_UI_FONT.into(),
            code_font: DEFAULT_CODE_FONT.into(),
            local_label: LOCAL.into(),
            local_clone_folder: String::new(),
            local_cli: CliPaths::default(),
            machines: Vec::new(),
            saved: Saved::Nowhere,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Reads the settings file. A missing file, or a value it doesn't
    /// understand, keeps the defaults. A file that exists but can't be read
    /// keeps them too, and is never written over: changes then last only
    /// until the app quits.
    #[cfg(not(target_family = "wasm"))]
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn load() -> Self {
        let Some(path) = Self::file() else {
            return Self::default();
        };
        match read(&path) {
            SettingsFile::Missing => Self {
                saved: Saved::File(path),
                ..Self::default()
            },
            SettingsFile::Text { text } => Self {
                saved: Saved::File(path),
                ..parse(&text)
            },
            SettingsFile::Unreadable { why } => {
                eprintln!("rho-ui: {why}; using the defaults, and not saving over it");
                Self::default()
            }
        }
    }

    /// Writes the settings file. Best effort: a failed write says why on
    /// stderr and only loses the change at the next start.
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn save(&self) {
        match &self.saved {
            Saved::Nowhere => {}
            Saved::File(path) => {
                if let Err(error) = store::write_atomic(path, &self.to_text()) {
                    eprintln!("rho-ui: couldn't save {}: {error}", path.display());
                }
            }
            Saved::Server(writer) => writer.write(StateWrite::Settings {
                text: self.to_text(),
            }),
        }
    }

    /// The settings the server's machine keeps, `file`, saved back through
    /// `writer`: a browser's. One that couldn't be read there keeps the
    /// defaults, as [`Self::load`] does, and the console says why.
    #[cfg(target_family = "wasm")]
    pub(crate) fn shared(file: SettingsFile, writer: StateWriter) -> Self {
        match file {
            SettingsFile::Missing => Self {
                saved: Saved::Server(writer),
                ..Self::default()
            },
            SettingsFile::Text { text } => Self {
                saved: Saved::Server(writer),
                ..parse(&text)
            },
            SettingsFile::Unreadable { why } => {
                log::error!("rho-ui: {why}; using the defaults, and not saving over it");
                Self::default()
            }
        }
    }

    /// Where the settings file is: `settings` in rho-ui's config folder.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn file() -> Option<PathBuf> {
        platform::app_dir(AppDir::Config).map(|dir| dir.join("settings"))
    }

    /// The remote machine at `host`, if it is on the list.
    pub(crate) fn machine(&self, host: &str) -> Option<&RemoteMachine> {
        self.machines.iter().find(|machine| machine.host == host)
    }

    pub(crate) fn machine_mut(&mut self, host: &str) -> Option<&mut RemoteMachine> {
        self.machines
            .iter_mut()
            .find(|machine| machine.host == host)
    }

    /// The program of `cli` set for `machine`; empty finds it there.
    pub(crate) fn cli_path(&self, machine: &MachineId, cli: ForgeCli) -> &str {
        if machine.is_local() {
            return self.local_cli.get(cli);
        }
        self.machine(machine.as_str())
            .map_or("", |remote| remote.cli.get(cli))
    }

    /// Sets the program of `cli` for `machine`; empty clears it.
    pub(crate) fn set_cli_path(&mut self, machine: &MachineId, cli: ForgeCli, path: String) {
        if machine.is_local() {
            self.local_cli.set(cli, path);
        } else if let Some(remote) = self.machine_mut(machine.as_str()) {
            remote.cli.set(cli, path);
        }
    }

    /// The settings file's text: `key = value` lines, `local.<program>` for
    /// each of this machine's CLIs with a program set, and for each remote
    /// machine `machine.<host>.label`, `.rho`, `.clone_folder` and
    /// `.<program>`.
    fn to_text(&self) -> String {
        let mut text = format!(
            "appearance = {}\nui_scale = {}\nui_font = {}\ncode_font = {}\nlocal.label = {}\nlocal.clone_folder = {}\n",
            self.appearance.name(),
            self.ui_scale.percent(),
            self.ui_font,
            self.code_font,
            self.local_label,
            self.local_clone_folder,
        );
        // Writing to a String can't fail.
        for (cli, path) in self.local_cli.iter() {
            let _ = writeln!(text, "local.{} = {path}", cli.program());
        }
        for machine in &self.machines {
            let host = &machine.host;
            let (label, rho, folder) = (&machine.label, &machine.rho, &machine.clone_folder);
            // Writing to a String can't fail.
            let _ = write!(
                text,
                "machine.{host}.label = {label}\nmachine.{host}.rho = {rho}\nmachine.{host}.clone_folder = {folder}\n"
            );
            for (cli, path) in machine.cli.iter() {
                let _ = writeln!(text, "machine.{host}.{} = {path}", cli.program());
            }
        }
        text
    }

    /// Makes `self` the current settings and applies them to the theme and
    /// every window.
    pub(crate) fn apply(self, cx: &mut App) {
        // First: loading a mode's theme resets what the theme file sets.
        let mode = self.appearance.mode(cx.window_appearance());
        Theme::change(mode, None, cx);
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

/// Reads the settings file at `path`, telling a missing file from one
/// that exists but can't be read, which nothing may write over.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn read(path: &std::path::Path) -> SettingsFile {
    match std::fs::read_to_string(path) {
        Ok(text) => SettingsFile::Text { text },
        Err(error) if error.kind() == io::ErrorKind::NotFound => SettingsFile::Missing,
        Err(error) => SettingsFile::Unreadable {
            why: format!("couldn't read {}: {error}", path.display()),
        },
    }
}

/// Reads `key = value` lines. Unknown keys and bad values are skipped. A
/// remote machine is listed where its first line is.
fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "appearance" => {
                if let Some(appearance) = Appearance::from_name(value) {
                    settings.appearance = appearance;
                }
            }
            "ui_scale" => {
                if let Some(scale) = value.parse().ok().and_then(UiScale::from_percent) {
                    settings.ui_scale = scale;
                }
            }
            "ui_font" if !value.is_empty() => settings.ui_font = value.to_owned().into(),
            "code_font" if !value.is_empty() => settings.code_font = value.to_owned().into(),
            "local.label" if !value.is_empty() => settings.local_label = value.to_owned().into(),
            "local.clone_folder" => value.clone_into(&mut settings.local_clone_folder),
            key => {
                if let Some(cli) = key.strip_prefix("local.").and_then(ForgeCli::from_program) {
                    settings.local_cli.set(cli, value.to_owned());
                } else if let Some((host, field)) = key
                    .strip_prefix("machine.")
                    .and_then(|rest| rest.rsplit_once('.'))
                {
                    machine_field(&mut settings.machines, host, field, value);
                }
            }
        }
    }
    settings
}

/// Sets `field` of the remote machine at `host`, listing it if new. A host
/// that can't name a machine is skipped.
fn machine_field(machines: &mut Vec<RemoteMachine>, host: &str, field: &str, value: &str) {
    if crate::machines::host_problem(host).is_some() {
        return;
    }
    let ix = machines.iter().position(|machine| machine.host == host);
    let ix = ix.unwrap_or_else(|| {
        machines.push(RemoteMachine::new(host));
        machines.len() - 1
    });
    let machine = &mut machines[ix];
    match field {
        "label" => value.clone_into(&mut machine.label),
        "rho" if !value.is_empty() => value.clone_into(&mut machine.rho),
        "clone_folder" => value.clone_into(&mut machine.clone_folder),
        program => {
            if let Some(cli) = ForgeCli::from_program(program) {
                machine.cli.set(cli, value.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_keeps_defaults_for_missing_or_bad_values() {
        let settings = parse(
            "appearance = light\nui_scale = 125\ncode_font = Fira Code\nui_font =\nbogus = 1\n",
        );
        assert_eq!(settings.appearance, Appearance::Light);
        assert_eq!(settings.ui_scale, UiScale(125));
        assert_eq!(settings.code_font, "Fira Code");
        assert_eq!(settings.ui_font, DEFAULT_UI_FONT);
        assert_eq!(parse("ui_scale = 133").ui_scale, UiScale::default());
        assert_eq!(parse("appearance = sepia").appearance, Appearance::Dark);
    }

    #[test]
    fn machines_survive_a_save_in_the_order_added() {
        let mut settings = Settings {
            local_label: "laptop".into(),
            local_clone_folder: "~/code".to_owned(),
            ..Settings::default()
        };
        settings
            .local_cli
            .set(ForgeCli::Gh, "/opt/gh/bin/gh".to_owned());
        let mut workstation = RemoteMachine::new("workstation");
        workstation.label = "Workstation".to_owned();
        workstation.rho = "~/.cargo/bin/rho".to_owned();
        workstation.clone_folder = "/scratch/me".to_owned();
        workstation
            .cli
            .set(ForgeCli::Gh, "~/.local/bin/gh".to_owned());
        settings.machines = vec![workstation, RemoteMachine::new("me@node0.example.edu")];
        let read = parse(&settings.to_text());
        assert_eq!(read.machines, settings.machines);
        assert_eq!(read.local_label, "laptop");
        assert_eq!(read.local_clone_folder, "~/code");
        assert_eq!(read.local_cli, settings.local_cli);
        assert_eq!(read.machines[1].shown_label(), "me@node0.example.edu");
        assert!(
            parse("machine.local.label = x\nmachine.-x.rho = y\n")
                .machines
                .is_empty(),
            "hosts that can't name a machine are skipped"
        );
    }
}
