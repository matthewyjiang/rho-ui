# Sessions

The sidebar lists the daemon's saved sessions, newest first, from every machine you've added. A session that is running a turn shows **Working**, one that waits on you shows **Needs you**, and one that finished while you were elsewhere shows **Done** until you open it. That holds whichever client started the turn, rho's terminal UI included.

## Starting a session

<kbd>Ctrl</kbd>+<kbd>N</kbd> opens a new session in the project of the session on screen. Above the composer, **Start a session in** lists your projects, most recently used first. Click one to move the draft there, or press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> to pick one from the keyboard.

![A new session's project list, with New worktree chosen](/shots/worktree-chosen.webp)

To add a project:

- Type an absolute or `~/` path in the search to browse folders, and choose **Add** on the one you want.
- **Add project…** opens your desktop's folder chooser.
- Paste a git URL (`https://…`, `ssh://…` or `git@host:owner/repo`) and choose **Clone**. rho-ui clones it into the machine's clone folder, adds it, and starts the session there.

A project is a folder, or for a git repository its main checkout. Hover a project and click × to take it off the list; the folder stays.

### Worktrees

In a git project, the button beside **Add project…** chooses where the session works:

- **Local**: the project's own checkout.
- **New worktree**: a fresh git worktree on a new `rho/…` branch from `HEAD`, so sessions in one repository never write the same files.

Nothing is created until you send the first message, so switching back and forth leaves nothing behind. The daemon owns the worktree and removes it once it is clean and merged, keeping the branch. [Settings → Worktrees](./settings#worktrees) lists them and prunes what can go.

### Session settings

The composer's footer holds the session's settings. On the left is the permission mode, which decides which tool calls run without asking. On the right is the model, with its reasoning level and fast mode. A ring beside it fills as the context window does; hover it for the tokens in use and what the session has cost. A change made while a turn runs applies when the turn ends.

### Commands and agents

Type `/` at the start of a message, or `$` anywhere in it, to complete the session's slash commands and skills: rho's own (`/compact`, `/init`, `/create-agent`), prompt templates, MCP prompts, workflows and agents. To run a new session as one of the workspace's agents, start its first message with `/agent:<name>` and the task.

### Attachments

Attach files with the paperclip, by dropping them on the composer, or by pasting them. Images go to the model as images. A text file of up to 200,000 characters goes whole. Anything else (a PDF, a binary, a longer text file) goes as its path, and the agent reads it with its own tools.

### Drafts

Leave a new session with something typed in it and it waits at the top of the sidebar as a **Draft**. Click it to pick up where you left off. Drafts last for the current run of the app.

## Organizing the sidebar

Right-click a session for its menu:

- **Rename…**: give it a title. rho keeps yours over one it generates.
- **Pin**: keep it at the top.
- **Snooze** for 1 hour, 4 hours or 1 day: hide it until then.
- **Settle**: file it away. It comes back if it gets new activity.
- **Copy** the session ID, the `rho -R` command that resumes it in a terminal, or its folder's path.
- **Delete…**.

Snoozed and settled sessions sit folded under the active ones. Sessions that started outside rho-ui, as in rho's terminal UI, start out settled so the sidebar doesn't fill with old history; any session with work under way always shows as active. Pinning, snoozing and settling are rho-ui's own, kept in its state database.

The search matches titles and project names, and the filter beside it hides machines or keeps to the projects you choose. <kbd>Ctrl</kbd>+<kbd>B</kbd> collapses the sidebar to a rail, which still shows when a session needs you. Drag its edge to resize it.

## Extra directories

A session's tools reach its folder. To give them another one, use the folder button in the title bar: type a path and press <kbd>Enter</kbd>. rho saves it with the session and tells the model from its next turn on.

## Opening and letting go

rho-ui follows a session while it is on screen and while it has anything under way: a turn, a request for you, background work, or something typed in its composer. Once a session you left has none of these, rho-ui lets it go so the daemon can unload it. Its row stays. Opening it again replays it from the daemon, which takes a fraction of a second even for the largest sessions.
