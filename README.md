<img src="assets/icon.svg" width="64" height="64" alt="">

# rho-ui

The desktop app for [rho](https://github.com/matthewyjiang/rho).

Sessions run in the rho daemon, so they keep running after you close the window, and sessions started in rho's terminal UI show up here too. Add remote machines in Settings to see their sessions beside this machine's, reached over ssh.

## Requirements

- rho 3 or later, which rho-ui offers to install on a machine without it; on a remote machine, one whose `rho daemon status --json` names its socket, as rho 3.1's does
- Linux or macOS (no Windows yet)
- For a remote machine: its host key in your `known_hosts` (not only a certificate authority for it), and a key or certificate it accepts in your key agent, or in a key file without a passphrase (`ssh HOST` in a terminal should sign in without asking anything)
- On Linux: Wayland or X11 with a working Vulkan driver, plus the system packages in GPUI Kit's [installation guide](https://gpui-kit.com/docs/installation/)

## Run

```bash
cargo run -p rho-ui -- [DIR] [--rho PROGRAM]
```

- `DIR`: where new sessions start. Defaults to the current directory.
- `PROGRAM`: the `rho` that starts the daemon if none runs. Defaults to `rho` on `PATH`, else `~/.local/bin/rho`, where rho's install script puts it.

The daemon keeps running after the app quits. Stop it with `rho daemon stop`.

### In a browser

```bash
scripts/build-web.sh
cargo run -p rho-ui-serve -- [--port PORT] [--bind ADDRESS]
```

Open the URL it prints, which holds its token. The page shows this machine's sessions, with the same views, terminal included, as the desktop app; rho-ui-serve, which listens on 127.0.0.1:8765 unless told, does on this machine what the browser can't. From another machine, tunnel to it, as with `ssh -L 8765:127.0.0.1:8765 HOST`. In a browser there are no remote machines yet, and nothing is remembered past the page. [Architecture](docs/architecture.md#in-a-browser) says how it works.

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

See [docs/](docs/) for features in detail, architecture, and testing.
