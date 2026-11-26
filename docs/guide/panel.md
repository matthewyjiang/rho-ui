# The right panel

The panel beside the transcript (<kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>B</kbd>) holds a terminal, side chats, the session's files, its diff, its pull request, its background work, and what fills its context, and for a lead, its work and its brief (see [Leads](./sessions#leads)). Each session has its own tabs, and they stay alive while you look at another session, terminal shells included, until rho-ui quits. Everything in the panel runs on the session's machine.

![A pull request in the panel, beside its session](/shots/pull-request-pr.webp)

## Terminal

- **Where it runs**: your `$SHELL` in the session's folder on this machine. On a remote machine, your login shell there, over rho-ui's own ssh connection, so this machine needs no `ssh` program.
- **Keys**: it uses Ghostty's ([Keyboard shortcuts](./keys#terminal)). It takes <kbd>Tab</kbd> for the shell, and off macOS also <kbd>Ctrl</kbd>+<kbd>B</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd> and <kbd>Ctrl</kbd>+<kbd>C</kbd>.
- **Mouse**: a program that takes the mouse, such as vim or htop, gets your clicks. Hold <kbd>Shift</kbd> to select or paste anyway.
- **Remote limits**: a remote machine runs at most 10 terminal shells at once, the sessions sshd allows on one connection by default (`MaxSessions`).
- **Remote shells**: they get `TERM=xterm-256color`, plus this machine's `LANG` and `LC_*` where the host's `AcceptEnv` allows. The command that starts the shell is POSIX, so a login shell of fish goes wrong with a folder whose name holds a backslash.

## Side chat

A side chat asks something aside without changing the session. Start one with `/side` or `/btw` and a question in the session's composer, or open **Side chat** in the panel for an empty one. You can have several.

- **It's a fork.** It starts from the session's conversation as rho last saved it, so a turn still under way isn't in it, and works in the same folder as the same agent on the same model. Nothing in it goes back to the session.
- **It asks before it acts.** It opens in Supervised mode, or Plan if the session is in Plan, and rho tells its model it's a side chat that should leave the workspace alone. Change its mode in its footer if you want it to edit.
- **Closing its tab deletes it**, stopping its turn and background work first. The panel's Files and Diff tabs reload when the session's turn ends, not a side chat's. Side chats don't come back after rho-ui quits: rho keeps them, unlisted, until you delete their session, which deletes them too.
- **A session forks once its first message is sent.** A side chat can't start another, and neither can a subagent's session. It needs rho 3.5 or newer; an older rho refuses.

## Files and Diff

Both read the session's folder on its machine when the turn ends and on **Refresh**, not while the agent works. In a git repository they show what git sees: tracked files, and untracked files `.gitignore` doesn't exclude. Diff compares against `HEAD`, staged or not.

Both have limits: Files won't open a file over 2 MiB or a binary one, and lists at most 8 MiB of paths. Diff shows at most 8 MiB of changes.

## Pull request

Shows the GitHub pull request for the session's branch, with its checks, reviews and threads, and lets you comment, merge, and mark it ready or draft.

- **The session machine's rho reads it.** Each machine's rho daemon reads GitHub for its own sessions, with a `gh` it finds on its own, signed in on that machine. It needs a rho newer than 3.3; an older one says to update rho. Comments and merges run rho-ui's `gh` on the session's machine. See [Source control](./settings#source-control).
- **Only github.com origins get a branch PR.** A session whose `origin` is on another host, GitHub Enterprise included, shows none for its branch; link one by its URL instead.
- **Profile pictures need `curl`** on the session's machine, which fetches each from where GitHub serves it. Without it, or for a picture that needs a sign-in, initials show. Refresh asks again for the ones that didn't load.
- **Images in the description and comments need `curl` too**, the same way. Those on the PR's own host, such as a private repository's uploads, are fetched signed in with that machine's `gh`. Only https links on the PR's host and GitHub's own image hosts load, PNG, JPEG, GIF or WebP, up to 10 MiB, 32 images and 1 GiB decoded a PR (an animated GIF keeps every frame), with curl 7.54 or later: the session's machine may sit in a private network, and anyone who comments can link an image. Others show as a link to open in the browser. Refresh, or a click on one, asks again for those that didn't load.
- **Merging is guarded.** A merge only merges the head commit you were shown, so a commit the session pushed since doesn't slip in. It doesn't delete the branch, which the session may still use.
- **Reading is lazy.** rho reads the PR every 30 seconds, and only while its tab shows and the window has focus. When the account's API limit has 50 points or fewer left, rho waits for the reset and the panel says so.
- **Link** follows other PRs too. Paste a URL, `owner/name#N`, or a number in the session's repository. Links are kept by the session's rho, so they follow the session to any rho-ui. The PRs the session's agent links or watches are linked too, marked **watched** while the agent waits on them.
- **One tab holds all of a session's PRs.** It opens on the branch's PR, or the one linked last when the branch has none, and stays on the PR you're reading until you switch or unlink it, even as the agent links more. The menu on the PR's name switches between them. Closing the tab only closes it. **Unlink** is the only thing that unlinks a PR, and it ends the agent's watch on it.
- **The sidebar and the title bar mark each session's PR**, with how its checks stand while it is open, so it shows with the panel hidden; `+1` counts the other PRs linked to the session. Away from its tab, rho reads a PR every 5 minutes, so a mark can trail GitHub by that long.
- **A watched PR keeps its session going.** While the agent watches a PR, rho wakes the session when the PR changes: its row says **Watching** where it would say Done, and the title bar's mark says **watched**. See [Sessions](./sessions#pinned-snoozed-and-settled).

## Context

Shows how full the session's context window is, where automatic compaction starts, and what fills the window: the system prompt, tool schemas, messages and tool results.

- **It needs a recent rho.** The session machine's rho must report on sessions, which rho does from rho-protocol 1.6; an older one says to update rho.
- **It reads between turns.** rho reports on a session only while no turn runs, so the tab reads when it opens, when a turn ends, and when you click the context ring. During a turn it keeps the numbers it read before.
- **Left out** means the conversation holds blocks another provider's model wrote, which the session's model can't be sent: its requests leave them out.
