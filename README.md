<img src="assets/icon.svg" width="64" height="64" alt="">

# rho-ui

The desktop app for [rho](https://github.com/matthewyjiang/rho), built with [GPUI Kit](https://gpui-kit.com).

rho-ui is a frontend of the rho daemon, like rho's terminal UI. It connects to the daemon's socket and speaks rho's own protocol through the [`rho-protocol`](https://github.com/matthewyjiang/rho/tree/daemon-v3/crates/rho-protocol) crate. It needs rho 3 or later: rho 2 has no daemon.

Sessions and their turns run in the daemon, so they outlive the app. A turn that rho-ui starts keeps running after the window closes, and a turn that the terminal UI started shows in rho-ui's sidebar, where you can open it, answer its approvals, and steer it.

## Run

```bash
cargo run -p rho-ui -- [DIR] [--rho PROGRAM]
```

`DIR` is where new sessions start; it defaults to the current directory. `PROGRAM` is the `rho` that starts the daemon when none runs; it defaults to `rho` on `PATH`. Which daemon rho-ui reaches follows from its own `RHO_HOME` (or `HOME`), so a `PROGRAM` that sets another `RHO_HOME` does not move it.

rho-ui connects to the daemon of the rho home (`$RHO_HOME`, or `~/.rho`) through `rho-protocol`'s `LocalDaemon`, which finds its socket the way rho does. When none runs, `LocalDaemon` starts it with `PROGRAM daemon start`, rho's own ensure-running path. The daemon belongs to the rho home, not to rho-ui, and keeps running after the app quits. Stop it with `rho daemon stop`.

On Linux you need a Wayland or X11 session with a working Vulkan driver. See the GPUI Kit [installation guide](https://gpui-kit.com/docs/installation/) for the system packages. The daemon's socket is a Unix socket, so rho-ui does not build on Windows yet.

## Layout

```mermaid
flowchart TD
    UI["rho-ui: GPUI Kit views"] --> Core["rho-ui-core: daemon connection, session state"]
    Core -->|"Unix socket, rho protocol"| Daemon["rho daemon"]
    TUI["rho's terminal UI"] -->|"same protocol"| Daemon
    Daemon --> Store["~/.rho/sessions"]
```

| Crate | Owns |
| --- | --- |
| `rho-ui-core` | The connection to the daemon, its requests, the event stream, `Session`, which turns session events into transcript entries, open requests and settings, and `Timeline`, which groups each run of tool calls and thoughts into one row and splits rows into turns, so the UI can fold a finished turn's working behind one line. It has no GUI code. It keeps one small Tokio runtime for the socket, because `rho-protocol`'s client needs one; every method returns a plain future that any executor can await. |
| `rho-ui` | Windows, views, and input. |

`rho-protocol` is not on crates.io while the protocol is 0.1. The workspace pins it to a commit of rho's `daemon-v3` branch. Two builds of protocol 0.1 can differ on the wire, so run a daemon built from the same commit. A daemon of another build still connects, and rho-ui shows the daemon's notice that requests may fail.

The composer's footer holds the session's settings: the permission mode (which tool calls run without asking) on the left, and on the right one button naming the model and reasoning, which opens the model list with search beside the reasoning levels and fast mode. New session opens the session right away, so these can be set before the first message; rho writes no file for a session until its first prompt, and the daemon discards an unused draft's session, so nothing is left behind. A setting changed while a turn runs applies when the turn ends.

While a turn runs, Enter in the composer steers it: the text joins the turn's next model request. Until the model reads it, it waits above the composer, where you can take it back.

The sidebar lists the daemon's saved sessions. Once a second it also asks the daemon which sessions have work under way, so a session that runs a turn or waits for an approval shows as working or waiting whichever client started it.

Settling and snoozing sessions is rho-ui's own idea; rho has no such state. Sessions started or continued in rho-ui list as active. Sessions from elsewhere, such as rho's terminal UI, start out settled, so the sidebar doesn't fill with old history; a session with work under way always lists as active, and snoozing, restoring, prompting, or steering one makes it rho-ui's. rho-ui keeps this in `$XDG_STATE_HOME/rho-ui/shelves` (by default `~/.local/state/rho-ui/shelves`). A settled session comes back when it has new activity in rho-ui, and a snoozed one when its time is up. Snoozed and Settled sit under the active sessions, folded; open one to see its sessions on one line each, and hover a session for its details.

Settings (Ctrl+, or the gear at the bottom of the sidebar) hold the UI scale, the UI font (default Noto Sans) and the code font (default JetBrainsMono Nerd Font), saved in `$XDG_CONFIG_HOME/rho-ui/settings`.

Neither crate is published to crates.io.

## Test

```bash
cargo test
```

- `rho-ui-core` has a scripted daemon for tests (`fake_daemon`, behind the `test-support` feature): `rho-protocol`'s real server side on a real Unix socket, with no model or network. `tests/fixtures/bash_turn.jsonl` is a recorded turn from a real daemon.
- `rho-ui` UI tests (`crates/rho-ui/src/ui_tests.rs`) open the real app in an offscreen window, click and type through GPUI Kit's test helpers, and talk to the same fake daemon. They need a working Vulkan driver; Mesa lavapipe works without a GPU. Each run saves screenshots to `target/ui-shots/` for review.

To measure frame times with 300 saved sessions and a 100-turn transcript, run `cargo test -p rho-ui --release frame_times -- --ignored --nocapture`.

To check rho-ui against a real daemon and model, build `rho` from the commit `Cargo.toml` pins, sign it in, and run the `live_daemon` test. It starts the daemon, runs a turn through an approval, and saves screenshots:

```bash
RHO_UI_LIVE_RHO=/path/to/rho cargo test -p rho-ui live_daemon -- --ignored --nocapture
```

Set `RHO_UI_LIVE_ATTACH` to the ID of a session whose turn waits on an approval, such as one you started in rho's terminal UI, and the test also opens that session, answers the approval, and steers the turn. Use a `RHO_HOME` of its own to keep the run away from your sessions.

`Cargo.lock` follows GPUI Kit 0.9.0's own lock file; its `test-support` feature does not resolve against newer versions of some shared crates.
