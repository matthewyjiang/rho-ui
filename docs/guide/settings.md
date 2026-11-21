# Settings

Open Settings with <kbd>Ctrl</kbd>+<kbd>,</kbd> or the gear at the bottom of the sidebar. rho-ui's own pages come first, then rho's settings, then About. With remote machines, the pages that depend on the machine have a **Machine** picker at the top: each machine has its own `gh`, sign-ins, worktrees and rho config.

![The Appearance page of Settings](/shots/settings-open.webp)

::: details In detail

rho-ui's own settings stay in its own file, not in rho's: the theme, scale and fonts must apply before the first frame, before any daemon answers, and the machines list is how rho-ui finds the remote daemons in the first place.

:::

## Appearance

- **Theme**: Dark (the default), Light, or System, which follows your desktop as it changes.
- **UI scale**, **UI font** (default Noto Sans) and **Code font** (default JetBrainsMono Nerd Font).
- **Smooth scrolling**: On, Off, or System, the default, which is on unless your desktop asks for reduced motion.

::: details In detail

Settings (Ctrl+, or the gear at the bottom of the sidebar) open in a dialog over the window and hold the theme, the UI scale, the UI font (default Noto Sans) and the code font (default JetBrainsMono Nerd Font), saved in `$XDG_CONFIG_HOME/rho-ui/settings`. The theme is dark by default, or light, or whichever the desktop uses (System), following it as it changes. System isn't the default because on Linux rho-ui starts before the desktop portal reports light or dark, so a dark desktop would see a light first frame, and a desktop with no portal would stay light. Both themes are rho-ui's own, in `crates/rho-ui/themes/rho-ui.json` (GPUI Kit's theme format), in place of Kit's stock ones.

A mouse wheel's notch glides over about a quarter of a second rather than jumping, and a touchpad swipe carries on and slows after the fingers lift. Smooth scrolling in Settings says whether they do: On, Off, or System, the default, which is on unless the desktop asks for reduced motion. On Windows that is Settings → Accessibility → Visual effects → Animation effects: with it off, scrolling jumps until Smooth scrolling is On.

:::

## Machines

The remote machines rho-ui reaches over ssh, with each one's name, `rho` path and clone folder. See [Remote machines](./machines).

## Source control

The forge CLIs each machine has, so far GitHub's `gh`: its version, where it was found, and the accounts it is signed in to. The [Pull request panel](./panel#pull-request) reads GitHub through it. If `gh` isn't found, set **Program** to its path. Signing in asks questions in a terminal, so the page offers the `gh auth login` command to copy.

On a remote machine, `gh` runs without a login shell. Set variables such as `GH_TOKEN` in a file a plain ssh command's shell reads, such as `~/.zshenv`.

::: details In detail

Source control shows the forge CLIs each machine has, so far GitHub's `gh`: its version and where it was found, and every account it is signed in to, with whether GitHub takes its token. rho-ui finds `gh` on the `PATH` its scripts run with, else through your login shell, as an ssh command may lack the `PATH` a terminal has; Program sets it instead, as an absolute or `~/` path, or a name on the `PATH`, saved in `$XDG_CONFIG_HOME/rho-ui/settings`. A Machine picker heads the page when there are remote machines, as each has its own `gh` and sign-ins. Signing in asks questions in a terminal, so an account that needs it offers `gh auth login` to copy; a token from a variable such as `GH_TOKEN` has to change where it is set, since `gh` prefers it. rho-ui runs `gh` without a login shell, so a variable such as `GH_TOKEN` or a proxy set only in a login profile doesn't reach it: on this machine, `gh` gets the environment rho-ui started with, from the desktop session; on a remote one, set it in a file an ssh command's shell reads, such as `~/.zshenv` for zsh. An account whose host couldn't be reached says so rather than asking you to sign in again; a check that takes over 60 s says so too. Opening the page checks the machine's CLIs if nothing has since it connected, and Refresh checks again; nothing checks them on a timer.

:::

## Providers

The model providers each machine's rho can use, signed in or not:

- **Signed in**: with **Sign out**. A key that comes from an environment variable can't be changed here, and the page names the variable.
- **API key**: a masked box. The provider checks the key before rho saves it.
- **Account login**, such as ChatGPT's or GitHub Copilot's: **Sign in** opens the provider's page in your browser and shows any code to enter. A remote machine signs in with a device code, which works from any browser.

The page also shows where rho keeps credentials: the OS keyring, or a file in rho's home. It can change that, which matters when a daemon started over ssh can't reach the keyring.

A session whose model isn't signed in still opens, with a card above the composer to sign in right there.

::: details In detail

Providers lists the auth profiles each machine's rho can run sessions on (`rho/auth/list`): those signed in, each with Sign out, then those that take an API key, each with a masked box and Save, then those that take an account's login (such as ChatGPT's or GitHub Copilot's), each with Sign in. A machine's rho keeps its own sign-ins, so a Machine picker heads the page when there are remote machines. A profile counts as signed in when rho has a credential for it, saved or in the environment rho-ui gives its sessions, where it can't be overwritten or signed out from here; rho says which variable holds it. Sign in starts the login in the daemon, which runs it; rho-ui opens its page in the browser (only an `https:` page: a remote daemon names it), and shows the page, with a button to copy it, and any code to enter there, until the provider approves it or Cancel stops it. On this machine the provider's page sends the browser back to the daemon here; a remote machine's daemon couldn't be reached that way, so it signs in with a device code, which any browser can take, where the provider has one; for a provider without, the prompt says to sign in on that machine or use an API key. One machine runs one login from here at a time. A new sign-in reaches every session at once, and running ones from their next message. The page also says where rho keeps credentials, its credential store: the OS keyring (the Secret Service on Linux, the Keychain on macOS) or a file in the rho home, and what chose it; the page can change it, which rho saves in its config; that matters most when rho can't reach the OS keyring, as a daemon started over ssh, without a session D-Bus, can't reach the Secret Service. `RHO_CREDENTIAL_STORE` in rho's environment chooses it instead, and the page says so. Nothing tells rho-ui of a sign-in made in a terminal, so the page reads the profiles again each time it opens and when another machine is picked.

A session whose model has no credentials on its machine still opens: rho-ui opens sessions with `sign_in: later`, as rho's terminal UI does, where rho would otherwise refuse to open one. A card above its composer says the model's auth profile isn't signed in, and signs it in there. A profile that takes an API key has a masked box for it, which Save or Enter sends to the daemon: the provider checks it, and a key it refuses is saved nowhere, with the provider's reason under the box; the box empties either way, so the key doesn't stay on screen. A profile that takes an account's login has Sign in, which starts the login in the daemon (see Providers below). Once the profile is signed in, the card goes. The composer still sends while it shows: a message sent signed out fails, saying why. A key the provider couldn't check, as a custom provider's or one behind a gateway that lists no models, is saved and signs the profile in, but may be wrong, so the card stays to say so, with a box to enter it again, until dismissed. The card follows the model, so switching to a model that is signed in makes it go too. rho tells nothing of a sign-out made elsewhere, as in rho's terminal UI, until a turn fails for it: when a turn fails, rho-ui asks again, and the card comes back if the profile is now signed out.

:::

## Worktrees

The git worktrees rho added for sessions, grouped by project. Each shows what a prune would do with it: remove it, once it is clean, merged, and its session is idle; or keep it, and why. **Remove** prunes the ones that can go, keeping their branches, as `rho worktrees prune` does.

::: details In detail

Worktrees lists the git worktrees each machine's rho added for sessions (`rho/worktree/prune` with `dry_run`), grouped by the project each came from. Each is named by its session's title, with its branch under it, or by its branch when there is no title to show: the session is gone or untitled, or rho-ui couldn't read which session it was (the prune names a worktree's job, so rho-ui finds the session in `rho/job/list`). Beside each is what a prune would do with it: remove it, once it is clean, merged into the current `HEAD` of the checkout it came from, and its session is neither loaded nor running, or keep it and say why: in use, uncommitted changes (flagged), not merged into that `HEAD`, or git's error (flagged). Those that need you lead their project, and one untouched for 14 days says so. A line heads the page with how many can be removed, and Remove prunes them as `rho worktrees prune` does, keeping each branch; the daemon checks each again, so the line then says how many it removed, and those leave the list. Opening Settings checks this machine's, picking a machine checks that one's, and the button beside the title checks again. A machine's rho keeps its own worktrees, so a Machine picker heads the page when there are remote machines.

