# Sessions

The sidebar lists the daemon's saved sessions, newest first, from every machine you've added. A session that is running a turn shows **Working**, one that waits on you shows **Needs you**, and one that finished while you were elsewhere shows **Done** until you open it. That holds whichever client started the turn, rho's terminal UI included.

::: details In detail

The sidebar lists the daemon's saved sessions. Once a second it also asks the daemon which sessions have work under way, so a session that runs a turn or waits for an approval shows as Working or Needs you whichever client started it. If the daemon refuses to say, the sidebar's footer says why, as it does when the saved sessions can't be listed, until it answers again. A machine whose connection dropped keeps its saved sessions listed; opening, renaming or deleting one says the machine isn't connected, and a rename keeps the old title. A turn that finishes while its session isn't on screen shows Done, in green, until you open the session or it works again. While background work the session's turns started still runs (a subagent, a workflow run, a process), the row says Waiting, by an hourglass, in place of Done; Done shows once that work ends. The daemon's job list names a session's subagents and workflow runs but not its processes, so for a session rho-ui doesn't follow (see below), only those make it wait.

The title bar shows the session's project and its title. Branch and other git details are not in it. Click the project to start a new session in it, as Ctrl+N does.

:::

## Starting a session

<kbd>Ctrl</kbd>+<kbd>N</kbd> opens a new session in the project of the session on screen. Above the composer, **Start a session in** lists your projects, most recently used first. Click one to move the draft there, or press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> to pick one from the keyboard.

![A new session's project list, with New worktree chosen](/shots/worktree-chosen.webp)

To add a project:

- Type an absolute or `~/` path in the search to browse folders, and choose **Add** on the one you want.
- **Add project…** opens your desktop's folder chooser.
- Paste a git URL (`https://…`, `ssh://…` or `git@host:owner/repo`) and choose **Clone**. rho-ui clones it into the machine's clone folder, adds it, and starts the session there.

A project is a folder, or for a git repository its main checkout. Hover a project and click × to take it off the list; the folder stays.

::: details In detail

A new session (Ctrl+N) starts in the project of the session on screen, else where the last new session started, else `DIR`. Above the draft's composer, "Start a session in" lists your projects, the most recently used first, with the draft's project checked; click one to move the draft there. rho-ui finds no projects on its own: you add them. Type an absolute or `~/` path in the search to browse folders, by the rules of the draft's machine (on Windows, `C:\code` or `~\code` here; a remote machine's paths are POSIX ones, such as `/srv/app`): the list shows the folders the path can go on to (choose one to go into it), and a folder that is there offers Add. Add project… opens the desktop's folder chooser on this machine, and starts the search at `~/` on a remote one. Paste a git URL (`https://…`, `ssh://…`, `git@host:owner/repo`) and the list offers Clone, as does the footer's button in place of Add project…: Enter clones it into the machine's clone folder, adds the clone to your projects, and starts the session there. A URL of a repository the machine already holds lists its checkout instead. A project is a folder, and for a git repository its main checkout, so adding a linked worktree or a subfolder adds the repository it belongs to, and sessions in its worktrees count toward its age. Hover a project and click × (or right-click it) to remove it from the list; its folder stays. rho-ui keeps the list in its [state database](../architecture#state). Ctrl+Shift+N starts a session with the keyboard in the list's search, which filters by name or path. In a git project, the button beside Add project… chooses where the session works: Local, the project's checkout, or New worktree, a git worktree that the rho daemon adds on a new `rho/…` branch from `HEAD`, so sessions in one repository don't write the same files. Choosing it makes nothing: the worktree is added when the first message goes, so switching back and forth leaves none behind. New sessions start with the last choice, across restarts too; a folder outside git works in Local without forgetting it. The draft's own session, which holds its settings, works in the checkout; the first message opens a session in the new worktree, gives it the draft's settings, and runs there. The daemon owns the worktree: it removes it once it is clean and merged, keeping the branch. Settings → Worktrees, or `rho worktrees prune`, removes those it can now. A session's directory is fixed once it opens, so moving the draft lets its session go and opens another in the new folder: the composer's text stays, and the settings start over from the new session's.

:::

### Worktrees

