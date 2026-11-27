# Sessions

Sessions belong to the rho daemon, not to rho-ui. The sidebar lists every saved session on every machine you've added, whichever client started it, and shows when one is working or waiting on you, even if rho's terminal UI started the turn.

## Pinned, snoozed and settled

Each machine's rho daemon keeps whether a session is pinned, snoozed or settled, so every rho-ui connected to it, on any computer, shows the same, and a change made in one shows in the others. Another rho-ui shows the change at once when the session is on its screen or working there; otherwise when it next reads the machine's sessions, as it does when it connects and when a session's work starts or ends. rho settles a session itself when the last of the pull requests linked to it merges, whether its agent watched it or not, unless you pinned it, it is still snoozed, or you restored it since the merge. A session with another linked pull request that hasn't merged stays where it is. A rho too old to keep marks shows its sessions by age alone, and can't pin, snooze or settle them.

A session you haven't marked is active for a week after its last activity, whichever client started it, and settled from then on, so the sidebar doesn't fill with old history. That goes by the daemon's record of the session, so every rho-ui shows the same. A session with work under way always shows as active, and one you settled comes back when it has new activity. Restoring or unpinning a session, or a snooze that ends, keeps it active for a week from then.

The active list reads as an inbox. Under the pinned sessions, which stay on top whatever they are doing, come the sessions that need you, then the Done ones, then the idle ones, and last those working, waiting on their own background work, or watching a pull request. A session moves as its state changes: it sinks when you send it a prompt and rises when its turn ends.

A session whose agent watches a pull request isn't Done: rho wakes it when the pull request changes, so on the active list it reads **Watching** until the watch ends. Watching doesn't keep a session active, though; you can still snooze or settle one.

A session stays Done until you open it, even across restarts of rho-ui. A turn that finishes while rho-ui is closed doesn't mark its session Done, since rho-ui didn't see it end. The session on screen turns Done too if its turn ends while rho-ui's window doesn't have the focus, until you come back to the window.

## Notifications

While rho-ui's window doesn't have the focus, a session that starts to need you (a question or an approval) or whose turn ends sends a desktop notification naming it. Sessions that do so within two seconds of each other share one notification. Clicking it opens the session; the ones that need you come first. Sessions on remote machines notify the same way. Nothing is sent while the window has the focus. **Settings → Notifications** turns them off, or keeps them to sessions that need you.

The window's title counts the sessions that need you or are Done, as `(2) Rho`, so a taskbar or window switcher shows it. On macOS the count also shows on the Dock icon, and the icon bounces once with each notification; on X11 the window is marked urgent until you focus it.

What the platform allows:

- **macOS** shows notifications only for the installed `Rho.app`, not a binary run from a terminal, and asks you to allow them the first time.
- **Linux** sends them to your desktop's notification server over D-Bus, on Wayland and X11 alike. Whether clicking one also raises the window is up to the compositor: on Wayland, GNOME and KDE may only mark rho-ui as wanting attention, but the session is open when you switch to it.

## Sessions a session starts

A session can start other sessions, as rho's `sessions` tool does. The sidebar folds those under the session that started them, one level deep: what a started session starts folds under the same top session. The fold opens by itself when you open one of them, and stays as you leave it until rho-ui quits.

A family moves as one. It sits on its starter's shelf, and is active while any session in it is busy. Pin, snooze and settle the starter; the sessions under it have no shelf actions of their own, and the starter offers none, nor Delete, while any session in the family is busy. While the family is active, a session in it that is settled or snoozed on its own, as one rho settles when its last linked pull request merges, leaves the fold until it has new activity or its snooze ends. Search still finds it, and while it is open, from search or its lead's **Work**, it shows in the fold. Moving the family leaves its mark alone, though one settled on its own reads as settled with a settled family, which Restore brings back whole. Pinning a snoozed family brings it back as Restore does. Pinning a settled family, or new activity in its starter, brings back only the starter, since rho-ui can't tell the sessions settled with the family from those rho settled when their pull requests merged.

The project and machine filter applies to each session on its own. A starter outside the filter, or one a search doesn't match, still shows, with its fold held open, when sessions under it do. The counts in the filter go by family: a session and the sessions under it count once, on any shelf.

A session whose starter isn't listed, because it was deleted or is a fork, shows on its own. A machine whose rho is older than rho-ui lists the sessions it started unfolded, since its daemon doesn't say which session started which.

## Leads

A lead is a session that runs as rho's `lead` agent: it takes a goal to merged pull requests by starting one session per pull request, answering their questions when its brief settles them, and watching the pull requests. It writes no code itself, and it asks you before it merges unless its brief says it may. It needs a rho that has the `lead` agent.

- **A folder of its own.** A lead works in `~/.rho/leads/<name>/` on its machine (under `$RHO_HOME` if you set one), named from its first message, with `-2`, `-3`… when that name is taken.
- **The brief is yours too.** The lead keeps what it knows between turns in `brief.md` in that folder: the goal, where it may work, what it may decide without you, its pull requests and its decisions. The lead updates it whenever something in it changes, and reads it when it starts and again after rho compacts its context, not every turn. So to change the brief, tell the lead: an edit you make by hand reaches it only at its next reread.
- **Named by its projects.** A lead's folder is in no project: the title bar and its sidebar row name the projects you picked for it instead, and the project filter shows it when any of them is chosen. A lead with no projects names none, and the project filter never hides it.
- **Its projects are where it may work.** The projects you pick for a lead are the directories its tools reach, and rho lists them to it. Pick them in its draft: the lead gets them as its first message starts it, so its first turn knows them. After that, add more from the lead's title bar, as any session's directories.
- **Its sessions fold under it**, as any session's do (see above). The lead reads their results itself, so a session of its that finishes stays off the lead's folded line and doesn't move the lead up the list; one that waits on you still does both.
- **Work and Brief in its panel.** Brief shows `brief.md` as it was at the end of the lead's last turn, or when you last came back to the lead, and can't be edited there. Work lists the sessions the lead started itself, not the ones those started.

