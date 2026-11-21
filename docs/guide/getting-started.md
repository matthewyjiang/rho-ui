# Getting started

rho-ui is a window onto the [rho](https://matthewyjiang.github.io/rho/) daemon. The daemon runs the sessions; rho-ui lists them, shows them, and sends what you type. That has two consequences worth knowing up front:

- Sessions keep running after you close rho-ui. The daemon keeps running too. Stop it with `rho daemon stop`, or from [Settings → About](./settings#about-and-the-daemon).
- A session you start in rho's terminal UI shows up in rho-ui, and the other way round.

## Requirements

- **rho 3 or later.** rho-ui offers to install it on a machine that doesn't have it.
- **Linux, macOS or Windows.**
- **On Linux:** Wayland or X11 with a working Vulkan driver, plus the system packages in GPUI Kit's [installation guide](https://gpui-kit.com/docs/installation/).
- **For remote machines:** see [Remote machines](./machines#what-a-remote-machine-needs).

## Install

Build it from source with Rust (the version in the repository's `mise.toml`) and Zig 0.17, which builds the terminal's Ghostty core:

```bash
git clone https://github.com/matthewyjiang/rho-ui
cd rho-ui
mise install   # or install the Rust and Zig versions in mise.toml yourself
cargo install --path crates/rho-ui
```

### Add it to your Linux app menu

To launch rho-ui from your desktop's app menu, with its icon in the dock and window list:

```bash
install -Dm644 assets/rho-ui.desktop ~/.local/share/applications/rho-ui.desktop
install -Dm644 assets/icon.svg ~/.local/share/icons/hicolor/scalable/apps/rho-ui.svg
```

An app launched from the menu doesn't get your shell's `PATH`. If `rho` isn't somewhere like `/usr/local/bin`, edit the entry's `Exec=` to `rho-ui --rho /path/to/rho`.

## Run

```bash
rho-ui [DIR] [--rho PROGRAM]
```

- `DIR` is where new sessions start until you pick a project. It defaults to the current directory.
- `PROGRAM` is the `rho` that starts the daemon if none is running. It defaults to `rho` on `PATH`, then `~/.local/bin/rho`, where rho's install script puts it.

If rho isn't installed, rho-ui says so and offers **Install Rho**, which runs rho's own installer and then connects.

## Your first session

1. Press <kbd>Ctrl</kbd>+<kbd>N</kbd> (<kbd>Cmd</kbd>+<kbd>N</kbd> on macOS) for a new session.
2. Pick a project under **Start a session in**, or **Add project…** to add a folder. rho-ui finds no projects on its own: you add the ones you want. You can also paste a git URL to clone it.
3. Set the permission mode and model in the composer's footer if you like, type what you want done, and press <kbd>Enter</kbd>.

From there, [Sessions](./sessions) covers the sidebar and projects, and [Working with a turn](./turns) covers what happens while the agent works.

## Where rho-ui keeps things

| File | Holds |
| --- | --- |
| `~/.config/rho-ui/settings` | Your settings: theme, scale, fonts, machines |
| `~/.local/state/rho-ui/state.sqlite3` | What it remembers: projects, pinned, settled and snoozed sessions, turn times, the sidebar's layout and filter |

Both follow `XDG_CONFIG_HOME` and `XDG_STATE_HOME` when set, on macOS too. Sessions themselves are rho's, kept in rho's home.
