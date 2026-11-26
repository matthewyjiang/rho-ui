# Sessions

Sessions belong to the rho daemon, not to rho-ui. The sidebar lists every saved session on every machine you've added, whichever client started it, and shows when one is working or waiting on you, even if rho's terminal UI started the turn.

## Pinned, snoozed and settled

Each machine's rho daemon keeps whether a session is pinned, snoozed or settled, so every rho-ui connected to it, on any computer, shows the same, and a change made in one shows in the others. Another rho-ui shows the change at once when the session is on its screen or working there; otherwise when it next reads the machine's sessions, as it does when it connects and when a session's work starts or ends. rho settles a session itself when the last of the pull requests linked to it merges, whether its agent watched it or not, unless you pinned it, it is still snoozed, or you restored it since the merge. A session with another linked pull request that hasn't merged stays where it is. A rho too old to keep marks shows its sessions by age alone, and can't pin, snooze or settle them.

A session you haven't marked is active for a week after its last activity, whichever client started it, and settled from then on, so the sidebar doesn't fill with old history. That goes by the daemon's record of the session, so every rho-ui shows the same. A session with work under way always shows as active, and one you settled comes back when it has new activity. Restoring or unpinning a session, or a snooze that ends, keeps it active for a week from then.

The active list reads as an inbox. Under the pinned sessions, which stay on top whatever they are doing, come the sessions that need you, then the Done ones, then the idle ones, and last those working, waiting on their own background work, or watching a pull request. A session moves as its state changes: it sinks when you send it a prompt and rises when its turn ends.

A session whose agent watches a pull request isn't Done: rho wakes it when the pull request changes, so on the active list it reads **Watching** until the watch ends. Watching doesn't keep a session active, though; you can still snooze or settle one.

A session stays Done until you open it, even across restarts of rho-ui. A turn that finishes while rho-ui is closed doesn't mark its session Done, since rho-ui didn't see it end.

## Sessions a session starts

A session can start other sessions, as rho's `sessions` tool does. The sidebar folds those under the session that started them, one level deep: what a started session starts folds under the same top session. The fold opens by itself when you open one of them, and stays as you leave it until rho-ui quits.

A family moves as one. It sits on its starter's shelf, and is active while any session in it is busy. Pin, snooze and settle the starter; the sessions under it have no shelf actions of their own, and the starter offers none, nor Delete, while any session in the family is busy.

The project and machine filter applies to each session on its own. A starter outside the filter, or one a search doesn't match, still shows, with its fold held open, when sessions under it do.

A session whose starter isn't listed, because it was deleted or is a fork, shows on its own. A machine whose rho is older than rho-ui lists the sessions it started unfolded, since its daemon doesn't say which session started which.

## Leads

A lead is a session that runs as rho's `lead` agent: it takes a goal to merged pull requests by starting one session per pull request, answering their questions when its brief settles them, and watching the pull requests. It writes no code itself, and it asks you before it merges unless its brief says it may. It needs a rho that has the `lead` agent.

- **A folder of its own.** A lead works in `~/.rho/leads/<name>/` on its machine (under `$RHO_HOME` if you set one), named from its first message, with `-2`, `-3`… when that name is taken.
- **The brief is yours too.** The lead keeps what it knows between turns in `brief.md` in that folder: the goal, where it may work, what it may decide without you, its pull requests and its decisions. Edit it at any time; the lead reads it at the start of every turn.
- **In no project.** A lead's folder belongs to no project, so the project filter doesn't hide it; the machine filter and search still do.
- **Its projects are where it may work.** The projects you pick for a lead are the directories its tools reach, and rho lists them to it. Pick them in its draft: the lead gets them as its first message starts it, so its first turn knows them. After that, add more from the lead's title bar, as any session's directories.
- **Its sessions fold under it**, as any session's do (see above). The lead reads their results itself, so a session of its that finishes stays off the lead's folded line and doesn't move the lead up the list; one that waits on you still does both.
- **Work and Brief in its panel.** Brief shows `brief.md` as it was at the end of the lead's last turn, or when you last came back to the lead, and can't be edited there. Work lists the sessions the lead started itself, not the ones those started.

## Projects

Every folder in a machine's clone folder (`$RHO_HOME/projects` unless you set one in **Settings → Machines**) is a project, found again each time you start a new session. Since the folder puts it on the list, removing it from the list does nothing: move or delete the folder instead. Hidden folders are skipped.

Add the folders you work in elsewhere yourself. Type a path in the project search to browse, or paste a git URL to clone it into the machine's clone folder.

A project is a repository rather than one checkout of it. Adding a linked worktree or a subfolder adds the repository it belongs to. With several machines, checkouts of the same repository count as one project, matched by repository name, so a project missing from one machine offers to clone it there.

For a repository with an origin remote on a web host, the button beside the project's name in the title bar opens the repository's page in your browser. rho-ui leaves out any credentials in the remote's URL, and drops an ssh port.

## Worktrees

In a git project you can start a session in a **New worktree** instead of the project's checkout, so parallel sessions in one repository never write the same files.

- Nothing is created until the first message goes, so trying the option leaves nothing behind.
- The daemon adds the worktree on a new `rho/…` branch from `HEAD`.
- The daemon removes it once it is clean and merged, keeping the branch. **Settings → Worktrees** and `rho worktrees prune` remove the ones that can go now.

## Commands, skills and agents

Type `/` at the start of a message or `$` anywhere to complete the session's commands and skills: rho's own commands, prompt templates, MCP prompts, workflows and agents. Commands that rho's terminal UI runs itself, such as `/model`, aren't offered: use the composer's footer instead. `/side` and `/btw` still work: they start a [side chat](./panel#side-chat).

To run a new session as one of the workspace's agents, start its first message with `/agent:<name>` and the task.

## Attachments

What the model gets depends on the file:

| File | Sent as |
| --- | --- |
| PNG, JPEG, GIF or WebP image | An image the model sees |
| Text file up to 200,000 characters | Its whole text |
| Anything else: a PDF, a binary, a longer text file | Its path, for the agent to read with its own tools |

One message carries at most 16 MiB of attachments, as the daemon reads a message as one line of up to 16 MiB. A message with attachments can't steer a running turn, so it waits and goes as the next prompt. A session on a remote machine can't take files that would go by path, since the path is on this machine.

## Things to know

- **Settings changes wait for the turn.** A model or permission change made while a turn runs applies when it ends.
- **Sessions a session starts have their own mode and model.** The sessions a session starts through rho's `sessions` tool open in the permission mode and on the model you choose for them in its settings panel, apart from its own, or rho's config's if you choose none. A choice made while a turn runs applies to the sessions started after that turn ends: the ones the turn starts meanwhile open with the choice as it was. Each started session passes the choice on to the sessions it starts, and an agent that names its own model keeps that model. The choice is saved with the session.
- **Drafts last for this run.** A draft you leave with something typed waits in the sidebar until rho-ui quits.
- **Sessions you leave are let go.** Once a session you left has nothing under way, rho-ui stops following it, so the daemon can unload it and rho's terminal UI can open it. Switching back shows it as you left it, scroll position and typed text included, while rho-ui catches up with the daemon; you can send once it has. A `ui` panel above where you were is rebuilt, which can move the scroll.
- **Extra directories are saved with the session.** Directories added from the title bar's folder button, a lead's projects among them, reach the model from its next turn on.
