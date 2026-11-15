//! rho-ui: the desktop UI for the rho coding agent.
//!
//! `rho-ui [DIR] [--rho PROGRAM]` opens one window. DIR is the workspace for
//! new sessions (default: the current directory); PROGRAM is the `rho` that
//! starts the daemon when none runs (default: `rho` on `PATH`).

mod app;
#[cfg(target_os = "macos")]
mod app_menu;
mod attachments;
mod background;
mod background_log;
mod badge;
mod cloning;
mod composer;
mod draft;
mod figures;
mod fold;
mod folders;
mod input_card;
mod kinetic;
mod lightbox;
mod machines;
mod pele;
mod pictures;
mod plan;
mod project_picker;
mod projects;
mod repo;
mod rho_settings;
mod right_panel;
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
mod usage;
mod work;
mod workspaces;

use std::{borrow::Cow, path::PathBuf, rc::Rc};

use gpui_kit::component::input::{Copy, Enter, Paste};
use gpui_kit::{
    px, size, Action, App, AppContext as _, AssetSource, Axis, KeyBinding,
    KeyBindingContextPredicate, NoAction, QuitMode, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions,
};
use rho_ui_core::DaemonTarget;

use crate::{
    app::{LaunchConfig, RhoApp},
    background_log::BackgroundLog,
    machines::RemoteMachine,
    project_picker::LastCheckout,
    projects::Projects,
    right_panel::{Direction, PaneCommand, PanelLayout, TerminalShell, TerminalShortcut},
    settings::Settings,
    sidebar::SidebarState,
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
        /// Show the right panel, or hide it.
        ToggleRightPanel,
        /// Quit the app.
        Quit
    ]
);

// Icons GPUI Kit's default bundle leaves out, borrowed from its full catalog.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        GitBranch,
        Download,
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
        FileImage,
        ListFilter,
        Server,
        Layers,
        Timer,
        SkipForward,
        FileDiff,
        FolderTree,
        Columns2,
        Rows2,
        ClipboardPaste,
        Pencil,
        Pin,
        PinOff,
        Circle,
        CircleDot,
        Trash,
        ZoomIn,
        ZoomOut,
        Scan,
        ImageOff
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
    Ok(LaunchConfig {
        workspace,
        daemon,
        remote: Rc::new(RemoteMachine::ssh_target),
        shell: TerminalShell::from_env(),
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

    // Quit with the window, on macOS too, where GPUI's default keeps the
    // app running with none: rho-ui has one window and no menu bar or Dock
    // menu to open another.
    gpui_kit::application()
        .with_assets(Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            let db = StateDb::open();
            init(cx, &db);
            // pele takes about 100 ms to load: started now, it's ready for the
            // first diagram. Tests load it when they draw one, so the load
            // doesn't slow the tests that time animations.
            pele::start();
            Settings::load().apply(cx);
            let options = window_options(cx);
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let sidebar = SidebarState::load(&db, time::now_unix());
                    let projects = Projects::load(&db);
                    RhoApp::new(launch, sidebar, projects, window, cx)
                })
            })
            .expect("failed to open the rho-ui window");
            cx.activate(true);
        });
}

/// App-wide setup shared by `main` and the UI tests: GPUI Kit, the theme
/// with default settings, key bindings, and the globals that remember
/// things between runs in `db`.
fn init(cx: &mut App, db: &StateDb) {
    gpui_kit::init(cx);
    themes::install(cx);
    Settings::default().apply(cx);
    cx.set_global(TurnTimes::load(db));
    cx.set_global(BackgroundLog::new(db));
    cx.set_global(PanelLayout::load(db));
    cx.set_global(LastCheckout::load(db));
    cx.on_action(|_: &Quit, cx| cx.quit());
    // `secondary` is Cmd on macOS and Ctrl elsewhere.
    cx.bind_keys([
        shortcut("secondary-n", NewSession, Some("RhoApp"), cx),
        shortcut("secondary-shift-n", NewSessionIn, Some("RhoApp"), cx),
        shortcut("secondary-,", OpenSettings, Some("RhoApp"), cx),
        shortcut("secondary-b", ToggleSidebar, Some("RhoApp"), cx),
        shortcut("secondary-alt-b", ToggleRightPanel, Some("RhoApp"), cx),
        shortcut("secondary-q", Quit, None, cx),
    ]);
    cx.bind_keys([
        KeyBinding::new("escape", StopTurn, Some("SessionView")),
        // The terminal takes these keys for the program in it: Tab moves the
        // keyboard elsewhere, and off macOS, where the app's shortcuts take
        // Ctrl, those are the shell's (Ctrl+B and Ctrl+N move the cursor in
        // readline). On macOS they take Cmd, which a terminal leaves to the
        // app.
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-b", NoAction, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-n", NoAction, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-n", NoAction, Some("Terminal")),
        // The terminal's clipboard keys, as Ghostty's: Cmd+C and Cmd+V on
        // macOS; elsewhere Ctrl+Shift+C and Ctrl+Shift+V, leaving Ctrl+C,
        // which Kit's root binds to copy, to the program.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", Copy, Some("Terminal")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", Paste, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-c", Copy, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-v", Paste, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", NoAction, Some("Terminal")),
        // "Input" is the key context of Kit's text fields. Kit binds only
        // Ctrl+V (Cmd+V on macOS) to paste; dictation apps set to paste for
        // terminals, such as Hex, send Ctrl+Shift+V.
        KeyBinding::new("ctrl-shift-v", Paste, Some("Input")),
        // Alt+Enter starts a line, as Kit's Shift+Enter does.
        KeyBinding::new(
            "alt-enter",
            Enter {
                secondary: false,
                shift: true,
            },
            Some("Input"),
        ),
    ]);
    cx.bind_keys(
        TERMINAL_KEYS
            .into_iter()
            .flat_map(|(keys, shortcut)| terminal_keys(keys, shortcut)),
    );
    #[cfg(target_os = "macos")]
    app_menu::install(cx);
}