:::

## Rho

rho's own settings, as each machine's daemon describes them, saved in that machine's `~/.rho/config.toml`. Each says what it does and when a change applies. Since the daemon describes them, settings a newer rho adds show up without a new rho-ui. Changes apply at once. If the file changed elsewhere since rho-ui read it, rho refuses the write and the page shows the settings as they are now.

::: details In detail

After rho-ui's own pages comes rho's: one page with a group for each category of settings rho's daemon describes (`rho/config/schema`), in its order, each listed under it in the sidebar, saved in that machine's `~/.rho/config.toml`. rho-ui draws them from the daemon's description alone, so a setting a newer rho adds shows up with no change to rho-ui. Each item is the setting's name, what it does and when a change applies (from the next turn, to new sessions, or once rho restarts), with a control for its kind: a switch, a dropdown of its choices, a number box held to its range with its unit, or a text box. Number and text boxes apply on Enter or when they lose the keyboard; an empty one resets the setting. A model setting shows its model, which `/config` in rho's terminal UI changes, and a kind rho-ui doesn't know is left out. A setting your file sets can go back to its default with the page's reset button. A change applies at once and shows at once; rho-ui sends one write at a time to each machine. If someone else changed the settings since rho-ui read them, in rho's terminal UI or by hand, rho refuses the write: the page says your change didn't land and shows the settings as they are now. Changes made elsewhere show up on an open page as they happen. When `config.toml` doesn't load, the page says why, beside the setting it names, and rho uses the last values that loaded.

Every machine's rho has its own `config.toml`. With remote machines, a Machine picker heads rho's page, set to this machine when Settings opens; picking another shows that machine's settings. A machine that is connecting or can't be reached says so in place of its settings.