## Projects

Every folder in a machine's clone folder (`$RHO_HOME/projects` unless you set one in **Settings → Machines**) is a project, found again each time you start a new session. Since the folder puts it on the list, removing it from the list does nothing: move or delete the folder instead. Hidden folders are skipped.

Add the folders you work in elsewhere yourself. Type a path in the project search to browse, or paste a git URL to clone it into the machine's clone folder.

A project is a repository rather than one checkout of it. Adding a linked worktree or a subfolder adds the repository it belongs to. With several machines, checkouts of the same repository count as one project, matched by repository name, so a project missing from one machine offers to clone it there.

For a repository with an origin remote on a web host, the button beside the project's name in the title bar opens the repository's page in your browser. rho-ui leaves out any credentials in the remote's URL, and drops an ssh port.

### No project

For a quick question or throwaway work, pick **No project** instead. The session gets a folder of its own, `~/.rho/scratch/<name>/` on its machine (under `$RHO_HOME` if you set one), named from its first message like a lead's, and works there in place, never in a worktree. Like a lead's folder, it is in no project: nothing names it as one, the project filter never hides the session, and a new session started from it opens in a project. No project is never the default, and rho-ui doesn't remove these folders.

### Sessions in several projects

A session names every project it works in, in its title bar and on its sidebar row, with their badges stacked: a lead the projects you picked for it, and a plain session its own project, then those of its added directories that are among your projects (other directories don't count). The project filter shows such a session when any of its projects is chosen, and the search finds it by any of their names. A subagent's session shows the projects of the session it runs under.

## Worktrees

In a git project you can start a session in a **New worktree** instead of the project's checkout, so parallel sessions in one repository never write the same files.

- Nothing is created until the first message goes, so trying the option leaves nothing behind.
- The daemon adds the worktree on a new `rho/…` branch from `HEAD`.
- The daemon removes it once it is clean and merged, keeping the branch. **Settings → Worktrees** and `rho worktrees prune` remove the ones that can go now.

## Commands, skills and agents

Type `/` at the start of a message or `$` anywhere to complete the session's commands and skills: rho's own commands, prompt templates, MCP prompts, workflows and agents. Commands that rho's terminal UI runs itself, such as `/model`, aren't offered: use the composer's footer instead. `/side` and `/btw` still work: they start a [side chat](./panel#side-chat).

To run a new session as one of the workspace's agents, start its first message with `/agent:<name>` and the task.

An MCP prompt runs as `/mcp:<server>:<name>`, followed by its arguments: everything after the command for a prompt of one argument, else `name=value` words. When its server suggests values for an argument, the menu offers them as you type it. <kbd>Tab</kbd> takes a suggestion, and <kbd>Enter</kbd> takes one only once you've moved the highlight, so a value you typed yourself sends as it is.

## Attachments

What the model gets depends on the file:

| File | Sent as |
| --- | --- |
| PNG, JPEG, GIF or WebP image | An image the model sees |
| Text file up to 200,000 characters | Its whole text |
| Anything else: a PDF, a binary, a longer text file | Its path, for the agent to read with its own tools |

Type `@` to attach a resource one of the session's MCP servers offers, such as a document or a database row. The menu matches what you type against each resource's URI and name. rho-ui reads the resource when you pick it, and the message can't be sent until the read is done. The model gets the resource's text under its URI, or a lone image as an image. Text beyond 200 KiB is cut off and says so, and binary parts are described rather than sent. A resource template, such as `docs://users/{id}`, is written into the message as text for you to fill in. Resources are offered only in an open session, not in a draft.

One message carries at most 16 MiB of attachments, as the daemon reads a message as one line of up to 16 MiB. A message with attachments can't steer a running turn, so it waits and goes as the next prompt. A session on a remote machine can't take files that would go by path, since the path is on this machine.

## Things to know

- **Settings changes wait for the turn.** A model or permission change made while a turn runs applies when it ends.
- **Sessions a session starts have their own mode and model.** The sessions a session starts through rho's `sessions` tool open in the permission mode and on the model you choose for them in its settings panel, apart from its own, or rho's config's if you choose none. A choice made while a turn runs applies to the sessions started after that turn ends: the ones the turn starts meanwhile open with the choice as it was. Each started session passes the choice on to the sessions it starts, and an agent that names its own model keeps that model. The choice is saved with the session.
- **Drafts last for this run.** A draft you leave with something typed waits in the sidebar until rho-ui quits.
- **Sessions you leave are let go.** Once a session you left has nothing under way, rho-ui stops following it, so the daemon can unload it and rho's terminal UI can open it. Switching back shows it as you left it, scroll position and typed text included, while rho-ui catches up with the daemon; you can send once it has. A `ui` panel above where you were is rebuilt, which can move the scroll.
- **A failing MCP server shows by the context ring.** When one of the session's MCP servers fails to start, times out, has an invalid config, or is still connecting, a plug mark shows next to the context ring, and its list gives each server's error. rho-ui checks again while servers start and after each turn.
- **Extra directories are saved with the session.** Directories added from the title bar's folder button, a lead's projects among them, reach the model from its next turn on.
