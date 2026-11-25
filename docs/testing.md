# Testing

```bash
cargo test
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` on Linux, macOS and Windows for every pull request and push to `main`, with the toolchain `mise.toml` pins. Each run uploads the UI tests' screenshots as `ui-shots-<os>` artifacts, and runs every test binary even when one fails (`--no-fail-fast`).

A test that can't run on Windows says so on the test itself. `#[cfg_attr(windows, ignore = "…")]` means it should run there once its reason is fixed: rho-ui still does the thing with a POSIX `sh` script, or the test scripts one. `#[cfg(unix)]` means it doesn't apply there: symlinks, a Unix socket, and the remote machine's scripts, which a test's fake ssh server runs with `sh`. Each feature that reads a machine is tested against one fixture through its local Rust path and, on Unix, through the `sh` script a remote machine runs, so the two can't drift apart. The local path runs on Windows too, except the check of an installed rho, whose fake `rho` is an `sh` script; that a missing rho checks as missing runs everywhere.

On Linux, CI also runs clippy on the web build, for `wasm32-unknown-unknown` (`cargo xtask clippy-web`; see [In a browser](architecture.md#in-a-browser)). `crates/rho-ui/tests/serve.rs` runs `rho-ui serve` as a program in a temporary home, with a fake daemon behind a fake `rho`, and reaches it with rho-ui-core's own server link, as the page does: the daemon, scripts and the terminal's shell; the desktop app's settings and state database, shared; a remote machine, which `fake_sshd` plays and the server reaches through an ssh config that a wrapper `ssh` on its `PATH` hands `ssh -G`; and that only a page holding the token gets through. No test drives the page in a browser; check that by hand with `cargo xtask web` and `cargo run -p rho-ui -- serve`.

Before adding or growing a test, load the `rho-ui-test-selection` skill (`.agents/skills`): every test needs a failure mode, one owner layer, and a gap.

- `rho-ui-core` has a scripted daemon for tests (`fake_daemon`, behind the `test-support` feature): `rho-protocol`'s real server side, listening where rho's daemon would (a Unix socket, or on Windows a named pipe), with no model or network. `tests/fixtures/bash_turn.jsonl` is a recorded turn from a real daemon, and `tests/fixtures/tool_cards.jsonl` holds tool calls a rho 3 daemon sent with their cards, streamed and finished, and `tests/fixtures/ui_surface.jsonl`, written by hand, holds `ui` calls with their surfaces as rho's protocol sends them. The fake's prompts "ui" and "gallery" show a form and every component of the A2UI basic catalog, and "widen" changes the form (`fake_daemon/surface.rs`). UI tests name a surface's elements with `surface_view::element_id`, as the panel does. It also serves a session's pull requests (`fake_daemon/pull_requests.rs`): every session answers no git repository and no links until a test gives its branch a PR (`open_pull_request`) or edits its set (`edit_pull_requests`), which sends the `pull_requests` event as rho's daemon does; `Script::pull_requests` makes it a daemon from before pull requests, which answers `MethodNotFound`.
- `rho-ui` UI tests (`crates/rho-ui/src/ui_tests.rs`) open the real app in an offscreen window, click and type through GPUI Kit's test helpers, and talk to the same fake daemon. A test reaches a remote machine through another fake daemon's socket in place of ssh, by giving `LaunchConfig::remote` its own resolver, and gives `LaunchConfig::rho_home` a folder that isn't there, or a temporary one, so a lead never makes folders in your rho home; with no ssh link there, a remote terminal's shell is tested in `crates/rho-ui-core/tests/ssh.rs` instead. `crates/rho-ui-core/tests/ssh.rs` runs the ssh path against `fake_sshd` (behind the same feature): russh's server side on 127.0.0.1 in the test's process, with a host key and a client key of its own. It runs each command with this machine's `sh` in a temporary home that plays the remote machine, under a real pseudo-terminal (`portable-pty`) when the client asks for one, as a remote terminal does (it says how each such session ended, and can refuse pseudo-terminals as `PermitTTY no` does), and forwards the daemon's channel to a fake daemon's socket. Its targets pick the `known_hosts` file (the server's key, none, or another key) and the key files to sign in with, and use no key agent, so the tests never touch your ssh setup. It counts the keys clients offer and the times they sign in, so a test can tell one shared connection from several. The ones that stop before running a command, at the host key or signing in, run on Windows too. On Linux they render on Mesa lavapipe, Vulkan in software (`mesa-vulkan-drivers` on Debian and Ubuntu), whatever GPU the machine has, so screenshots match across machines; set `VK_LOADER_DRIVERS_SELECT` to pick another driver. On macOS they render on Metal, with the text system GPUI's Mac platform uses, built alone because the platform must be made on the main thread. GPUI has no offscreen renderer for Windows, so there they render with its wgpu one, on Vulkan: a Windows machine needs a Vulkan driver, and CI installs the Vulkan loader and Mesa lavapipe. Shortcuts are pressed as `secondary-…`, so they test whichever modifier the platform binds. The terminal tests run `/bin/dash`, whose `$ ` prompt they wait for (macOS's `/bin/sh` is bash). Each run saves screenshots to `target/ui-shots/` for review.

To check rho-ui against a real daemon and model, install rho 3, sign it in, and run the `live_daemon` test. It starts the daemon, runs a turn through an approval, and saves screenshots. A new rho home starts in `bypass`, which asks for no approvals, so set `permission_mode = "supervised"` under `[behavior]` in its `config.toml`:

```bash
RHO_UI_LIVE_RHO=/path/to/rho cargo test -p rho-ui live_daemon -- --ignored --nocapture
```

Set `RHO_UI_LIVE_ATTACH` to the ID of a session whose turn waits on an approval, such as one you started in rho's terminal UI, and the test also opens that session, answers the approval, and steers the turn; without it, the test says it skipped that half. Give that turn a command that runs for several seconds, such as `sleep 8`, so the steer arrives while it runs. Use a `RHO_HOME` of its own to keep the run away from your sessions.

`live_background` checks background work the same way: a turn starts two background processes and ends while they run, one is stopped from the list, and the other exits by itself. The daemon's own turn tells the model of the stop, and the test sends a message during it; after the exit, the test prints the daemon's turns and its last reply for you to check. It takes a few minutes:

```bash
RHO_UI_LIVE_RHO=/path/to/rho cargo test -p rho-ui live_background -- --ignored --nocapture
```

`live_lead` checks a lead the same way, with a rho that has the `lead` agent: a lead started from the sidebar starts one worker in the git repository `RHO_UI_LIVE_REPO` names and ends its turn while the worker sleeps; the daemon's own turn then tells it the worker is done. The test prints both turns' entries and the lead's reply for you to check. The repository needs one commit and no remote, and the rho home must be in `bypass`:

```bash
RHO_UI_LIVE_RHO=/path/to/rho RHO_UI_LIVE_REPO=/path/to/repo cargo test -p rho-ui live_lead -- --ignored --nocapture
```

`a_live_daemon_over_ssh` reaches the daemon on an ssh host with your own ssh setup, twice, and prints how long each connect took; the second rides the first's connection. The host must have rho installed and its key in `known_hosts`. Set `RHO_UI_LIVE_SSH_RHO` when rho is not on the PATH of a non-interactive ssh command there. To point it at `localhost` with a `known_hosts` of its own, give it a `Host` entry in `~/.ssh/config` with `HostName localhost` and `UserKnownHostsFile`:

```bash
RHO_UI_LIVE_SSH=workstation cargo test -p rho-ui-core --test ssh -- --ignored --nocapture
```
