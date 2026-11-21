<img src="assets/icon.svg" width="64" height="64" alt="">

# rho-ui

The desktop app for [rho](https://github.com/matthewyjiang/rho).

**Docs: [matthewyjiang.github.io/rho-ui](https://matthewyjiang.github.io/rho-ui/)**

Sessions run in the rho daemon, so they keep running after you close the window, and sessions started in rho's terminal UI show up here too. Add remote machines in Settings to see their sessions beside this machine's, reached over ssh.

## Requirements

- rho 3 or later, which rho-ui offers to install on a machine without it; on a remote machine, one whose `rho daemon status --json` names its socket, as rho 3.1's does
- Linux, macOS or Windows
- For a remote machine: its host key in your `known_hosts` (not only a certificate authority for it), and a key or certificate it accepts in your key agent, or in a key file without a passphrase (`ssh HOST` in a terminal should sign in without asking anything)
- On Linux: Wayland or X11 with a working Vulkan driver

## Run

```bash
cargo run -p rho-ui -- [DIR] [--rho PROGRAM]
```

- `DIR`: where new sessions start. Defaults to the current directory.
- `PROGRAM`: the `rho` that starts the daemon if none runs. Defaults to `rho` on `PATH`, else `~/.local/bin/rho`, where rho's install script puts it.

The daemon keeps running after the app quits. Stop it with `rho daemon stop`.

### In a browser

```bash
cargo xtask web                                  # the web build, into target/web
cargo run -p rho-ui -- serve [--port PORT] [--bind ADDRESS]
```

Open the URL it prints, which holds its token. `rho-ui serve`, which listens on 127.0.0.1:8765 unless told, does on this machine what the browser can't, so the page works as the desktop app here would: this machine's sessions and your remote machines', the same views, terminal included, and the same settings and remembered state, which the page and the desktop app share. From another machine, tunnel to it, as with `ssh -L 8765:127.0.0.1:8765 HOST`.

`cargo xtask dist` builds one `rho-ui` with the web build inside it, so `rho-ui serve` needs nothing beside it. [Architecture](docs/architecture.md#in-a-browser) says how it works.

## Install

Download the build for your OS from the [latest release](https://github.com/matthewyjiang/rho-ui/releases/latest): a tarball for Linux (x86_64), `Rho.app` for macOS (Apple silicon), a zip for Windows (x86_64). [Getting started](https://matthewyjiang.github.io/rho-ui/guide/getting-started#from-a-release) has the steps for each, with the libraries `rho-ui serve` needs on a Linux server.

None is signed yet. macOS won't open `Rho.app` the first time: click Done, then Open Anyway in System Settings → Privacy & Security, or clear its quarantine before opening it:

```bash
xattr -dr com.apple.quarantine /Applications/Rho.app
```

On Windows, if SmartScreen says it protected your PC, click More info, then Run anyway.

To build it from source instead, with Rust and Zig at the versions `mise.toml` pins (and on Linux, the system packages in GPUI Kit's [installation guide](https://gpui-kit.com/docs/installation/)):

```bash
cargo install --path crates/rho-ui
```

## Keys

On macOS, Cmd takes the place of Ctrl in these, and the menu bar lists them, with Cmd+H to hide and Cmd+M to minimize.

| Key | Action |
| --- | --- |
| Ctrl+N | New session |
| Ctrl+Shift+N | New session, picking the project from the keyboard |
| Ctrl+, | Settings |
| Ctrl+B | Collapse or open the sidebar |
| Ctrl+Alt+B | Show or hide the right panel |
| Ctrl+Q | Quit |
| Enter (while a turn runs) | Steer the turn |
| Shift+Enter or Alt+Enter | New line in the composer |
| Ctrl+V, or Ctrl+Shift+V on any platform | Paste text, an image, or a copied file |

## Files

- `~/.config/rho-ui/settings`: theme, scale, fonts, machines
- `~/.local/state/rho-ui/state.sqlite3`: your projects, settled and snoozed sessions, turn times, and the sidebar's layout and filter

These follow `XDG_CONFIG_HOME` and `XDG_STATE_HOME` when set, on macOS too.

## More

The [docs site](https://matthewyjiang.github.io/rho-ui/) has the user guide. [docs/](docs/) holds its source, with the architecture and testing notes.
