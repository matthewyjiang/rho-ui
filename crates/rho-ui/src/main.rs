//! rho-ui: the desktop UI for the rho coding agent.
//!
//! `rho-ui [DIR] [-- AGENT COMMAND...]` opens one window. DIR is the
//! workspace for new sessions (default: the current directory); the agent
//! command defaults to `rho acp`.

mod app;
mod approval;
mod composer;
mod draft;
mod kinetic;
mod repo;
mod session_list;
mod session_settings;
mod session_view;
mod settings;
mod settings_view;
mod shelf;
mod sidebar;
mod store;
mod time;
mod transcript;
mod turn_times;
#[cfg(test)]
mod ui_tests;
mod work;
mod workspaces;

use std::{borrow::Cow, path::PathBuf};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{
    px, size, App, AppContext as _, AssetSource, KeyBinding, SharedString, TitlebarOptions,
    WindowBounds, WindowOptions,
};
use rho_ui_core::AgentCommand;

use crate::{
    app::{LaunchConfig, RhoApp},
    settings::Settings,
    shelf::Shelves,
    turn_times::TurnTimes,
};

gpui_kit::actions!(
    rho_ui,
    [
        /// Start a session in the launch workspace.
        NewSession,
        /// Stop the running turn in the focused session.
        StopTurn,
        /// Open the settings screen.
        OpenSettings,
        /// Close the settings screen.
        CloseSettings,
        /// Quit the app.
        Quit
    ]
);

// Icons GPUI Kit's default bundle leaves out, borrowed from its full catalog.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        GitBranch,
        AlarmClock,
        Archive,
        SquarePen,
        CircleDashed,
        Terminal,
        Brain,
        Wrench,
        Shield,
        Zap
    ]
);

/// GPUI Kit's default icons plus [`ExtraIcons`], all borrowed from static
/// storage.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(icon) => Ok(Some(icon)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

const USAGE: &str = "usage: rho-ui [DIR] [-- AGENT COMMAND...]";

/// Parses `[DIR] [-- AGENT COMMAND...]`. Returns the usage text on bad input.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<LaunchConfig, String> {
    let mut args = args.into_iter();
    let mut dir: Option<PathBuf> = None;
    let mut command = AgentCommand::rho();
    let mut command_line = "rho acp".to_owned();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                let rest = args.by_ref();
                let program = rest
                    .next()
                    .ok_or_else(|| format!("missing agent command after --\n{USAGE}"))?;
                let args: Vec<String> = rest.collect();
                command_line = std::iter::once(program.as_str())
                    .chain(args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                command = AgentCommand::new(program, args);
                break;
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            path if dir.is_none() => dir = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}\n{USAGE}")),
        }
    }
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir()
            .map_err(|error| format!("can't read the current directory: {error}"))?,
    };
    let workspace = dir
        .canonicalize()
        .map_err(|error| format!("can't open {}: {error}", dir.display()))?;
    if !workspace.is_dir() {
        return Err(format!("{} is not a directory", workspace.display()));
    }
    Ok(LaunchConfig {
        workspace,
        command,
        command_line,
    })
}

#[expect(
    clippy::print_stderr,
    reason = "a command-line error goes to stderr before any window exists"
)]
fn main() {
    let launch = match parse_args(std::env::args().skip(1)) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    gpui_kit::application().with_assets(Assets).run(move |cx| {
        init(cx);
        Settings::load().apply(cx);
        cx.set_global(TurnTimes::load());
        cx.on_action(|_: &Quit, cx| cx.quit());
        let options = window_options(cx);
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| RhoApp::new(launch, Shelves::load(time::now_unix()), window, cx))
        })
        .expect("failed to open the rho-ui window");
        cx.activate(true);
    });
}

/// App-wide setup shared by `main` and the UI tests: GPUI Kit, the theme
/// with default settings, and key bindings.
fn init(cx: &mut App) {
    gpui_kit::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    Settings::default().apply(cx);
    // In memory until `main` loads the saved times; tests keep it so.
    cx.set_global(TurnTimes::default());
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewSession, Some("RhoApp")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("RhoApp")),
        KeyBinding::new("escape", StopTurn, Some("SessionView")),
        KeyBinding::new("escape", CloseSettings, Some("SettingsView")),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

/// The main window: centered, large enough for the sidebar and a readable
/// transcript side by side.
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1200.), px(800.)), cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("rho".into()),
            ..Default::default()
        }),
        app_id: Some("rho-ui".to_owned()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LaunchConfig, String> {
        parse_args(args.iter().map(std::string::ToString::to_string))
    }

    #[test]
    fn agent_command_follows_double_dash() {
        let launch = parse(&["/tmp", "--", "rho", "--model", "x", "acp"]).unwrap();
        assert_eq!(
            launch.workspace,
            PathBuf::from("/tmp").canonicalize().unwrap()
        );
        assert_eq!(launch.command_line, "rho --model x acp");
    }

    #[test]
    fn rejects_extra_positional_and_empty_command() {
        assert!(parse(&["/tmp", "/var"]).is_err());
        assert!(parse(&["--"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
