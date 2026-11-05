//! rho-ui: the desktop UI for the rho coding agent.
//!
//! `rho-ui [DIR] [-- AGENT COMMAND...]` opens one window. DIR is the
//! workspace for new threads (default: the current directory); the agent
//! command defaults to `rho acp`.

mod app;
mod approval;
mod composer;
mod config_bar;
mod sidebar;
mod thread_view;
mod tool_card;
mod transcript;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{
    px, size, AppContext as _, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
};
use rho_ui_core::AgentCommand;

use crate::app::{LaunchConfig, RhoApp};

gpui_kit::actions!(
    rho_ui,
    [
        /// Start a thread in the launch workspace.
        NewThread,
        /// Stop the running turn in the focused thread.
        StopTurn,
        /// Quit the app.
        Quit
    ]
);

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

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-n", NewThread, Some("RhoApp")),
                KeyBinding::new("escape", StopTurn, Some("ThreadView")),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1200.), px(800.)), cx)),
                window_min_size: Some(size(px(720.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("rho".into()),
                    ..Default::default()
                }),
                app_id: Some("rho-ui".to_owned()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| RhoApp::new(launch, window, cx))
            })
            .expect("failed to open the rho-ui window");
            cx.activate(true);
        });
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
