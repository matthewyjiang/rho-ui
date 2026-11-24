//! rho-ui: the desktop UI for the rho coding agent.
//!
//! `rho-ui [DIR] [--rho PROGRAM]` opens one window. DIR is the workspace for
//! new sessions (default: the current directory, or home when opened from
//! the desktop: see [`default_workspace`]); PROGRAM is the `rho` that
//! starts the daemon when none runs (default: `rho` on `PATH`).
//!
//! Built for wasm, it is rho-ui's web build instead, which `rho-ui serve`
//! serves to a browser; `web.rs` starts it.

// A release on Windows is a GUI program, so starting it opens no console
// window beside its own; its command-line errors then show nowhere. Debug
// builds keep the console, for their logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod added_dirs;
mod app;
#[cfg(target_os = "macos")]
mod app_menu;
mod app_update;
mod attachments;
mod auths;
mod background;
mod background_log;
mod badge;
mod cloning;
mod composer;
mod draft;
mod figures;
mod fold;
mod folders;
mod forge;
mod input_card;
mod kinetic;
mod leads;
mod lightbox;
mod local;
mod machines;
mod parts;
mod pele;
mod pictures;
mod plan;
mod pr_mark;
mod project_picker;
mod projects;
mod pull_requests;
mod repo;
mod rho_install;
mod rho_settings;
mod right_panel;
#[cfg(unix)]
mod serve;
mod session_list;
mod session_settings;
mod session_view;
mod settings;
mod settings_view;
mod shelf;
mod sidebar;
mod sign_in;
mod store;
mod surface_view;
mod themes;
mod time;
mod title_bar;
mod transcript;
mod turn_times;
#[cfg(test)]
mod ui_tests;
mod unseen;
mod usage;
#[cfg(target_family = "wasm")]
mod web;
mod window_chrome;
mod work;
mod workspaces;
mod worktrees;

#[cfg(not(target_family = "wasm"))]
use std::path::{Path, PathBuf};
use std::{borrow::Cow, rc::Rc, sync::Arc};

use gpui_kit::component::input::{Copy, Enter, Paste};
use gpui_kit::AppContext as _;
#[cfg(not(target_family = "wasm"))]
use gpui_kit::QuitMode;
use gpui_kit::{
    px, size, Action, App, AssetSource, Axis, DevicePixels, KeyBinding, KeyBindingContextPredicate,
    NoAction, SharedString, SvgSize, WindowBounds, WindowOptions,
};
#[cfg(not(target_family = "wasm"))]
use rho_ui_core::{DaemonTarget, RhoInstall};

use crate::{
    app::{LaunchConfig, RhoApp},
    background_log::BackgroundLog,
    project_picker::LastCheckout,
    projects::Projects,
    right_panel::{Direction, PaneCommand, PanelLayout, TerminalShortcut},
    settings::Settings,
    sidebar::SidebarState,
    store::StateDb,
    turn_times::TurnTimes,
};
#[cfg(not(target_family = "wasm"))]
use crate::{
    machines::{Reach, RemoteMachine},
    right_panel::TerminalShell,
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
        FolderGit2,
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
        MessageCircleQuestionMark,
        SkipForward,
        FileDiff,
        FolderTree,
        FolderPlus,
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
        ImageOff,
        Hourglass,
        GitPullRequest,
        GitPullRequestDraft,
        GitPullRequestClosed,
        GitMerge,
        CircleMinus,
        CircleSlash,
        MessageSquare,
        MessagesSquare,
        Link,
        Unlink,
        KeyRound,
        TextQuote,
        // The A2UI basic catalog's icons, for surfaces (`surface_view.rs`).
        BellOff,
        CalendarDays,
        Camera,
        CircleQuestionMark,
        CreditCard,
        FastForward,
        Film,
        House,
        Image,
        Lock,
        LockOpen,
        Mail,
        MapPin,
        Music,
        Phone,
        Printer,
        Rewind,
        Send,
        Share2,
        ShoppingCart,
        SkipBack,
        SquareDashed,
        StarHalf,
        Upload,
        Volume,
        Volume1,
        Volume2,
        VolumeX,
        X
    ]
);

/// GPUI Kit's default icons plus [`ExtraIcons`]. Natively all are borrowed
/// from static storage; in a browser, Kit's own come from the server, at
/// `/assets/icons/`, as each is first drawn.
struct Assets(gpui_kit::assets::Assets);

