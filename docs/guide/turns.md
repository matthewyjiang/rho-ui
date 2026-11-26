# Working with a turn

## Steering

Pressing <kbd>Enter</kbd> while a turn runs doesn't queue your message: it steers the turn, joining the turn's next model request. Until the model reads it, it waits above the composer and you can take it back. A message the turn can't take, such as one with attachments, or one sent while the turn waits on you, goes as the next prompt once the turn ends.

## Questions with a deadline

If rho's `[questionnaire] timeout_seconds` is set, the agent's questions count down to a fallback answer the agent chose. Your first key, paste or click in the question stops the countdown, and the question waits for you. The countdown uses this machine's clock, so a remote machine whose clock is off shifts it.

## Background work

Processes, workflow runs and subagents a turn starts can outlive the turn. When one finishes, the daemon starts a turn of its own to give the model the result. rho-ui keeps a history of the work that ended, across restarts. Work that started and ended while rho-ui was closed isn't in it.

A subagent's session opens read-only: only the subagent writes to it, and its questions come to the session that started it.

A process's output opens in the panel's Background tab.

- **It's read, not pushed.** rho-ui asks the daemon for new output every half second while the output is on screen, and for each process's time every second while one runs. It stops asking once nothing runs.
- **rho keeps only so much of it.** Past its output budget, the oldest output goes first, so a long-running process's output may start partway through.
- **A side chat's processes** show their time, but their output doesn't open: the side chat is already in the panel.

## Signing in

A session whose model isn't signed in still opens, with a card above the composer to sign in. A message sent before you do fails, saying why. rho doesn't report a sign-out made elsewhere until a turn fails because of it.

## Replies

- **Images by path**, as in `![The loss curve](out/loss.png)`, are drawn in place, read from the session's machine relative to its folder. rho-ui draws PNG, JPEG, GIF and WebP images up to 32 MiB and 4096 px a side. It doesn't fetch web links.
- **Diagrams and formulas**: ` ```mermaid ` blocks draw as diagrams and `$$ … $$` or `$ … $` as formulas. A diagram that takes over 1 s or 64 MiB to draw stays as code.
- **Long replies** are drawn in parts of about 2,000 characters, so <kbd>Ctrl</kbd>+<kbd>A</kbd> selects the part you clicked in, not the whole reply.
- **Add to prompt** appears when you select text in the transcript. It quotes the selection in your next message.

## Forms

rho's `ui` tool can show you a form or a small panel in the transcript. A button sends what you entered back to the agent, which reads it at its next step. rho-ui draws images in a form only from `data:` URLs; other links open in your browser.
