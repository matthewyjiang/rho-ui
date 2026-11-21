# Getting started

rho-ui is a window onto the [rho](https://matthewyjiang.github.io/rho/) daemon. The daemon runs the sessions; rho-ui lists them, shows them, and sends what you type. That has two consequences worth knowing up front:

- Sessions keep running after you close rho-ui. The daemon keeps running too. Stop it with `rho daemon stop`, or from [Settings → About](./settings#about-and-the-daemon).
- A session you start in rho's terminal UI shows up in rho-ui, and the other way round.

## Requirements

- **rho 3 or later.** rho-ui offers to install it on a machine that doesn't have it.
- **Linux, macOS or Windows.**
- **On Linux:** Wayland or X11 with a working Vulkan driver.
- **For remote machines:** see [Remote machines](./machines#what-a-remote-machine-needs).

## Install

### From a release

Each [release](https://github.com/matthewyjiang/rho-ui/releases) has a build for each OS. None is signed yet, so each OS asks once whether to trust it.

**Linux** (x86_64, glibc 2.39 or later, as in Ubuntu 24.04 or Debian 13): unpack the tarball and install it for your user, with the desktop entry and icon that put it in your app menu:

```bash
tar -xzf rho-ui-*-x86_64-linux.tar.gz
cd rho-ui-*-x86_64-linux
install -Dm755 rho-ui ~/.local/bin/rho-ui
install -Dm644 rho-ui.desktop ~/.local/share/applications/rho-ui.desktop
install -Dm644 icon.svg ~/.local/share/icons/hicolor/scalable/apps/rho-ui.svg
```

It links `libxcb`, `libxkbcommon` and `libxkbcommon-x11`, which every Linux desktop has; a server without a desktop needs them installed for `rho-ui serve` (see [In a browser](./browser#what-differs-from-the-desktop)).

**macOS** (Apple silicon): unzip it and move `Rho.app` to Applications. The first time you open it, macOS says it can't verify the app. Click **Done**, then open **System Settings → Privacy & Security**, click **Open Anyway** beside the note that Rho was blocked, and confirm. Or, in a terminal, before opening it:

```bash
xattr -dr com.apple.quarantine /Applications/Rho.app
```

**Windows** (x86_64): unzip it and run `rho-ui.exe`. If SmartScreen says it protected your PC, click **More info**, then **Run anyway**. It opens no console window, so it shows no command-line errors.


### From source

Build it with Rust (the version in the repository's `mise.toml`) and Zig 0.17, which builds the terminal's Ghostty core. On Linux, you also need the system packages in GPUI Kit's [installation guide](https://gpui-kit.com/docs/installation/).

```bash
git clone https://github.com/matthewyjiang/rho-ui
cd rho-ui
mise install   # or install the Rust and Zig versions in mise.toml yourself
cargo install --path crates/rho-ui
```

To launch it from your Linux desktop's app menu, with its icon in the dock and window list:

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

Press <kbd>Ctrl</kbd>+<kbd>N</kbd> and add a project: rho-ui doesn't find projects on its own. Then type what you want done. [Sessions](./sessions) covers projects, worktrees and attachments; [Working with a turn](./turns) covers steering and background work.

## Where rho-ui keeps things

| File | Holds |
| --- | --- |
| `~/.config/rho-ui/settings` | Your settings: theme, scale, fonts, machines |
| `~/.local/state/rho-ui/state.sqlite3` | What it remembers: projects, pinned, settled and snoozed sessions, turn times, the sidebar's layout and filter |

Both follow `XDG_CONFIG_HOME` and `XDG_STATE_HOME` when set, on macOS too. Sessions themselves are rho's, kept in rho's home.