impl Assets {
    /// Kit's icons from `server`, the page's origin, in a browser; native
    /// builds ignore it.
    fn new(server: &str) -> Self {
        Self(gpui_kit::assets::Assets::new(server.to_owned()))
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        #[cfg(target_family = "wasm")]
        if let Some(font) = web::svg_font(path) {
            return Ok(Some(font));
        }
        match ExtraIcons.load(path)? {
            Some(icon) => Ok(Some(icon)),
            None => self.0.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = self.0.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

#[cfg(not(target_family = "wasm"))]
const USAGE: &str = "usage: rho-ui [DIR] [--rho PROGRAM]
       rho-ui serve [--help]   rho-ui in a browser";

/// Parses `[DIR] [--rho PROGRAM]`. Returns the usage text on bad input.
#[cfg(not(target_family = "wasm"))]
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
    let workspace = if let Some(dir) = dir {
        let workspace = rho_ui_core::platform::canonicalize(&dir)
            .map_err(|error| format!("can't open {}: {error}", dir.display()))?;
        if !workspace.is_dir() {
            return Err(format!("{} is not a directory", workspace.display()));
        }
        Some(workspace)
    } else {
        let cwd = std::env::current_dir()
            .and_then(|cwd| rho_ui_core::platform::canonicalize(&cwd))
            .map_err(|error| format!("can't read the current directory: {error}"))?;
        let exe = std::env::current_exe()
            .and_then(|exe| rho_ui_core::platform::canonicalize(&exe))
            .ok();
        default_workspace(cwd, exe.as_deref().and_then(Path::parent))
    };
    let daemon = DaemonTarget::rho(&rho).ok_or("can't find the rho home: set RHO_HOME or HOME")?;
    Ok(LaunchConfig {
        workspace,
        local: Reach {
            daemon,
            rho: Some(RhoInstall::Here { rho }),
        },
        remote: Rc::new(RemoteMachine::ssh_reach),
        shell: TerminalShell::from_env(),
        rho_home: rho_ui_core::platform::rho_home(),
    })
}

/// Where new sessions start with no DIR given: `cwd`, unless it's a folder
/// that only a launch from the desktop leaves, which no one means to work
/// in: the filesystem's root, as Finder and the Dock give, or the
/// program's own folder, as Explorer gives. Then `None`, the home folder.
#[cfg(not(target_family = "wasm"))]
fn default_workspace(cwd: PathBuf, exe_dir: Option<&Path>) -> Option<PathBuf> {
    let from_desktop = cwd.parent().is_none() || exe_dir == Some(cwd.as_path());
    (!from_desktop).then_some(cwd)
}

#[cfg(target_family = "wasm")]
fn main() {
    web::main();
}

#[cfg(not(target_family = "wasm"))]
#[expect(
    clippy::print_stderr,
    reason = "a command-line error goes to stderr before any window exists"
)]
fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "serve") {
        #[cfg(unix)]
        return serve::main(args.skip(1));
        #[cfg(not(unix))]
        {
            eprintln!("rho-ui serve runs on Unix only");
            return std::process::ExitCode::FAILURE;
        }
    }
    let launch = match parse_args(args) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("{message}");
            // scripts/install.sh and install.ps1 take 2 from `--help` to
            // mean the binary loads.
            return std::process::ExitCode::from(2);
        }
    };

    // Quit with the window, on macOS too, where GPUI's default keeps the
    // app running with none: rho-ui has one window and no menu bar or Dock
    // menu to open another.
    let app = gpui_kit::application()
        .with_assets(Assets::new(""))
        .with_quit_mode(QuitMode::LastWindowClosed);
    // GPUI's restart, which restarts into an update on Windows
    // (`app_update.rs`), starts the program with this one's arguments.
    #[cfg(windows)]
    let app = app.with_restart_arguments(std::env::args_os().skip(1).collect());
    app.run(move |cx| {
        open_app(launch, &StateDb::open(), Settings::load(), cx);
        cx.activate(true);
    });
    std::process::ExitCode::SUCCESS
}

/// Sets the app up with `settings` and opens its window, remembering
/// things in `db`: the desktop's start, and the browser's once its fonts
/// and state are in.
fn open_app(launch: LaunchConfig, db: &StateDb, settings: Settings, cx: &mut App) {
    init(cx, db);
    // pele takes about 100 ms to load: started now, it's ready for the
    // first diagram. Tests load it when they draw one, so the load doesn't
    // slow the tests that time animations.
    pele::start();
    settings.apply(cx);
    let options = window_options(cx);
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            let sidebar = SidebarState::load(db);
            let projects = Projects::load(db);
            RhoApp::new(launch, sidebar, projects, window, cx)
        })
    })
    .expect("failed to open the rho-ui window");
}

