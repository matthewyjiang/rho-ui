# Sessions

Sessions belong to the rho daemon, not to rho-ui. The sidebar lists every saved session on every machine you've added, whichever client started it, and shows when one is working or waiting on you, even if rho's terminal UI started the turn.

## Pinned, snoozed and settled

Pinning, snoozing and settling are rho-ui's own; rho has no such state, so rho's terminal UI doesn't see them. Sessions that started outside rho-ui begin settled, so the sidebar doesn't fill with your old terminal history. A session with work under way always shows as active, and a settled one comes back when it has new activity.

The active list reads as an inbox. Under the pinned sessions, which stay on top whatever they are doing, come the sessions that need you, then the Done ones, then the idle ones, and last those working, waiting on their own background work, or watching a pull request. A session moves as its state changes: it sinks when you send it a prompt and rises when its turn ends.

A session whose agent watches a pull request isn't Done: rho wakes it when the pull request changes, so it reads **Watching** until the watch ends. Watching doesn't keep a session active, though; you can still snooze or settle one.

A session stays Done until you open it, even across restarts of rho-ui. A turn that finishes while rho-ui is closed doesn't mark its session Done, since rho-ui didn't see it end.

## Projects

rho-ui doesn't find projects on its own: add the folders you work in. Type a path in the project search to browse, or paste a git URL to clone it into the machine's clone folder (`$RHO_HOME/projects` unless you set one in **Settings → Machines**).

A project is a repository rather than one checkout of it. Adding a linked worktree or a subfolder adds the repository it belongs to. With several machines, checkouts of the same repository count as one project, matched by repository name, so a project missing from one machine offers to clone it there.

For a repository with an origin remote on a web host, the button beside the project's name in the title bar opens the repository's page in your browser. rho-ui leaves out any credentials in the remote's URL, and drops an ssh port.

## Worktrees

In a git project you can start a session in a **New worktree** instead of the project's checkout, so parallel sessions in one repository never write the same files.

- Nothing is created until the first message goes, so trying the option leaves nothing behind.
- The daemon adds the worktree on a new `rho/…` branch from `HEAD`.
- The daemon removes it once it is clean and merged, keeping the branch. **Settings → Worktrees** and `rho worktrees prune` remove the ones that can go now.

## Commands, skills and agents

Type `/` at the start of a message or `$` anywhere to complete the session's commands and skills: rho's own commands, prompt templates, MCP prompts, workflows and agents. Commands that rho's terminal UI runs itself, such as `/model`, aren't offered: use the composer's footer instead.

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
- **Drafts last for this run.** A draft you leave with something typed waits in the sidebar until rho-ui quits.
- **Sessions you leave are let go.** Once a session you left has nothing under way, rho-ui stops following it, so the daemon can unload it and rho's terminal UI can open it. Switching back shows it as you left it, scroll position and typed text included, while rho-ui catches up with the daemon; you can send once it has. A `ui` panel above where you were is rebuilt, which can move the scroll.
- **Extra directories are saved with the session.** Directories added from the title bar's folder button reach the model from its next turn on.
