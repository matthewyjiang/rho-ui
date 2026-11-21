# Working with a turn

A turn is one round of the agent working on your message: model replies, tool calls, and the answer at the end.

## While it runs

The header under your message counts up ("Working for 38s"), and says what holds the turn up when the daemon reports it, such as "compacting the context" or "rate limited, retrying in 12s".

Each tool call shows as one line naming the call, like `read_file(src/main.rs)` or the command it ran, with short facts after it: `32 lines`, `13 matches in 2 files`, `+4 −1`. Click it to open what it found: a search's matches, a command's output, an edit's diff. When the turn ends, its tool calls fold behind one "Worked for" line, so the transcript reads as messages and answers.

![A finished turn: its tool calls folded under one row, then the reply](/shots/work-steps.webp)

When the agent keeps a plan, **Todo 2 of 5** shows above the composer. Click it to see the steps.

### Steering

Press <kbd>Enter</kbd> while a turn runs and your message steers it: the text joins the turn's next model request. Until the model reads it, it waits above the composer, where you can take it back. A message the turn can't take yet waits there too and goes as the next prompt.

To stop the turn, use the stop button in the composer.

## When it needs you

Requests show in a card above the composer, the oldest first. The card takes the keyboard when the composer is empty, so <kbd>Enter</kbd> answers.

- **Approvals**: **Allow once**, **Allow for this session**, or **Deny**, with an optional reason the tool reads.
- **Plan approvals**: the plan, with a permission mode to carry it out in, or **Keep planning**, and a box for feedback.
- **Questions** the agent asks, one at a time: pick a choice or several, type your own answer where the question takes one, skip an optional one, or decline them all.

![An approval card above the composer](/shots/turn-approval.webp)

If rho's `[questionnaire] timeout_seconds` is set, a question can count down to a fallback answer. Your first key, paste or click in the card stops the countdown, and the question waits for you.

## Replies

Replies are Markdown. Code blocks have a copy button. ` ```mermaid ` blocks draw as diagrams and `$$ … $$` as formulas. An image the reply links by path, as in `![The loss curve](out/loss.png)`, is drawn in place, read from the session's machine. Click any image to open it full size and zoom.

Drag to select text across messages and copy it with <kbd>Ctrl</kbd>+<kbd>C</kbd>. When you let go, **Add to prompt** quotes the selection above the composer, for your next message to refer to.

## Forms from the agent

rho's `ui` tool can show you a form or a small panel, drawn right in the transcript: text fields, choices, date pickers, sliders, buttons. Your edits are checked as you make them, and a button sends what you entered back to the agent.

![A form the agent showed, with each kind of control](/shots/surface-gallery-inputs-dark.webp)

[Behavior in detail](../features#surfaces) lists how each A2UI component is drawn.

## Background work and subagents

Processes, workflow runs and subagents a turn starts are listed above the composer while they run, each with a stop button. The turn can end while they run on. When one finishes, the daemon starts a turn of its own to hand the model the result, and you see that turn like any other. The title bar's counter opens what has ended, with how each ended.

Click a subagent to open its own session, nested under the one that started it, read-only. The title bar's path takes you back.
