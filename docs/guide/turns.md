# Working with a turn

A turn is one round of the agent working on your message: model replies, tool calls, and the answer at the end.

## While it runs

The header under your message counts up ("Working for 38s"), and says what holds the turn up when the daemon reports it, such as "compacting the context" or "rate limited, retrying in 12s".

Each tool call shows as one line naming the call, like `read_file(src/main.rs)` or the command it ran, with short facts after it: `32 lines`, `13 matches in 2 files`, `+4 −1`. Click it to open what it found: a search's matches, a command's output, an edit's diff. When the turn ends, its tool calls fold behind one "Worked for" line, so the transcript reads as messages and answers.

![A finished turn: its tool calls folded under one row, then the reply](/shots/work-steps.webp)

When the agent keeps a plan, **Todo 2 of 5** shows above the composer. Click it to see the steps.

::: details In detail

While a turn runs, its header under the prompt counts up ("Working for 38s"), and says what holds the turn up when the daemon reports it: "compacting the context" while rho compacts it, or why the provider is being asked again and when, as in "rate limited, retrying in 12s". If the provider throws away a reply it was streaming, what had arrived of it leaves the transcript, thoughts and all, and the retry's reply takes its place.

Each tool call shows the card rho's terminal UI draws for it, which the daemon sends with the call: one line naming the call, such as `read_file(src/main.rs)` or the command it ran, with short facts after it (`32 lines`, `13 matches in 2 files`, `+4 −1`, a nonzero exit). It opens into what the card shows: a search's matches, a command's output, a checklist, an error, an edit's diff. What the call returned to the model and no person needs, such as a whole file it read or an edit's receipt, stays hidden. A diff names its files from the session's workspace, and a diff of one file leaves out its header, as the call's line names the file. While a turn runs, its folded row of steps shows the step running now; opened, the row sums itself up instead, as that step is listed right below.

When the agent keeps a plan, "Todo 2 of 5" shows above the composer. It starts folded; a click opens its steps: done steps struck through, the step under way in bold. It scrolls past about seven steps, and goes once every step is done.

:::

### Steering

Press <kbd>Enter</kbd> while a turn runs and your message steers it: the text joins the turn's next model request. Until the model reads it, it waits above the composer, where you can take it back. A message the turn can't take yet waits there too and goes as the next prompt.

To stop the turn, use the stop button in the composer.

::: details In detail

While a turn runs, Enter in the composer steers it: the text joins the turn's next model request. Until the model reads it, it waits above the composer, where you can take it back. Once read, it shows in the transcript as a turn of its own, as it will when the session replays (rho's replay doesn't mark steers): the work before it folds behind "Worked for", and the header under the steer counts up from there, while the sidebar counts the whole job. rho's attach doesn't say when a steer applied either, so a session reopened mid-turn counts the steer's header from the turn's start. A message the turn can't take (it waits on an answer, or the daemon had just begun a turn of its own) waits above the composer too, and goes as the next prompt once the turn ends.

:::

## When it needs you

Requests show in a card above the composer, the oldest first. The card takes the keyboard when the composer is empty, so <kbd>Enter</kbd> answers.

- **Approvals**: **Allow once**, **Allow for this session**, or **Deny**, with an optional reason the tool reads.
- **Plan approvals**: the plan, with a permission mode to carry it out in, or **Keep planning**, and a box for feedback.
- **Questions** the agent asks, one at a time: pick a choice or several, type your own answer where the question takes one, skip an optional one, or decline them all.

![An approval card above the composer](/shots/turn-approval.webp)

If rho's `[questionnaire] timeout_seconds` is set, a question can count down to a fallback answer. Your first key, paste or click in the card stops the countdown, and the question waits for you.

::: details In detail

When a turn needs you, its request shows in a card above the composer, the oldest first, with a count of any behind it; it takes the keyboard if the composer is empty, so Enter answers. An approval offers Allow once, Allow for this session, and Deny, beside a box for a reason to deny: the tool reads it, and Enter there denies. A plan approval shows the plan and offers each permission mode to carry it out in, and Keep planning, beside a box for feedback that the model reads with whichever you pick. Questions the agent asks, or a subagent asks through its parent, are a questionnaire, one question at a time: pick one choice or several, type an answer of your own where the question takes one, skip an optional question, and submit, or decline them all. A choice the agent suggests is picked already, or marked "suggested" where it only points at it. An answer that doesn't reach the daemon leaves the card as you filled it in. If rho's `[questionnaire] timeout_seconds` is set, a question can come with a fallback: the card counts down to when rho answers it with the agent's fallback answers, and says what they are and why, and the count of requests behind the card counts down to the soonest of theirs. Your first key, paste or click in the card stops its countdown (`rho/session/hold`), and the question waits for you. A question rho answered at its deadline leaves a note in the transcript saying what it answered. The countdown reads the deadline by this machine's clock, so a remote machine's clock that is off shifts it. rho-ui opens its sessions with the questionnaire tool, so their agents can ask.

