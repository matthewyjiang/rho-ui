# Settings

rho-ui's own settings live in `~/.config/rho-ui/settings`, apart from rho's: they have to apply before the first frame, before any daemon answers. rho's settings are saved in each machine's `~/.rho/config.toml`. Pages that depend on the machine have a **Machine** picker, since each machine has its own `gh`, sign-ins, worktrees and rho config.

## Smooth scrolling

**System**, the default, follows your desktop's animation setting. On Windows that's **Settings → Accessibility → Visual effects → Animation effects**: with it off, scrolling jumps until you set Smooth scrolling to **On**.

## Source control

The Pull request panel's comments and merges run `gh`. rho-ui looks for it on `PATH`, then through your login shell, then among [mise](https://mise.jdx.dev)'s shims (`MISE_SHIMS_DIR`, else `shims` in mise's data folder, `~/.local/share/mise` by default); set **Program** if it lives elsewhere. A shim runs only when mise has a `gh` version to run, as from its global config. Reading the PR is rho's job: each machine's rho daemon finds `gh` on its own, whatever **Program** says. A rho newer than 3.5.0 looks where rho-ui does (`PATH`, your login shell, mise's shims); rho 3.4 and 3.5 check only their `PATH` and your login shell. Signing in to `gh` asks questions in a terminal, so run `gh auth login` there.

rho-ui runs `gh` without a login shell. Variables such as `GH_TOKEN`, or a proxy set only in a login profile, don't reach it:

- **On this machine**, `gh` gets the environment rho-ui started with, from your desktop session.
- **On a remote machine**, set them in a file a plain ssh command's shell reads, such as `~/.zshenv`.

## Providers

Sign-ins belong to each machine's rho, and reach its sessions at once.

- **Keys from the environment**, such as `ANTHROPIC_API_KEY`, count as signed in but can't be changed or signed out here. The page names the variable.
- **Account logins on a remote machine** use a device code, which works from any browser, where the provider offers one.
- **Sign-ins made in a terminal** show the next time the page opens.
- **The credential store** is the OS keyring or a file in rho's home, and the page can switch it. A daemon started over ssh often can't reach the Secret Service on Linux, so a remote machine may need the file store. `RHO_CREDENTIAL_STORE` in rho's environment overrides the choice.

## Worktrees

Lists the worktrees rho added for sessions, and **Remove** prunes them as `rho worktrees prune` does. A worktree can be removed once it is clean, merged into its checkout's current `HEAD`, and its session is idle. Branches are kept.

## Rho

rho's own settings, as each machine's daemon describes them, so settings a newer rho adds show up without a new rho-ui. Each says when a change applies. If `config.toml` changed elsewhere since the page read it, rho refuses the write and the page shows the settings as they are now.

A model setting lists the models that machine's rho can run with its sign-ins, and the list follows each sign-in and sign-out. If rho can't list them, the page says why and shows the model without a picker. A rho too old to list them also shows the model alone: change it with `/config` in rho's terminal UI.

## MCP & hooks

What each machine's rho sets up for its sessions: its MCP servers, from `config.toml` and plugins, and its hooks files, the user's `~/.rho/hooks.toml` and a project's `.rho/hooks.toml`. The page only reads them; change them in those files, then refresh. It starts no server and runs no hook, so a server that is enabled here can still fail to start in a session: the plug mark in a session's composer says so.

- **Project.** A project's plugins and hooks file apply only to sessions in it, so the page reads for one project: the folder of the session or draft on screen when Settings opened (and its machine), else none, which lists your own alone. For a session in a worktree that's the worktree, whose hooks file the session loads, not the main checkout's; for a lead, the first project it works in. Pick another of that machine's projects to see what its sessions get.
- **Trusting a project.** rho loads a project's hooks file only when the environment rho-ui gives its sessions sets `RHO_TRUST_PROJECT_HOOKS=1`, and a project's plugins, with their MCP servers, only with `RHO_TRUST_PROJECT_PLUGINS=1`. Either trusts every project on that machine, not just one. On this machine that environment is the one rho-ui started with; on a remote machine, the login environment there. Restart rho-ui after changing it. An untrusted project's MCP servers don't show at all: rho doesn't list the plugins it skipped.
- **An invalid hooks file** stops every hook, the other file's too, until it's fixed.
- **A rho too old** to list them says so; update rho on that machine.

## The daemon

**Settings → About** shows each machine's daemon, with **Restart** and **Stop**.

- **Restart** cuts running turns short. They carry on afterwards, but background processes stop.
- **Stop** sticks: rho-ui won't start that daemon again by itself until you press **Start** or rho-ui restarts.
- **A daemon too old for rho-ui** refuses to connect. The sidebar offers to restart it into the newer rho installed there, or says to update rho. On a remote machine where the daemon is busy and older than the rho installed there, rho-ui can only show rho's message with **Retry**.
- **A daemon newer than rho-ui** says to update rho-ui.
- **A daemon of another build**, such as one built from source, speaks the same protocol, but some requests may fail. The sidebar warns until you close the warning or turn off **Warn about other builds** in About.

## Updating rho

rho-ui checks each machine for a newer rho when it connects, at most once an hour, because GitHub allows 60 unauthenticated requests an hour. **Update** runs `rho update`, so rho updates itself the way it was installed. When that needs a terminal, as pacman's `sudo` does, rho-ui shows the command to copy instead.

After an update, the daemon keeps running the old rho until nothing would be cut short. **Restart now** doesn't wait. A rho too old for `rho update --check` gets no offer: update it once by hand.