In a git project, the button beside **Add project…** chooses where the session works:

- **Local**: the project's own checkout.
- **New worktree**: a fresh git worktree on a new `rho/…` branch from `HEAD`, so sessions in one repository never write the same files.

Nothing is created until you send the first message, so switching back and forth leaves nothing behind. The daemon owns the worktree and removes it once it is clean and merged, keeping the branch. [Settings → Worktrees](./settings#worktrees) lists them and prunes what can go.

### Session settings

The composer's footer holds the session's settings. On the left is the permission mode, which decides which tool calls run without asking. On the right is the model, with its reasoning level and fast mode. A ring beside it fills as the context window does; hover it for the tokens in use and what the session has cost. A change made while a turn runs applies when the turn ends.

::: details In detail

The composer's footer holds the session's settings: the permission mode (which tool calls run without asking) on the left, and on the right one button naming the model and reasoning, which opens the model list with search beside the reasoning levels and fast mode. Beside that button, a ring fills as the context window does; hover it for the tokens in use and what the session has cost. New session opens the session right away, so these can be set before the first message; rho writes no file for a session until its first prompt, and the daemon discards an unused draft's session, so nothing is left behind. A setting changed while a turn runs applies when the turn ends.

:::

### Commands and agents

Type `/` at the start of a message, or `$` anywhere in it, to complete the session's slash commands and skills: rho's own (`/compact`, `/init`, `/create-agent`), prompt templates, MCP prompts, workflows and agents. To run a new session as one of the workspace's agents, start its first message with `/agent:<name>` and the task.

::: details In detail

Type `/` at the start of a message, or `$` anywhere in it, and a menu above the composer lists the session's slash commands or skills that start with what you typed. The daemon lists them: rho's own commands it runs (`/compact`, `/init`, `/create-agent`), prompt templates, MCP prompts, workflows and agents, and the skills the session can load. To run a new session as one of the workspace's agents, make its first message `/agent:<name>` and the task: rho binds the session to that agent, which takes over its settings, and runs the task. Up and Down move the highlight, Tab completes, and Enter completes unless the name is typed out, or reads as an amount such as `$20`: then Enter sends. Escape closes the menu, without stopping a turn. A completed name goes as typed, and rho runs the command or loads the skill. Commands rho's terminal UI runs itself, such as `/model`, aren't offered.

:::

### Attachments

Attach files with the paperclip, by dropping them on the composer, or by pasting them. Images go to the model as images. A text file of up to 200,000 characters goes whole. Anything else (a PDF, a binary, a longer text file) goes as its path, and the agent reads it with its own tools.

::: details In detail

Attach any file with the paperclip at the left of the composer's footer, by dropping it on the composer, or by pasting it (Ctrl+V) after copying it in a file manager; paste an image the same way. Each shows above the composer's text: an image as a tile, any other file as a card. An image over 4096 px a side, too large for a texture, shows as a card saying it is too large to preview, with its size, and still goes as an image; so it does in the transcript. When files can't be attached, one alert says why for each. Hover one to remove it, or press Backspace in the empty text box to remove the last. They go with the text as one message, text first, as rho's terminal UI sends them, and the transcript shows them above the message's text.

What goes depends on the file. A PNG, JPEG, GIF or WebP image goes as an image the model sees. A text file of up to 200,000 characters (what the terminal UI sends of a document it attaches) goes whole. Any other file, such as a PDF or a binary, or a longer text file, goes as its path, and the agent reads it with its own tools; its card says "Sent by path", or for a file past a limit, which and by how much, such as "By path: 250000 characters, over the 200000 sent whole". A file over what one message can carry goes by its path unread, and an image whose data doesn't read goes by its path too, saying so. Attachments in one message can come to 16 MiB once encoded, images counting a third more as base64: the daemon reads a message as one line of at most 16 MiB (`rho_protocol::MAX_LINE_BYTES`). A steer carries only text, so a message with attachments sent while a turn runs waits above the composer and goes as the next prompt once the turn ends, and so does any message sent after it.

:::

### Drafts

Leave a new session with something typed in it and it waits at the top of the sidebar as a **Draft**. Click it to pick up where you left off. Drafts last for the current run of the app.

::: details In detail

Leave a new session's draft with something in its composer (text or an attachment) and it waits at the top of the sidebar, above pinned sessions, labelled Draft and titled with the message's first line. The one you left most recently is on top. Click it to pick up where you left off: the message, project and settings are as you left them, because the draft keeps its session open in the daemon. Hover it, or right-click it, to discard it. A draft left empty goes, as before. New session on a draft that holds a message leaves that draft in the sidebar and starts a fresh one in the same project. Drafts follow the search and the filter, and last only for this run of the app. A draft survives the daemon reconnecting with its message and project, but its session is opened again, so its settings start over. One whose machine is removed from the settings goes with it.

:::

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

::: details In detail

Right-click a session for its menu: Rename…, Pin or Unpin, Snooze (1 hour, 4 hours or 1 day), Settle or Restore, Copy (the session ID, the `rho -R` command that resumes it, or its folder's path), and Delete…, which a busy session can't do. Rename turns the title into a text box: Enter, or clicking away, saves it in rho (`rho/session/rename`), and Escape leaves it as it was. rho keeps a title you give over one it generates, and refuses a blank one. Pinned sessions lead the list, the latest pinned first, with a pin beside their age; hover one for Unpin. A pinned session can't be settled or snoozed until unpinned.

Pinning, settling and snoozing sessions is rho-ui's own idea; rho has no such state. Sessions started or continued in rho-ui list as active. Sessions from elsewhere, such as rho's terminal UI, start out settled, so the sidebar doesn't fill with old history; a session with work under way always lists as active, and pinning, snoozing, restoring, prompting, or steering one makes it rho-ui's. rho-ui keeps this in its [state database](../architecture#state). A settled session comes back when it has new activity in rho-ui, and a snoozed one when its time is up. Snoozed and Settled sit under the active sessions, folded; open one to see its sessions on one line each, and hover a session for its details. An opened shelf shows its newest 25 sessions; Show more under them adds 25 more each time, and folding the shelf goes back to 25.

The button at the top left of the sidebar, or Ctrl+B, collapses it to a narrow rail and opens it again. The rail keeps that button and Settings where they were and puts New session under it. While any session waits for an approval or runs a turn, the rail shows its status icon, so collapsing hides no session that needs you: hover the icon to see how many, and click it to open the sidebar. rho-ui remembers whether the sidebar is collapsed.

Drag the open sidebar's right edge to resize it, no narrower than 14 rem (224px at the default scale) and as far as leaves the session 30 rem beside the right panel; double-click the edge for its default width, 18 rem. rho-ui remembers its width.

:::

## Extra directories

A session's tools reach its folder. To give them another one, use the folder button in the title bar: type a path and press <kbd>Enter</kbd>. rho saves it with the session and tells the model from its next turn on.

::: details In detail

A session's title bar has a folder button that lists the directories its tools reach besides its workspace, with their count once there are any. Type a path in its box, absolute or under `~/` by the rules of the session's machine, and Enter or Add adds it: rho saves it with the session and tells the model from its next turn on. A path rho refuses, such as one that isn't a directory, says why under the box and stays there to fix. Added while a turn runs, it waits for the turn to end, as a settings change does. A subagent's session only lists its directories.

:::

## Opening and letting go

rho-ui follows a session while it is on screen and while it has anything under way: a turn, a request for you, background work, or something typed in its composer. Once a session you left has none of these, rho-ui lets it go so the daemon can unload it. Its row stays. Opening it again replays it from the daemon, which takes a fraction of a second even for the largest sessions.

::: details In detail

rho-ui follows a session while it is on screen, and while it has something under way: a turn, a request waiting for you, background work or its result not yet given to the model, a message being sent, or something typed in its composer, its directory box or its sign-in key box. Once a session you left has none of these, rho-ui lets it go, so the daemon can unload it; its row stays, Done and all, and follows the daemon's jobs. Opening it again replays it from the daemon, which for the largest sessions takes a fraction of a second; where it was scrolled to isn't kept. A saved session shows "Loading session…" until its replay has all arrived, then opens at its end, drawn once rather than growing as the history arrives; its composer sends from then on. A subagent's session goes with the session above it.

:::
