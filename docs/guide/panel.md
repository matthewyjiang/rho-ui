# The right panel

The panel beside the transcript (<kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>B</kbd>) holds a terminal, the session's files, its diff, its pull request, and its background work. Each session has its own tabs, and they stay alive while you look at another session, terminal shells included, until rho-ui quits. Everything in the panel runs on the session's machine.

![A pull request in the panel, beside its session](/shots/pull-request-pr.webp)

## Terminal

- **Where it runs**: your `$SHELL` in the session's folder on this machine. On a remote machine, your login shell there, over rho-ui's own ssh connection, so this machine needs no `ssh` program.
- **Keys**: it uses Ghostty's ([Keyboard shortcuts](./keys#terminal)). It takes <kbd>Tab</kbd> for the shell, and off macOS also <kbd>Ctrl</kbd>+<kbd>B</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd> and <kbd>Ctrl</kbd>+<kbd>C</kbd>.
- **Mouse**: a program that takes the mouse, such as vim or htop, gets your clicks. Hold <kbd>Shift</kbd> to select or paste anyway.
- **Remote limits**: a remote machine runs at most 10 terminal shells at once, the sessions sshd allows on one connection by default (`MaxSessions`).
- **Remote shells**: they get `TERM=xterm-256color`, plus this machine's `LANG` and `LC_*` where the host's `AcceptEnv` allows. The command that starts the shell is POSIX, so a login shell of fish goes wrong with a folder whose name holds a backslash.

## Files and Diff

Both read the session's folder on its machine when the turn ends and on **Refresh**, not while the agent works. In a git repository they show what git sees: tracked files, and untracked files `.gitignore` doesn't exclude. Diff compares against `HEAD`, staged or not.

Both have limits: Files won't open a file over 2 MiB or a binary one, and lists at most 8 MiB of paths. Diff shows at most 8 MiB of changes.

## Pull request

Shows the GitHub pull request for the session's branch, with its checks, reviews and threads, and lets you comment, merge, and mark it ready or draft.

- **It needs `gh`.** It reads GitHub as this machine's account if that's signed in to the host, else as the session machine's. Comments and merges run on the session's machine. See [Source control](./settings#source-control).
- **Profile pictures need `curl`** on the machine that reads GitHub, which fetches each from where GitHub serves it. Without it, or for a picture that needs a sign-in, initials show.
- **Merging is guarded.** A merge only merges the head commit you were shown, so a commit the session pushed since doesn't slip in. It doesn't delete the branch, which the session may still use.
- **Reading is lazy.** It polls every 30 seconds, and only while its tab shows and the window has focus. When the account's API limit has 50 points or fewer left, it waits for the reset and says so.
- **Link** follows other PRs too. Paste a URL, `owner/name#N`, or a number in the session's repository. Links are kept between runs.
- **The sidebar marks each session's PR** beside its branch, with a red dot while its checks fail. It refreshes every 5 minutes.