:::

## About and the daemon

rho-ui's version, and for each machine the version of rho, its daemon's process ID, and **Restart** and **Stop**. Each asks first, saying what it would cut short: turns under way or waiting for you, logins, and sessions with background work.

- **Stop**: rho-ui won't start that daemon again by itself until you press **Start**, or until rho-ui restarts.
- **A daemon too old for rho-ui** refuses to connect. The sidebar offers **Restart** into the newer rho installed there, or says to update rho.
- **A daemon newer than rho-ui** says to update rho-ui.

::: details In detail

Last comes About: rho-ui's version, a link to its source and its license, then the version of Rho on each machine, or that it is connecting or can't be reached, with what that machine's rho needs under it, as the sidebar offers it (see [Installing and updating rho](./settings#installing-and-updating-rho)).
Settings → About shows each machine's daemon: its version and process ID, with Restart and Stop. Each asks first, saying how many things in progress it would cut short (turns under way or waiting for you, logins, and sessions with background work), as the daemon's status says at that moment. A restart cuts running turns short, and they carry on after it in a rho that resumes them; background processes stop.

- A daemon too old for rho-ui's protocol refuses to connect. The sidebar's footer says so with Retry and Restart, which asks first, saying what the daemon's refusal said a restart would cut short and how many other clients it disconnects, then runs `rho daemon restart` on the machine and connects again. That connects only if the rho installed there is new enough: if the restarted daemon still refuses, the footer says to update rho there instead of offering the restart again. Restart is offered only where rho-ui knows the machine's `rho`. A daemon newer than rho-ui says to update rho-ui, with Retry. On a remote machine, a daemon in use that is older than the rho installed there stops rho-ui's setup script at `rho daemon start`, which then fails with rho's message and Retry: the script's failure carries no version mismatch for rho-ui to offer the restart on.
- A daemon of another build than rho-ui's connects, but a request may fail: the footer warns so until it is gone. Builds have no order, so either side may be the older one. When the rho installed there is another version than the daemon runs, the footer offers Restart, which restarts the daemon in place into it; otherwise it says to update whichever is older.
- After Stop, rho-ui doesn't start or restart that daemon by itself, not even into an upgrade it was waiting for: the footer and About say it is stopped, with Start, which connects and so starts it. Quitting rho-ui forgets the stop: the next run starts the daemon as usual.

:::

## Installing and updating rho

rho-ui checks each machine's rho when it connects to it.

- **No rho**: rho-ui offers **Install Rho**, which runs rho's own installer.
- **A newer release**: the sidebar's footer offers **Update**, which runs `rho update`. When the update needs a terminal, as with pacman's `sudo`, it shows the command to copy instead.
- **After an update**, the daemon keeps running the old rho until nothing would be cut short. The footer says how many things it waits on, with **Restart now**. Open sessions reconnect after the restart, and what you typed stays.

::: details In detail

rho-ui checks each machine's rho when it connects to it, or fails to, by running `rho update --check --json` there, over ssh for a remote machine. A check that found rho is good for an hour, since each one asks GitHub, which allows 60 requests an hour from one address without a login. A `rho` given as plain `rho` that isn't on `PATH` falls back to `~/.local/bin/rho`, where rho's install script puts it (on Windows, `%LOCALAPPDATA%\Programs\rho\bin\rho.exe`, where its PowerShell installer does), since a desktop session's or an ssh command's `PATH` often lacks that folder.

- No rho on this machine, and so no daemon to connect to: the main pane offers Install Rho in place of the connection's error, which runs rho's install script (`https://matthewyjiang.github.io/rho/install.sh`) into `~/.local/bin`, or into the folder of a `--rho` path, then connects. On Windows it runs rho's PowerShell installer (`install.ps1`) instead, into `%LOCALAPPDATA%\Programs\rho\bin` or the folder of a `--rho` path. A remote machine without rho gets the same offer at the bottom of the sidebar, into the folder of its rho path in Settings. A machine whose daemon answers has rho, wherever the check looked, so it is never offered an install.
- A newer release: the sidebar's footer says so with Update, which runs `rho update`, so rho installs it the way it was installed. When that needs a terminal, as pacman's `sudo` does, the footer shows the command to copy instead.
- After an update, the daemon still runs the old rho until nothing would be cut short. While it waits on work, the footer says how many things it waits for, with Restart now, which asks first (see [Rho's daemon](./settings#about-and-the-daemon)). rho-ui asks the daemon about a waiting upgrade when its jobs change, at once after it updates rho, and otherwise every 30 seconds, since the daemon's status reads its credential store each time. That restarts the daemon at once, stopping its background processes. Once it is back, rho-ui follows every session it had open again, in the same views, so what you typed in a composer stays: rho carries on a turn the restart cut short when a client that takes turns the daemon starts attaches to its session again. Sessions other clients follow, such as rho's terminal UI, are theirs to carry on.

A rho from before `rho update --check` can't say whether a newer one is out, so rho-ui offers nothing for it; update it once with `rho update`.

:::