:::

## Replies

Replies are Markdown. Code blocks have a copy button. ` ```mermaid ` blocks draw as diagrams and `$$ … $$` as formulas. An image the reply links by path, as in `![The loss curve](out/loss.png)`, is drawn in place, read from the session's machine. Click any image to open it full size and zoom.

Drag to select text across messages and copy it with <kbd>Ctrl</kbd>+<kbd>C</kbd>. When you let go, **Add to prompt** quotes the selection above the composer, for your next message to refer to.

::: details In detail

A code block in a message or a notice names its language in its top corner, beside a button that copies its code in one click; the button shows a check for 2 seconds after.

Drag to select a message's text, across messages too, and copy it with Ctrl+C. A long reply is drawn in stretches of about 2,000 characters, which shows in two places: Select All (Ctrl+A) in it selects the stretch you clicked in, not the whole reply, and a selection is dropped once the stretch it started in is scrolled well off screen.

Letting go of a drag that selected text in the transcript, or a double click on a word, shows an Add to prompt button just above the pointer. It quotes the selection above the composer's text box, not in it, and puts the keyboard in the box. Quote several passages and each shows, muted, up to three lines, with a button to remove it. They go before the text as Markdown block quotes, the first added first (after a slash command, which rho runs only from a prompt's start), so the transcript shows the sent message with them quoted. The button goes with a click elsewhere or once you scroll the transcript. A send that fails gives the message back as text, quotes and all, in the text box.

Messages, and a PR's description and posts in the Pull request surface, draw a ` ```mermaid ` block as a diagram and `$$ … $$` or `$ … $` as a formula, in the theme's colors and font, redrawn when the theme or text size changes. Diagrams come from [pele](https://github.com/obsidianmd/pele), Obsidian's Mermaid renderer, run in an embedded QuickJS; formulas from [RaTeX](https://github.com/erweixin/RaTeX). A diagram wider than its column scales down to fit. A diagram shows as its code until its closing fence arrives, so a streaming reply doesn't redraw it with every chunk. As anyone who can comment on a PR writes its text, a PR's diagrams are drawn in the background, on a pele of their own, only while its tab shows; each shows as code until it is drawn, and of a PR's different diagrams, the first 20 are drawn, its description's first, and the rest show as code (rho-ui says so on stderr). What can't be drawn stays as written: a diagram pele can't read shows as code, and so does one that takes over 1 s or 64 MiB to draw (rho-ui says so on stderr); a formula RaTeX can't lay out shows as its source, and dollar signs read as prices ("$5 and $10") stay prose.

A paragraph of only images by path, such as `![The loss curve](out/loss.png)`, draws each image at its own size, scaled down to the transcript's width and to 24 rem tall. A relative path is from the session's folder, and the file is read on the session's machine, so a remote session's images come over its ssh connection. Each message reads its images once, so a file rewritten later still shows in an earlier reply as it was then. Images are read one at a time, as each read on a remote machine is a session on its shared ssh connection. An image that can't be shown says so in its place, with its path and why: it isn't a PNG, JPEG, GIF or WebP image, it is over 32 MiB or 4096 px a side (what rho's own `read_file` previews), or it is a web link, which rho-ui doesn't fetch. One that couldn't be read, because the file isn't there yet or the machine didn't answer, is read again when the turn ends, or when clicked. `data:` images are drawn by GPUI Kit's Markdown, as is an image inside a line of text, which Kit shows only from a `data:` URL. Images a tool returns, such as `read_file`'s preview of an image, show as tiles in the call's details.

Click any image, whether in a reply, a tool call or an attachment, to open it at full size in a window over the transcript, with its alt text (or the tool call) under it. It opens fitted to the window, never larger than its own pixels. The bar above it zooms in and out, fits it again, or shows it at 1:1, up to 16 screen pixels to one of the image's; the mouse wheel, a pinch, the platform's zoom modifier with a trackpad scroll, and the keys `+`, `-`, `0` (fit) and `1` (full size) zoom too, around the pointer where there is one, and a double click switches between the fit and full size. Zoomed in, drag the image or scroll a trackpad to move around it. Escape, the close button or a click outside closes it.

:::

## Forms from the agent

rho's `ui` tool can show you a form or a small panel, drawn right in the transcript: text fields, choices, date pickers, sliders, buttons. Your edits are checked as you make them, and a button sends what you entered back to the agent.

![A form the agent showed, with each kind of control](/shots/surface-gallery-inputs-dark.webp)

::: details In detail

rho's `ui` tool shows you a form or a small panel, an [A2UI](https://a2ui.org) surface, and can wait for you to act on it. rho-ui draws the surface in the transcript, under the run of tool calls that holds its `ui` call. It stays there and stays usable after the call returns and after the turn's working folds. A later call that updates the surface moves it under that call, and a surface the agent deletes leaves the transcript. A session opened again draws its surfaces the same from the daemon's replay; one that fell behind keeps them as they were, the tab you picked included.

Edits go to the surface's data model as you make them, so its checks update at once: a check that fails shows its message under the control, and a button whose own check fails is disabled, with the reason as its tooltip. Pressing a button whose action is an event sends it to the session, with what you entered, and returns the keyboard to the composer. A waiting `ui` call returns the action; otherwise the model reads it at its turn's next boundary, or in a turn rho starts for it. A button that opens a link opens it in your browser. If the session no longer has the surface, as it may not after its daemon was updated, the surface says so under itself, with what rho said, and takes no more actions. A surface too large to draw, past 2,000 components, stops where it is cut off and says so. If rho-ui can't apply part of a surface, because its A2UI is newer, the surface says that too.

Each component of the A2UI basic catalog has a form:

| Component | Drawn as |
| --- | --- |
| `Text` | Markdown, as a reply's text is; a caption is small and muted |
| `Row`, `Column`, `List` | Layout, placed as `justify` and `align` say. A row wraps when its children don't fit, and its inputs share its width. A row that says nothing lines its children up along its bottom edge, so a button sits level with the field beside it |
| `Card` | A bordered group, at most 36 rem wide |
| `Button` | A button: filled for `primary`, outlined by default, and a link for `borderless` |
| `CheckBox` | A checkbox |
| `TextField` | A text box under its label: one line, several lines that grow with the text, a number box with steppers, or dots with a button to show them. Text that is not a number stays out of a number field's data, and the form's buttons that send wait until it is fixed |
| `DateTimeInput` | A date picker, with the time of day beside the calendar when the field takes one, or a time field alone. Dates are kept as `2026-01-31`, times as `09:30`, both as `2026-01-31T09:30` |
| `ChoicePicker` | Radio buttons for one choice, checkboxes for several, or a row of toggle chips. The `filterable` option is ignored |
| `Slider` | A slider with its value beside it |
| `Tabs` | A tab bar over the tab that shows |
| `Modal` | A button that opens the content in a dialog |
| `Icon` | The matching icon; a dashed square for a name without one |
| `Divider` | A rule, across or upright |
| `Image` | The picture, at the size and fit it asks for, when its URL is a `data:` URL; an avatar is round. rho-ui loads no other URL, which shows as a chip, as a video's does |
| `Video`, `AudioPlayer` | A chip with the description that opens an `http` or `https` link in your browser |

The controls are the app's own, so the keyboard reaches them as it reaches the rest of the app.

:::

## Background work and subagents

Processes, workflow runs and subagents a turn starts are listed above the composer while they run, each with a stop button. The turn can end while they run on. When one finishes, the daemon starts a turn of its own to hand the model the result, and you see that turn like any other. The title bar's counter opens what has ended, with how each ended.

Click a subagent to open its own session, nested under the one that started it, read-only. The title bar's path takes you back.

::: details In detail

Background work a session's turns started (processes, workflow runs, subagents) is listed above the composer while it runs, with a button to stop each one. A turn ends while its background work runs on: rho-ui opens sessions with `idle_wake: turn`, so when the work finishes the daemon starts a turn of its own that gives the model the result, and rho-ui shows that turn like any other. Work leaves the list as it ends. A button in the title bar, counting what ended, opens the session's ended work, newest first, with how each piece ended (its exit status, failed, or stopped); it keeps everything rho-ui saw end, after the model has its result and across restarts. The daemon forgets work once the model has its result, so rho-ui records the work it sees start and end. Work it saw running that ended while it was closed shows as ended, with no word on how; work that started and ended while it was closed doesn't show. The daemon's job list, which rho-ui reads once a second, adds what the session's own list lacks: a queued subagent or workflow run says where it waits in line, a failed one says why under its row, and a run that ended while rho-ui was closed says how, from the daemon's record of it.

Click a subagent's row, in either list, to open the subagent's own session, nested under the session that started it: the title bar reads `project / session / subagent's task`, and a click on any step above the last goes back to it. rho-ui observes the session (`rho/session/observe`) rather than attaching to it, as only the subagent writes there, so the view shows its messages and tool calls as they come, with no composer and no approvals (a subagent's questions go to its parent). A subagent of a subagent nests one step deeper. When the run ends, the parent shows the subagent's whole result as a card, its Markdown rendered and cut to its first 12 lines; Show more under it shows the rest, and Show less folds it again. Any card rho sends, such as a finished command's, shows the same way. Once the run ends, the daemon stops sending its events and the view keeps what it showed; opening the row again reads the session afresh. The sidebar lists only the session at the top, and marks it while one of its subagents is on screen.

:::
