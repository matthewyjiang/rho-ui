# Settings

Open Settings with <kbd>Ctrl</kbd>+<kbd>,</kbd> or the gear at the bottom of the sidebar. rho-ui's own pages come first, then rho's settings, then About. With remote machines, the pages that depend on the machine have a **Machine** picker at the top: each machine has its own `gh`, sign-ins, worktrees and rho config.

![The Appearance page of Settings](/shots/settings-open.webp)

## Appearance

- **Theme**: Dark (the default), Light, or System, which follows your desktop as it changes.
- **UI scale**, **UI font** (default Noto Sans) and **Code font** (default JetBrainsMono Nerd Font).
- **Smooth scrolling**: On, Off, or System, the default, which is on unless your desktop asks for reduced motion.

## Machines

The remote machines rho-ui reaches over ssh, with each one's name, `rho` path and clone folder. See [Remote machines](./machines).

## Source control

The forge CLIs each machine has, so far GitHub's `gh`: its version, where it was found, and the accounts it is signed in to. The [Pull request panel](./panel#pull-request) reads GitHub through it. If `gh` isn't found, set **Program** to its path. Signing in asks questions in a terminal, so the page offers the `gh auth login` command to copy.

On a remote machine, `gh` runs without a login shell. Set variables such as `GH_TOKEN` in a file a plain ssh command's shell reads, such as `~/.zshenv`.

## Providers

The model providers each machine's rho can use, signed in or not:

- **Signed in**: with **Sign out**. A key that comes from an environment variable can't be changed here, and the page names the variable.
- **API key**: a masked box. The provider checks the key before rho saves it.
- **Account login**, such as ChatGPT's or GitHub Copilot's: **Sign in** opens the provider's page in your browser and shows any code to enter. A remote machine signs in with a device code, which works from any browser.

The page also shows where rho keeps credentials: the OS keyring, or a file in rho's home. It can change that, which matters when a daemon started over ssh can't reach the keyring.

A session whose model isn't signed in still opens, with a card above the composer to sign in right there.

## Worktrees

The git worktrees rho added for sessions, grouped by project. Each shows what a prune would do with it: remove it, once it is clean, merged, and its session is idle; or keep it, and why. **Remove** prunes the ones that can go, keeping their branches, as `rho worktrees prune` does.

## Rho

rho's own settings, as each machine's daemon describes them, saved in that machine's `~/.rho/config.toml`. Each says what it does and when a change applies. Since the daemon describes them, settings a newer rho adds show up without a new rho-ui. Changes apply at once. If the file changed elsewhere since rho-ui read it, rho refuses the write and the page shows the settings as they are now.

## About and the daemon

rho-ui's version, and for each machine the version of rho, its daemon's process ID, and **Restart** and **Stop**. Each asks first, saying what it would cut short: turns under way or waiting for you, logins, and sessions with background work.

- **Stop**: rho-ui won't start that daemon again by itself until you press **Start**, or until rho-ui restarts.
- **A daemon too old for rho-ui** refuses to connect. The sidebar offers **Restart** into the newer rho installed there, or says to update rho.
- **A daemon newer than rho-ui** says to update rho-ui.

## Installing and updating rho

rho-ui checks each machine's rho when it connects to it.

- **No rho**: rho-ui offers **Install Rho**, which runs rho's own installer.
- **A newer release**: the sidebar's footer offers **Update**, which runs `rho update`. When the update needs a terminal, as with pacman's `sudo`, it shows the command to copy instead.
- **After an update**, the daemon keeps running the old rho until nothing would be cut short. The footer says how many things it waits on, with **Restart now**. Open sessions reconnect after the restart, and what you typed stays.