/// App-wide setup shared by `main` and the UI tests: GPUI Kit, the theme
/// with default settings, key bindings, and the globals that remember
/// things between runs in `db`.
fn init(cx: &mut App, db: &StateDb) {
    gpui_kit::init(cx);
    themes::install(cx);
    window_chrome::install(cx);
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
/// transcript side by side, with the app's own title bar where it draws
/// one (`window_chrome`).
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1200.), px(800.)), cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(window_chrome::titlebar_options(cx)),
        app_owns_titlebar_drag: window_chrome::app_owns_titlebar_drag(cx),
        app_id: Some("rho-ui".to_owned()),
        icon: app_icon(cx),
        ..Default::default()
    }
}

/// `assets/icon.svg` rasterized for the window icon, which GPUI sets on X11
/// only; Wayland takes the icon from the desktop entry named by `app_id`.
/// `None`, with a log line, if the SVG doesn't render.
#[expect(
    clippy::print_stderr,
    reason = "rho-ui has no log; stderr reaches the terminal or the journal"
)]
fn app_icon(cx: &App) -> Option<Arc<image::RgbaImage>> {
    /// Big enough for a crisp dock or alt-tab icon; X11 scales it down.
    const SIDE: i32 = 256;
    let svgs = cx.svg_renderer();
    let icon = svgs
        .parse_svg(include_bytes!("../../../assets/icon.svg"))
        .and_then(|svg| {
            let side = DevicePixels(SIDE);
            svgs.render_parsed(&svg, SvgSize::ExactSize(size(side, side)))
        })
        .inspect_err(|error| {
            eprintln!("rho-ui: the app icon didn't render, so the window has none: {error}");
        })
        .ok()?;
    // GPUI renders BGRA; the window icon takes RGBA.
    let mut pixels = icon.as_bytes(0)?.to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    image::RgbaImage::from_raw(SIDE.unsigned_abs(), SIDE.unsigned_abs(), pixels).map(Arc::new)
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
                !Assets::new("").load(&path).is_ok_and(|icon| icon.is_some())
            })
            .collect();
        assert_eq!(missing, Vec::<&str>::new(), "add them to `ExtraIcons`");
    }

    fn parse(args: &[&str]) -> Result<LaunchConfig, String> {
        parse_args(args.iter().map(std::string::ToString::to_string))
    }

    #[test]
    fn rho_program_follows_its_flag() {
        let dir = std::env::temp_dir();
        let workspace = dir.to_str().expect("a UTF-8 temporary directory");
        let launch = parse(&[workspace, "--rho", "/opt/rho/bin/rho"]).unwrap();
        assert_eq!(
            launch.workspace,
            Some(rho_ui_core::platform::canonicalize(&dir).unwrap())
        );
        // Both sides read the rho home from this process's environment.
        assert_eq!(
            launch.local.daemon,
            DaemonTarget::rho("/opt/rho/bin/rho").unwrap()
        );
        assert_eq!(
            launch.local.rho,
            Some(RhoInstall::Here {
                rho: PathBuf::from("/opt/rho/bin/rho")
            })
        );
        assert_eq!(
            parse(&[workspace]).unwrap().local.daemon,
            DaemonTarget::rho("rho").unwrap()
        );
    }

    #[test]
    fn a_desktop_launch_starts_sessions_at_home() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let app = Path::new(if cfg!(windows) { r"C:\Rho" } else { "/opt/rho" });
        let project = app.join("project");
        assert_eq!(default_workspace(root.into(), Some(app)), None);
        assert_eq!(default_workspace(app.to_owned(), Some(app)), None);
        assert_eq!(
            default_workspace(project.clone(), Some(app)),
            Some(project.clone())
        );
        assert_eq!(default_workspace(project.clone(), None), Some(project));
    }

    #[test]
    fn rejects_extra_positional_and_missing_program() {
        let dir = std::env::temp_dir();
        let workspace = dir.to_str().expect("a UTF-8 temporary directory");
        assert!(parse(&[workspace, workspace]).is_err());
        assert!(parse(&["--rho"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
