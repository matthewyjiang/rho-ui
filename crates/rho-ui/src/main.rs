//! rho-ui: the desktop UI for the rho coding agent.
//!
//! `rho-ui [DIR] [--rho PROGRAM]` opens one window. DIR is the workspace for
//! new sessions (default: the current directory); PROGRAM is the `rho` that
//! starts the daemon when none runs (default: `rho` on `PATH`).

mod app;
mod approval;
mod attachments;
mod background;
mod composer;
mod draft;
mod fold;
mod kinetic;
mod project_picker;
mod projects;
mod repo;
mod session_list;
mod session_settings;
mod session_view;
mod settings;
mod settings_view;
mod shelf;
mod sidebar;
mod store;
mod themes;
mod time;
mod title_bar;
mod transcript;
mod turn_times;
#[cfg(test)]
mod ui_tests;
mod work;
mod workspaces;

use std::{borrow::Cow, path::PathBuf};

use gpui_kit::{
    px, size, App, AppContext as _, AssetSource, KeyBinding, SharedString, TitlebarOptions,
    WindowBounds, WindowOptions,
};
use rho_ui_core::DaemonTarget;

use crate::{
    app::{LaunchConfig, RhoApp},
    projects::Projects,
    settings::Settings,
    shelf::Shelves,
    sidebar::SidebarLayout,
    store::StateDb,
    turn_times::TurnTimes,
};

gpui_kit::actions!(
    rho_ui,
    [
        /// Start a session in the project on screen, or the last one used.
        NewSession,
        /// Start a session, choosing its project first.
        NewSessionIn,
        /// Stop the running turn in the focused session.
        StopTurn,
        /// Open settings.
        OpenSettings,
        /// Collapse the sidebar to its rail, or open it again.
        ToggleSidebar,
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
        Zap,
        CornerDownRight,
        Clock,
        SquareTerminal,
        ListChecks,
        Workflow,
        Bot,
        Cog,
        Square,
        Paperclip,
        FileCode,
        FileBraces,
        FileSpreadsheet,
        FileArchive,
        FileImage
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

const USAGE: &str = "usage: rho-ui [DIR] [--rho PROGRAM]";

/// Parses `[DIR] [--rho PROGRAM]`. Returns the usage text on bad input.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<LaunchConfig, String> {
    let mut args = args.into_iter();
    let mut dir: Option<PathBuf> = None;
    let mut rho = PathBuf::from("rho");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rho" => {
                rho = args
                    .next()
                    .ok_or_else(|| format!("missing program after --rho\n{USAGE}"))?
                    .into();
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
    let daemon = DaemonTarget::rho(rho).ok_or("can't find the rho home: set RHO_HOME or HOME")?;
    Ok(LaunchConfig { workspace, daemon })
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
        let db = StateDb::open();
        cx.set_global(TurnTimes::load(&db));
        cx.on_action(|_: &Quit, cx| cx.quit());
        let options = window_options(cx);
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let shelves = Shelves::load(&db, time::now_unix());
                let projects = Projects::load(&db);
                let layout = SidebarLayout::load(&db);
                RhoApp::new(launch, shelves, projects, layout, window, cx)
            })
        })
        .expect("failed to open the rho-ui window");
        cx.activate(true);
    });
}

/// App-wide setup shared by `main` and the UI tests: GPUI Kit, the theme
/// with default settings, and key bindings.
fn init(cx: &mut App) {
    gpui_kit::init(cx);
    themes::install(cx);
    Settings::default().apply(cx);
    // In memory until `main` loads the saved times; tests keep it so.
    cx.set_global(TurnTimes::load(&StateDb::in_memory()));
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewSession, Some("RhoApp")),
        KeyBinding::new("ctrl-shift-n", NewSessionIn, Some("RhoApp")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("RhoApp")),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some("RhoApp")),
        KeyBinding::new("escape", StopTurn, Some("SessionView")),
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
    fn rho_program_follows_its_flag() {
        let launch = parse(&["/tmp", "--rho", "/opt/rho/bin/rho"]).unwrap();
        assert_eq!(
            launch.workspace,
            PathBuf::from("/tmp").canonicalize().unwrap()
        );
        // Both sides read the rho home from this process's environment.
        assert_eq!(
            launch.daemon,
            DaemonTarget::rho("/opt/rho/bin/rho").unwrap()
        );
        assert_eq!(
            parse(&["/tmp"]).unwrap().daemon,
            DaemonTarget::rho("rho").unwrap()
        );
    }

    #[test]
    fn rejects_extra_positional_and_missing_program() {
        assert!(parse(&["/tmp", "/var"]).is_err());
        assert!(parse(&["--rho"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
