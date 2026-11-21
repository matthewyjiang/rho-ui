# The right panel

The panel at the right of the window holds tools that sit beside the session. Show or hide it with the toggle at the right end of the title bar, or <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>B</kbd>. Each tool opens as a tab; **+** opens another. Each session has its own tabs, and they stay alive while you look at another session, terminals included, until rho-ui quits.

::: details In detail

The panel at the right of the window holds surfaces, tools that sit beside the session. The toggle at the right end of the title bar, or Ctrl+Alt+B, shows and hides it; while it shows, the toggle sits at the same place in the panel's header. Drag its left edge to resize it, as far as leaves the session 30 rem (480px at the default scale) and no narrower than 16 rem; double-click the edge for its default width, 24 rem. rho-ui remembers whether it shows, and its width. With no surface open, the panel lists the surfaces to open; while one shows, + in its header opens a menu of the rest. Each surface opens as a tab; hover one and × takes the place of its icon to close it. Drag a tab onto another to move it there; tabs that don't fit scroll sideways. Each session has its own tabs: switching sessions switches to that session's, and a session on screen for the first time has none open. A session's tabs live on while another shows, its terminals' shells too, until rho-ui quits. With no session on screen, as on a draft, the panel has no tabs. There are five surfaces so far.

:::

## Terminal

A shell in the session's folder, on the session's machine: your `$SHELL` here, or your login shell on a remote machine, over rho-ui's own ssh connection.

![Two terminal panes side by side](/shots/terminal-tabs-split.webp)

Open as many tabs as you like. Right-click for **Split right** and **Split down**, or use Ghostty's keys ([Keyboard shortcuts](./keys#terminal)). Each tab is named for the program running in it, like `zsh` or `vim`. It scrolls back 10,000 lines. Programs that use the mouse, such as vim or htop, get it; hold <kbd>Shift</kbd> to select anyway. The emulator is Ghostty's terminal core, drawn in the app's theme and code font.

A remote machine runs at most 10 terminal shells at once, the sessions sshd allows on one connection by default.

::: details In detail

