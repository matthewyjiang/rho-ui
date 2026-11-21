# The right panel

The panel at the right of the window holds tools that sit beside the session. Show or hide it with the toggle at the right end of the title bar, or <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>B</kbd>. Each tool opens as a tab; **+** opens another. Each session has its own tabs, and they stay alive while you look at another session, terminals included, until rho-ui quits.

## Terminal

A shell in the session's folder, on the session's machine: your `$SHELL` here, or your login shell on a remote machine, over rho-ui's own ssh connection.

![Two terminal panes side by side](/shots/terminal-tabs-split.webp)

Open as many tabs as you like. Right-click for **Split right** and **Split down**, or use Ghostty's keys ([Keyboard shortcuts](./keys#terminal)). Each tab is named for the program running in it, like `zsh` or `vim`. It scrolls back 10,000 lines. Programs that use the mouse, such as vim or htop, get it; hold <kbd>Shift</kbd> to select anyway. The emulator is Ghostty's terminal core, drawn in the app's theme and code font.

A remote machine runs at most 10 terminal shells at once, the sessions sshd allows on one connection by default.

## Files

The session's folder as a tree. In a git repository it lists what git sees: tracked files, and untracked files that `.gitignore` doesn't exclude. Click a file to read it with syntax highlighting. It refreshes when the turn ends, and on **Refresh**.

## Diff

The repository's uncommitted changes: everything changed against `HEAD`, staged or not, plus new untracked files, with a count of lines added and removed per file. It refreshes when the turn ends, and on **Refresh**.

![The uncommitted changes in the Diff panel](/shots/diff-changes.webp)

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

## Background work

The session's running processes, subagents and workflow runs, each with a stop button, then what has ended. Under a workflow it lists the agents it started.
