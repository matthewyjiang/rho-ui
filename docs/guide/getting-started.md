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

Each [release](https://github.com/matthewyjiang/rho-ui/releases) has a build for Linux (x86_64, glibc 2.39 or later, as in Ubuntu 24.04 or Debian 13), macOS (Apple silicon) and Windows (x86_64). The install script downloads the latest one for your OS, checks it against the release's `SHA256SUMS`, and installs it. Run it again to update.

On Linux or macOS:

```bash
curl -fsSL https://matthewyjiang.github.io/rho-ui/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://matthewyjiang.github.io/rho-ui/install.ps1 | iex
```

Where it puts things:

- **Linux:** `rho-ui` in `~/.local/bin`, and the app menu's entry and icon in `~/.local/share`.
- **macOS:** `Rho.app` in `/Applications` (`~/Applications` if you can't write there), with `rho-ui` linked into `~/.local/bin`. A download made with `curl` isn't quarantined, so macOS doesn't ask whether to trust the app.
- **Windows:** `rho-ui.exe` in `%LOCALAPPDATA%\Programs\rho-ui`, added to your user `PATH`, and **Rho** in the Start menu.

`RHO_UI_VERSION` installs another release than the latest, `RHO_UI_INSTALL_DIR` another folder for `rho-ui`, and on macOS, `RHO_UI_APP_DIR` another one for `Rho.app`. Set them for the shell that runs the script, not for `curl`:

```bash
curl -fsSL https://matthewyjiang.github.io/rho-ui/install.sh | RHO_UI_VERSION=0.1.0 sh
```

```powershell
$env:RHO_UI_VERSION = "0.1.0"; irm https://matthewyjiang.github.io/rho-ui/install.ps1 | iex
```

The script checks that the new `rho-ui` starts before it replaces anything, so a build that can't run on your system leaves the one you had.

#### Updates

A release build checks GitHub for a newer release when it starts, then hourly, and offers **Update** in the sidebar and in Settings → About. Updating runs that release's install script over the rho-ui that's running, in the folder it's in, so it needs `curl` on Linux and macOS. The running rho-ui keeps going until you choose **Restart**. Your sessions keep running in the daemon across the restart, but text you haven't sent is lost.

Only a rho-ui laid out as the install script lays it out updates itself: `rho-ui` (`rho-ui.exe`) on Linux and Windows, `Rho.app` on macOS. A renamed copy, a build from source, and `cargo install` never do. On macOS the script also links `rho-ui` into `~/.local/bin`, or into `RHO_UI_INSTALL_DIR` if that's set where rho-ui runs.

On Linux, `rho-ui` links `libxcb`, `libxkbcommon` and `libxkbcommon-x11`, which every desktop has. A server without a desktop needs them installed for `rho-ui serve` (see [In a browser](./browser#what-differs-from-the-desktop)).

Opened from the Finder, the macOS app gets the system's `PATH`, not your shell's. It finds `rho` only in `~/.local/bin`, where rho's installer puts it, or in `/usr/bin` and the other folders of that `PATH`. For a `rho` elsewhere, such as Homebrew's, link it there:

```bash
mkdir -p ~/.local/bin && ln -s "$(command -v rho)" ~/.local/bin/rho
```

#### By hand

None of the builds is signed yet, so each OS asks once whether to trust one downloaded from a browser.

**Linux:** unpack the tarball, put `rho-ui` in `~/.local/bin`, then [add it to your app menu](#add-it-to-your-linux-app-menu) from the same folder:

```bash
tar -xzf rho-ui-x86_64-linux.tar.gz
cd rho-ui-x86_64-linux
install -Dm755 rho-ui ~/.local/bin/rho-ui
```

**macOS:** unzip it and move `Rho.app` to Applications. The first time you open it, macOS says it can't verify the app. Click **Done**, then open **System Settings → Privacy & Security**, click **Open Anyway** beside the note that Rho was blocked, and confirm. Or, in a terminal, before opening it:

```bash
xattr -dr com.apple.quarantine /Applications/Rho.app
```

**Windows:** unzip it and run `rho-ui.exe`. If SmartScreen says it protected your PC, click **More info**, then **Run anyway**. It opens no console window, so it shows no command-line errors.

### From source

Build it with Rust (the version in the repository's `mise.toml`) and Zig 0.17, which builds the terminal's Ghostty core. On Linux, you also need the system packages in GPUI Kit's [installation guide](https://gpui-kit.com/docs/installation/).

```bash
git clone https://github.com/matthewyjiang/rho-ui
cd rho-ui
mise install   # or install the Rust and Zig versions in mise.toml yourself
cargo install --path crates/rho-ui
```

### Add it to your Linux app menu

The install script does this for you. Otherwise, to launch rho-ui from your desktop's app menu, with its icon in the dock and window list, install its desktop entry and icon, from the release's folder or the checkout's `assets/`. An app launched from the menu doesn't get your shell's `PATH`, so the entry names `rho-ui` by its full path, `BIN`:

```bash
BIN=~/.local/bin/rho-ui   # from cargo install: BIN=~/.cargo/bin/rho-ui
mkdir -p ~/.local/share/applications
sed "s|^Exec=rho-ui|Exec=$BIN|" rho-ui.desktop > ~/.local/share/applications/rho-ui.desktop
install -Dm644 icon.svg ~/.local/share/icons/hicolor/scalable/apps/rho-ui.svg
```

If `rho` is neither on the menu's `PATH` nor in `~/.local/bin`, link it into `~/.local/bin` as on macOS above.

## Run

```bash
rho-ui [DIR] [--rho PROGRAM]
```

- `DIR` is where new sessions start until you pick a project. It defaults to the current directory, or your home folder when that is the filesystem's root or rho-ui's own folder, as when you open it from the Finder, the Dock or Explorer.
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