/// The terminal's keys for tabs and splits: Ghostty's defaults on each
/// platform. They differ by more than Cmd for Ctrl, so `secondary-` can't
/// say them. Cmd+Shift+[ reaches GPUI as `cmd-{`, as macOS reports it.
#[cfg(target_os = "macos")]
const TERMINAL_KEYS: [(&str, TerminalShortcut); 10] = [
    ("cmd-t", TerminalShortcut::NewTab),
    ("cmd-}", TerminalShortcut::NextTab),
    ("cmd-{", TerminalShortcut::PreviousTab),
    ("cmd-d", pane(PaneCommand::Split(Axis::Horizontal))),
    ("cmd-shift-d", pane(PaneCommand::Split(Axis::Vertical))),
    ("cmd-w", pane(PaneCommand::Close)),
    ("cmd-alt-left", pane(PaneCommand::Focus(Direction::Left))),
    ("cmd-alt-right", pane(PaneCommand::Focus(Direction::Right))),
    ("cmd-alt-up", pane(PaneCommand::Focus(Direction::Up))),
    ("cmd-alt-down", pane(PaneCommand::Focus(Direction::Down))),
];

/// The terminal's keys for tabs and splits: Ghostty's defaults on each
/// platform. They differ by more than Cmd for Ctrl, so `secondary-` can't
/// say them.
#[cfg(not(target_os = "macos"))]
const TERMINAL_KEYS: [(&str, TerminalShortcut); 10] = [
    ("ctrl-shift-t", TerminalShortcut::NewTab),
    ("ctrl-pagedown", TerminalShortcut::NextTab),
    ("ctrl-pageup", TerminalShortcut::PreviousTab),
    ("ctrl-shift-o", pane(PaneCommand::Split(Axis::Horizontal))),
    ("ctrl-shift-e", pane(PaneCommand::Split(Axis::Vertical))),
    ("ctrl-shift-w", pane(PaneCommand::Close)),
    ("ctrl-alt-left", pane(PaneCommand::Focus(Direction::Left))),
    ("ctrl-alt-right", pane(PaneCommand::Focus(Direction::Right))),
    ("ctrl-alt-up", pane(PaneCommand::Focus(Direction::Up))),
    ("ctrl-alt-down", pane(PaneCommand::Focus(Direction::Down))),
];

const fn pane(command: PaneCommand) -> TerminalShortcut {
    TerminalShortcut::Pane(command)
}

/// Binds `keys`, from [`TERMINAL_KEYS`], to `shortcut`, which acts on the
/// terminal with the keyboard, else the one under the pointer, and
/// otherwise lets the key go on (`RightPanel::terminal_shortcut`). Bound in
/// "Input" too, the key context of Kit's text fields, ahead of Kit's own
/// bindings there (Ctrl+Shift+E selects to the end of the line, Cmd+Alt+Up
/// adds a cursor above), which get the key when no terminal does.
fn terminal_keys(keys: &'static str, shortcut: TerminalShortcut) -> [KeyBinding; 2] {
    [
        KeyBinding::new(keys, shortcut, Some("RhoApp")),
        KeyBinding::new(keys, shortcut, Some("Input")),
    ]
}

/// The binding of `keys` to `action` in `context`, where `secondary-` in
/// `keys` takes this platform's modifier, which [`KeyBinding::new`] can't
/// resolve.
fn shortcut(keys: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let context = context.map(|context| {
        Rc::new(KeyBindingContextPredicate::parse(context).expect("the key context parses"))
    });
    KeyBinding::load(keys, Box::new(action), context, false, None, cx).expect("the shortcut parses")
}

/// The main window: centered, large enough for the sidebar and a readable
/// transcript side by side.
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1200.), px(800.)), cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Rho".into()),
            ..Default::default()
        }),
        app_id: Some("rho-ui".to_owned()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon of Lucide's full catalog the source names (imported as
    /// `Lucide`) is in the app's assets: one left out of [`ExtraIcons`]
    /// draws nothing, without a word.
    #[test]
    fn every_lucide_icon_named_is_bundled() {
        fn sources(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("the source directory reads") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    sources(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    found.push(std::fs::read_to_string(&path).expect("a source file reads"));
                }
            }
        }
        let mut texts = Vec::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut texts,
        );
        let mut names: Vec<&str> = texts
            .iter()
            .flat_map(|text| text.split("Lucide::").skip(1))
            .filter_map(|rest| {
                let end = rest.find(|c: char| !c.is_ascii_alphanumeric())?;
                Some(&rest[..end]).filter(|name| !name.is_empty())
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        assert!(
            names.contains(&"Timer"),
            "the scan finds the names: {names:?}"
        );
        let missing: Vec<&str> = names
            .into_iter()
            .filter(|name| {
                // Lucide's file names: `Columns2` is `columns-2.svg`.
                let mut stem = String::new();
                let mut last: Option<char> = None;
                for c in name.chars() {
                    let starts_word = c.is_ascii_uppercase()
                        || (c.is_ascii_digit() && !last.is_some_and(|l| l.is_ascii_digit()));
                    if starts_word && last.is_some() {
                        stem.push('-');
                    }
                    stem.push(c.to_ascii_lowercase());
                    last = Some(c);
                }
                let path = format!("icons/{stem}.svg");
                !Assets.load(&path).is_ok_and(|icon| icon.is_some())
            })
            .collect();
        assert_eq!(missing, Vec::<&str>::new(), "add them to `ExtraIcons`");
    }

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