Terminal runs a shell in the session's folder, on the session's machine: on this one, your `$SHELL`; on a remote one, your login shell there, under a pseudo-terminal sshd makes, over an ssh connection of rho-ui's own that the machine's terminals share, so this machine needs no `ssh` program, and your key agent asks once more, for the first terminal there, not for each. It is told it is an `xterm-256color`, and gets `COLORTERM=truecolor` and this machine's locale (`LANG` and `LC_*`, as a `SendEnv LANG LC_*` in your ssh config would send them) where the host's `AcceptEnv` lets them through. sshd hands the command that starts it, `cd` into the folder then `exec "$SHELL" -l`, to your login shell there, so that shell must read POSIX quoting (sh, bash and zsh do; with fish, a folder whose name holds a backslash goes wrong). It opens a new tab each time you ask, as many as you like. A tab is named for the program its shell runs, such as `zsh` or `vim`: on this machine the terminal's foreground process (on Linux), on a remote one the title the shell sets, else just Terminal. Right-click a terminal for its menu: Copy, Paste, Split right, Split down, and Close. A split puts a new pane beside or below, each pane with a shell of its own; drag the divider between two panes to resize them. Closing a tab's last pane closes the tab, and a tab is named for the pane last clicked or typed in. Ghostty's keys do the same: Ctrl+Shift+T opens a terminal tab, Ctrl+Page Down and Ctrl+Page Up go to the next and previous one, Ctrl+Shift+O splits right, Ctrl+Shift+E splits down, Ctrl+Shift+W closes the pane, and Ctrl+Alt+arrow moves to the pane that way. On macOS they are Ghostty's Mac keys: Cmd+T, Cmd+Shift+] and Cmd+Shift+[, Cmd+D, Cmd+Shift+D, Cmd+W, and Cmd+Alt+arrow. They act on the terminal you type in, or, while you type elsewhere, the one under the pointer; with neither, the key does what it would anyway, such as Ctrl+Shift+E selecting to the end of the line in a text box. Closing a pane you only point at leaves the keyboard where it is. A terminal tab belongs to its session, like every tab, so its shells run in that session's folder. Click the terminal to type in it. It takes Tab for the shell rather than the app, and off macOS Ctrl+B, Ctrl+N and Ctrl+C too, where the app's shortcuts use Ctrl; Ctrl+Shift+V pastes (Cmd+V on macOS), the wheel scrolls back through 10,000 lines, and typing returns to the bottom. Drag to select text, double-click for a word, triple-click for a line, and hold Ctrl+Alt while dragging for a rectangle; dragging past the top or bottom scrolls, selecting as it goes. Ctrl+Shift+C (Cmd+C on macOS) copies the selection, and on Linux letting go of the mouse also puts it in the primary selection, which a middle click pastes, in the terminal or elsewhere. Typing clears the selection. A program that asks for the mouse, such as vim, htop, or less with `--mouse`, gets its clicks, drags and wheel instead; hold Shift to select, paste, or open the menu anyway. Input methods work as in any text box: what you compose shows underlined at the cursor, and goes to the shell once you commit it. When the shell on screen exits, as with `exit` or Ctrl+D, its pane closes like a closed pane, even while a job it started in the background still runs, and the tab with its last pane, also while another session is on screen. A shell that couldn't start says why, with Restart to try again; so does a remote shell whose connection drops. The terminals' connection is apart from the one rho-ui's own reads of the machine use (the Files, Diff and Pull request surfaces, pictures, git), so they never take each other's ssh sessions. A remote machine runs at most 10 terminal shells at once, the sessions sshd allows on a connection by default (`MaxSessions`); an eleventh doesn't start, and says so: exit a shell there, or close a pane, which keeps a shell for each of the machine's sessions it has shown. Closing a pane hangs up on its shells at once. An open remote terminal keeps the terminals' connection open: the 10 minutes it lingers start once the last terminal there goes. Ghostty's terminal core, libghostty-vt 1.0.1, keeps the screen, encodes keys and the mouse, and finds what a click selects; rho-ui draws it in the theme's code font and colors.

:::

## Files

The session's folder as a tree. In a git repository it lists what git sees: tracked files, and untracked files that `.gitignore` doesn't exclude. Click a file to read it with syntax highlighting. It refreshes when the turn ends, and on **Refresh**.

::: details In detail

Files lists the session's folder as a tree, folders first, each by name. In a git repository it lists what git sees: tracked files that are there, and untracked files that `.gitignore` doesn't leave out; outside one, every file under the folder. Click a folder to open or shut it, and a file to read it in place of the tree, read-only, its syntax highlighted by its extension, with line numbers; Back returns to the tree as you left it. A file over 2 MiB, or one holding a NUL (git's test for binary), shows a note instead. The list stops at 8 MiB of paths, and says so. Like Diff, it reads the files on the session's machine, again when its session comes back on screen, when its turn ends, and on Refresh, and reads the open file again with them; a read that finds the same leaves the view where it was scrolled.

:::

## Diff

The repository's uncommitted changes: everything changed against `HEAD`, staged or not, plus new untracked files, with a count of lines added and removed per file. It refreshes when the turn ends, and on **Refresh**.

![The uncommitted changes in the Diff panel](/shots/diff-changes.webp)

::: details In detail

Diff shows the uncommitted changes in the repository the session works in: every change to tracked files against `HEAD`, staged or not, and each untracked file that `.gitignore` doesn't leave out, as a new file, with how many files changed and lines added and removed, and each file's own count in its header. Long lines wrap to the panel's width. It runs `git` on the session's machine, over the ssh connection for a remote one, and reads the changes again when its session comes back on screen, when its turn ends, and on Refresh; a read that finds the same changes leaves the view where it was scrolled. Changes made during a turn show once it ends, or on Refresh. A folder outside any git repository says so. Diff shows at most 8 MiB of changes: past that it shows the whole files that fit and says where it stopped. A pull request's diff is for later, with GitHub's `gh`.

:::

## Pull request

The pull request for the branch the session works on, if its repository is on GitHub. It shows:

- Its state, title, author, branches and size.
- Its review decision.
- Its checks, failed ones first. Click one to open its log.
- Its description, comment threads on the code, and conversation.

![A pull request with its checks, description and review threads](/shots/pull-request-pr.webp)

Post a comment from the box at the end. You can also merge it (in the way you last chose, with the repository's others in the button's menu), mark a draft ready for review, or convert it to a draft. A merge asks first, and merges only the head commit you were shown, in case the session has pushed since.

**Link** follows more PRs than the branch's own: paste a URL, `owner/name#N`, or a number. Each opens in its own tab.

The sidebar marks each session's PR beside its branch, with a red dot while its checks fail.

The panel reads GitHub through the `gh` CLI, as the account signed in on the session's machine (or this one). It polls every 30 seconds, and only while its tab is showing and the window has the focus. See [Source control](./settings#source-control).

::: details In detail

Pull request shows the pull request whose head is the branch the session works on, in a repository whose origin is on GitHub: whether it is open, a draft, merged or closed, its number and title, its author and branches, the lines it adds and removes and the files it touches, whether its review approved it, its checks with the ones that failed first (a click opens one's log; the first 50 show, with how many more there are), and its description. Under them are its comment threads on the code, each under its file and line, open ones open and resolved ones closed until clicked; then its conversation: its comments and reviews in one timeline, oldest first, each with who wrote it, when, and what a review decided (a click on its age opens it on GitHub). It reads the newest 30 comments, 30 reviews and 30 threads of 10 comments, and says how many older ones are on GitHub. A box at the end posts a comment, with the Comment button or Ctrl+Enter (⌘Enter on macOS), through `gh pr comment` on the session's own machine, as its account there, which the box names; then the PR is read again to show it. Replies in threads, and reviews, stay on GitHub. It reads GitHub through `gh` (see Source control): this machine's when it is signed in to the repository's host, as that needs no ssh, else the session machine's; a repository this machine's account can't see is read from the session's machine until Refresh. A branch's PR is the newest one from the repository itself, an open one first; a fork's PR with the same branch name doesn't count, and when the 5 newest PRs from that branch name are all forks', the surface says so. The session's default branch, or a checkout on no branch, has none. One GraphQL query reads it, every 30 seconds while the surface shows, when the session's turn ends, and on Refresh; the PR's detail is read again only when it changed or its checks run. Nothing is read while the surface's tab isn't the one shown, the panel is hidden, or the window doesn't have the focus; on coming back, it reads at once if the last read is 30 seconds old. When a read fails, the surface keeps what it read before and says why above it. When GitHub's API limit for the account has 50 or fewer points left, polling waits for it to reset, and the surface says so and within how many minutes. Refresh reads it all again, and checks `gh` again when no machine could read.

Under the checks, an open PR offers to merge, in the way the account last chose where the repository allows it, with the repository's other ways in the button's menu, and to convert it to a draft; a draft offers Ready for review. A merge asks to confirm, merges only the head commit the surface showed when it was asked for (the session may push more since), and doesn't ask to delete the branch, which a session may still work on (a repository set to delete head branches on merge still does); a PR that conflicts with its base can't merge, and says so. These run `gh pr merge`, `gh pr ready` and `gh pr ready --undo` as the comment box does, on the session's machine, then read the PR again; when `gh` refuses, the surface says what it said.

A session can follow more PRs than its branch's: Link (the chain at the top) takes a PR's URL, `owner/name#N`, or `#N` or `N` in the session's repository, also a fork's PR, and opens it in a Pull request tab of its own, after the others; a number with no PR says so. Each Pull request tab is named for its PR's number, as `#12`, the branch's first (named Pull request until its PR is found). Unlink (beside Link), or × on a linked PR's tab, unlinks it and closes its tab. Closing the branch's tab closes the linked PRs' tabs too, which stay linked and open again with it. Links are kept between runs.

In the sidebar, each active session's row marks its PR after the branch (its branch's, else the first PR linked to it): the icon of its state, its number, and a red dot while its checks fail. These are read with one GraphQL query of each machine's sessions per host, without their details, when a session comes into the list and every 5 minutes while the window has the focus; the session on the Pull request surface is read by the surface instead.

:::

## Background work

The session's running processes, subagents and workflow runs, each with a stop button, then what has ended. Under a workflow it lists the agents it started.

::: details In detail

Background work lists the session's running work, each with a button to stop it, then what ended, newest first, as the title bar's list does; a subagent's row opens its session. Under each run it lists the runs that one started, from the daemon's job list: a workflow's agent nodes, or a subagent's own subagents, each with its state, place in line, or error, and each subagent's row opens its session.

:::
