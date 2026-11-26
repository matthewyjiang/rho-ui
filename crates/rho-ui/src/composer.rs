//! The prompt box under the transcript. Enter sends, Shift+Enter or
//! Alt+Enter starts a new line; while a turn runs Send becomes Stop, and
//! Enter steers the turn. Its footer holds the attach button and the session
//! settings on the left, then the context ring, the model, and the round Send
//! button.
//!
//! What the user attaches shows above the text and goes with it, after it,
//! as one message, as rho's terminal UI sends one: an image pasted, or any
//! file chosen with the attach button, dropped on the box, or pasted as a
//! copied file. Each has a remove control on hover; Backspace in the empty
//! text box removes the last.
//!
//! Text the user adds from the transcript (see [`ComposerState::add_quote`])
//! shows as quotes above the text, each with a remove control, and goes
//! before the text as Markdown block quotes.
//!
//! Typing a slash command at the start of the text, or a `$skill` anywhere
//! in it, opens a menu of the session's names that complete it, floating
//! above the box. Up and Down move the highlight, Tab completes, and Enter
//! completes unless the name is typed out already, or reads as an amount
//! such as `$20`: then it sends. Escape closes the menu.
//!
//! In an open session the same menu offers its MCP servers' resources for
//! an `@` word, and the values a server suggests for an argument of its
//! prompt's command ([`mcp`]). Enter takes a resource once something is
//! typed after the `@`, and a value only once the highlight moved. A
//! resource is read and attached, and a template, whose placeholders the
//! user fills in, is written out as text.

mod mcp;

pub(crate) use mcp::McpLink;

use std::{collections::HashSet, path::PathBuf, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    attachment::AttachmentGroup,
    button::{Button, ButtonRounded, ButtonVariants as _},
    h_flex,
    input::{Backspace, Enter, Escape, IndentInline, MoveDown, MoveUp, Textarea, TextareaState},
    list::ListItem,
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _, ThemeStyled as _,
};
use gpui_kit::{
    deferred, div, rems, Action, AnyElement, App, AppContext as _, ClipboardEntry, ClipboardItem,
    Context, Entity, ExternalPaths, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, RenderOnce, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    check_attachment_total, ArgumentValue, Attachment, CommandInfo, CommandKind, Completion,
    Mention, Prompt, SessionMcpResource,
};

use crate::{
    attachments::{self, Preview},
    leads::Kind,
    usage::UsageRing,
};

/// Grows from one line to this many before it scrolls: enough for a short
/// multi-paragraph prompt without pushing the transcript off a laptop screen.
const MAX_ROWS: usize = 8;

/// What the user is writing: the text, and what is attached to it. Its
/// owner subscribes to [`Self::input`] for Enter, and observes this entity,
/// which notifies when the attachments change.
pub(crate) struct ComposerState {
    input: Entity<TextareaState>,
    attachments: Vec<Item>,
    /// Passages quoted from the transcript, in the order added.
    quotes: Vec<Quote>,
    /// Names the next attachment or quote. Never reused, so a card keeps
    /// its identity as others come and go.
    next_id: u64,
    /// Why the last attachments were refused, one line each, until it is
    /// dismissed, or the next attachment or send.
    refusal: Option<SharedString>,
    /// MCP resources being read to attach, in the order picked.
    reads: Vec<mcp::Read>,
    /// The session's slash commands and skills, which the menu offers.
    commands: Vec<CommandInfo>,
    /// What the session's MCP servers offer, and the requests for it.
    mcp: mcp::Mcp,
    /// What is being typed at the cursor, as last seen.
    typing: Option<Typing>,
    /// What completes it, while the menu is open.
    menu: Option<Menu>,
    /// What Escape closed the menu on; typing on opens it again.
    dismissed: Option<Typing>,
    _subscription: Subscription,
    /// The label of the remote machine the message goes to, whose agent
    /// can't read a file that would go by its path here.
    remote: Option<SharedString>,
}

/// An attachment in the composer.
struct Item {
    id: u64,
    attachment: Attachment,
    preview: Preview,
}

/// A passage quoted from the transcript.
struct Quote {
    id: u64,
    text: String,
}

/// What is being typed at the cursor that the menu can complete.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Typing {
    /// A command or skill name.
    Name(Completion),
    /// An MCP resource.
    Mention(Mention),
    /// The value of an MCP prompt's argument.
    Value(ArgumentValue),
}

/// What the open menu offers for what is being typed.
enum Choices {
    Names(Completion, Vec<CommandInfo>),
    Resources(Mention, Vec<SessionMcpResource>),
    Values(ArgumentValue, Vec<String>),
}

impl Choices {
    /// The choices for `typing`; `None` when there are none.
    fn of(typing: Typing, commands: &[CommandInfo], mcp: &mcp::Mcp) -> Option<Self> {
        let choices = match typing {
            Typing::Name(completion) => {
                let matches = completion.matches(commands).cloned().collect();
                Self::Names(completion, matches)
            }
            Typing::Mention(mention) => {
                let matches = mention.matches(mcp.resources()).cloned().collect();
                Self::Resources(mention, matches)
            }
            Typing::Value(value) => {
                // A value typed out already completes nothing, and one a
                // server suggests twice shows once.
                let typed = &value.ask().value;
                let mut seen = HashSet::new();
                let matches = mcp.values(value.ask());
                let matches = matches
                    .filter(|&suggested| suggested != typed && seen.insert(suggested))
                    .cloned()
                    .collect();
                Self::Values(value, matches)
            }
        };
        (choices.len() > 0).then_some(choices)
    }

    /// Choice `ix`'s element ID, which names it across menus: a resource
    /// by its server too, as two may offer one URI.
    fn id(&self, ix: usize) -> String {
        match self {
            Self::Names(_, matches) => format!("completion-{}", matches[ix].name()),
            Self::Resources(_, matches) => {
                format!("completion-@{}:{}", matches[ix].server, matches[ix].uri)
            }
            Self::Values(_, matches) => format!("completion-value-{}", matches[ix]),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Names(_, matches) => matches.len(),
            Self::Resources(_, matches) => matches.len(),
            Self::Values(_, matches) => matches.len(),
        }
    }

    /// What the menu is, for screen readers.
    fn label(&self) -> &'static str {
        match self {
            Self::Names(completion, _) => match completion.kind() {
                CommandKind::Slash => "Commands",
                CommandKind::Skill => "Skills",
            },
            Self::Resources(..) => "MCP resources",
            Self::Values(..) => "Suggested values",
        }
    }
}

/// The open menu of what completes the word at the cursor.
struct Menu {
    choices: Choices,
    /// The highlighted match.
    selected: usize,
    /// The user moved the highlight, so Enter takes it whatever is typed.
    picked: bool,
    /// Keeps the highlighted match in view.
    scroll: ScrollHandle,
}

impl Menu {
    /// Whether Enter takes the highlighted choice rather than sending: a
    /// name typed in part that reads as one, a resource once something
    /// follows the `@`, and a value, which may be what the user means
    /// already, only once the highlight moved.
    fn enter_takes(&self) -> bool {
        match &self.choices {
            Choices::Names(completion, matches) => {
                !completion.is_typed_out(&matches[self.selected])
                    && (self.picked || completion.reads_as_name())
            }
            Choices::Resources(mention, _) => self.picked || !mention.typed().is_empty(),
            Choices::Values(..) => self.picked,
        }
    }
}

/// A key the open menu takes from the text box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuKey {
    Up,
    Down,
    Tab,
    Enter,
    Escape,
}

impl ComposerState {
    /// An empty composer for a message to this machine, or with `remote`,
    /// to the remote machine of that label; see [`Self::set_remote`].
    pub(crate) fn new(
        remote: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, MAX_ROWS)
                .submit_on_enter(true)
                .placeholder(Kind::Session.placeholder())
        });
        // The text box notifies when its text or cursor moves.
        let subscription = cx.observe(&input, |this, _, cx| this.update_menu(cx));
        Self {
            input,
            attachments: Vec::new(),
            quotes: Vec::new(),
            next_id: 0,
            refusal: None,
            reads: Vec::new(),
            commands: Vec::new(),
            mcp: mcp::Mcp::default(),
            typing: None,
            menu: None,
            dismissed: None,
            _subscription: subscription,
            remote,
        }
    }

    /// Offers `commands`, the session's slash commands and skills, to
    /// complete.
    pub(crate) fn set_commands(&mut self, commands: &[CommandInfo], cx: &mut Context<'_, Self>) {
        if self.commands != commands {
            self.commands = commands.to_vec();
            self.refresh_menu(cx);
        }
    }

    /// Builds the menu again for what is typed, as what it offers changed.
    /// The highlight stays on its choice while the menu still offers it.
    fn refresh_menu(&mut self, cx: &mut Context<'_, Self>) {
        let kept = (self.menu.as_ref()).map(|menu| (menu.choices.id(menu.selected), menu.picked));
        self.typing = None;
        self.update_menu(cx);
        let (Some((id, picked)), Some(menu)) = (kept, &mut self.menu) else {
            return;
        };
        if let Some(ix) = (0..menu.choices.len()).find(|&ix| menu.choices.id(ix) == id) {
            menu.selected = ix;
            menu.picked = picked;
            menu.scroll.scroll_to_item(ix);
        }
    }

    /// Opens, updates or closes the menu for what is typed at the cursor,
    /// and asks the session's MCP servers what it needs from them.
    fn update_menu(&mut self, cx: &mut Context<'_, Self>) {
        let input = self.input.read(cx);
        let (text, cursor) = (input.value(), input.cursor());
        let typing = Completion::at(&text, cursor)
            .map(Typing::Name)
            .or_else(|| Mention::at(&text, cursor).map(Typing::Mention))
            .or_else(|| ArgumentValue::at(&text, cursor, self.mcp.prompts()).map(Typing::Value));
        // Asked on every change: a failed read asks again at the next key.
        self.ask_mcp(&text, typing.as_ref(), cx);
        if typing == self.typing {
            return;
        }
        if self.dismissed != typing {
            self.dismissed = None;
        }
        let menu = typing
            .clone()
            .filter(|_| self.dismissed.is_none())
            .and_then(|typing| Choices::of(typing, &self.commands, &self.mcp))
            .map(|choices| Menu {
                choices,
                selected: 0,
                picked: false,
                scroll: ScrollHandle::new(),
            });
        self.typing = typing;
        if menu.is_some() || self.menu.is_some() {
            self.menu = menu;
            cx.notify();
        }
    }

    /// Takes `key` from the text box if the menu is open and has a use for
    /// it; says whether it did.
    fn menu_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        self.update_menu(cx);
        let Some(menu) = &mut self.menu else {
            return false;
        };
        let last = menu.choices.len() - 1;
        let selected = menu.selected;
        match key {
            MenuKey::Up | MenuKey::Down => {
                menu.selected = match key {
                    MenuKey::Up if selected == 0 => last,
                    MenuKey::Up => selected - 1,
                    _ if selected == last => 0,
                    _ => selected + 1,
                };
                menu.picked = true;
                menu.scroll.scroll_to_item(menu.selected);
                cx.notify();
            }
            MenuKey::Escape => {
                self.menu = None;
                self.dismissed = self.typing.clone();
                cx.notify();
            }
            MenuKey::Tab => self.complete(selected, window, cx),
            MenuKey::Enter => {
                if !menu.enter_takes() {
                    return false;
                }
                self.complete(selected, window, cx);
            }
        }
        true
    }

    /// Completes what is typed at the cursor with the menu's match `ix`:
    /// a name or value written in, a resource read to attach, or a
    /// template's URI written in to fill in.
    fn complete(&mut self, ix: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        // `ix` comes from the menu as last drawn; the matches may have
        // changed since.
        let Some(menu) = self.menu.take_if(|menu| ix < menu.choices.len()) else {
            return;
        };
        let text = self.input.read(cx).value();
        let edit = match menu.choices {
            Choices::Names(completion, matches) => completion.edit(&text, &matches[ix]),
            Choices::Values(value, matches) => {
                self.mcp.forget_values();
                value.edit(&matches[ix])
            }
            Choices::Resources(mention, matches) if matches[ix].templated => {
                mention.edit(&text, &matches[ix].uri)
            }
            Choices::Resources(mention, matches) => {
                self.read_resource(&matches[ix], cx);
                mention.remove(&text)
            }
        };
        self.input.update(cx, |input, cx| {
            input.set_selected_range(edit.range, cx);
            input.replace(edit.text, window, cx);
            input.set_selected_range(edit.cursor..edit.cursor, cx);
        });
        cx.notify();
    }

    /// The label of the remote machine the message goes to; see
    /// [`Self::set_remote`].
    pub(crate) fn remote(&self) -> Option<SharedString> {
        self.remote.clone()
    }

    /// Says the message goes to the remote machine labelled `remote`, or
    /// to this one with `None`. Files that would go by their path are
    /// refused there, and taken off when the message moves there.
    pub(crate) fn set_remote(&mut self, remote: Option<SharedString>, cx: &mut Context<'_, Self>) {
        self.remote = remote;
        let Some(machine) = self.remote.clone() else {
            return;
        };
        let before = self.attachments.len();
        self.attachments.retain(|item| !by_path(&item.attachment));
        if self.attachments.len() != before {
            self.refusal = Some(format!("Took off the files that go by path: Rho on {machine} can't read this machine's files.").into());
        }
        cx.notify();
    }

    /// The text box. Its `PressEnter` event (without Shift) is the send
    /// request.
    pub(crate) fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    /// Whether the user has typed, quoted or attached nothing, not even
    /// whitespace.
    pub(crate) fn is_empty(&self, cx: &App) -> bool {
        self.attachments.is_empty()
            && self.reads.is_empty()
            && self.quotes.is_empty()
            && self.input.read(cx).value().is_empty()
    }

    /// Whether there is a message to send: more than whitespace, a quote,
    /// or an attachment.
    pub(crate) fn has_message(&self, cx: &App) -> bool {
        !self.attachments.is_empty()
            || !self.quotes.is_empty()
            || !self.input.read(cx).value().trim().is_empty()
    }

    /// Whether an MCP resource is still being read to attach.
    #[cfg(test)]
    pub(crate) fn is_reading(&self) -> bool {
        !self.reads.is_empty()
    }

    /// Whether the session's MCP catalog is being asked for.
    #[cfg(test)]
    pub(crate) fn is_asking_catalog(&self) -> bool {
        self.mcp.is_asking_catalog()
    }

    /// Whether the message can go: there is one, and no MCP resource is
    /// still being read for it, which would go missing.
    pub(crate) fn can_send(&self, cx: &App) -> bool {
        self.reads.is_empty() && !self.prompt(cx).is_empty()
    }

    /// The message as it stands: the quotes and the text, trimmed, and the
    /// attachments.
    pub(crate) fn prompt(&self, cx: &App) -> Prompt {
        let typed = self.input.read(cx).value();
        let quotes = self.quotes.iter().map(|quote| quote.text.as_str());
        let text = with_quotes(quotes, typed.trim());
        let attachments = self.attachments.iter().map(|item| item.attachment.clone());
        Prompt::new(text, attachments.collect())
    }

    /// Quotes `text` above the composer's text box, after any quoted
    /// already; see [`trim_quote`]. Blank text adds nothing.
    pub(crate) fn add_quote(&mut self, text: &str, cx: &mut Context<'_, Self>) {
        let Some(text) = trim_quote(text) else {
            return;
        };
        self.next_id += 1;
        self.quotes.push(Quote {
            id: self.next_id,
            text: text.to_owned(),
        });
        cx.notify();
    }

    /// Removes quote `id`.
    fn remove_quote(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        self.quotes.retain(|quote| quote.id != id);
        cx.notify();
    }

    /// Takes the message out, leaving the composer empty.
    pub(crate) fn take(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Prompt {
        let prompt = self.prompt(cx);
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Their decoded images stay: the transcript shows the same ones.
        self.attachments.clear();
        self.quotes.clear();
        self.refusal = None;
        cx.notify();
        prompt
    }

    /// Puts `prompt` back, as a send that failed does: its text, quotes
    /// and all, above any typed since, its attachments before any added
    /// since.
    pub(crate) fn give_back(
        &mut self,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (text, attachments) = prompt.into_parts();
        if !text.is_empty() {
            let typed = self.input.read(cx).value();
            let text = if typed.is_empty() {
                text
            } else if text
                .lines()
                .last()
                .is_some_and(|line| line.starts_with('>'))
            {
                // A blank line, or the typed text would join the quote.
                format!("{text}\n\n{typed}")
            } else {
                format!("{text}\n{typed}")
            };
            self.input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
        let returned: Vec<_> = attachments
            .into_iter()
            .map(|attachment| {
                self.item(attachment)
                    .expect("an attachment the composer took reads as it did then")
            })
            .collect();
        self.attachments.splice(0..0, returned);
        cx.notify();
    }

    /// Asks the desktop's file chooser for files to attach.
    fn choose_files(cx: &mut Context<'_, Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            let _ = this.update(cx, |this, cx| match chosen {
                Ok(Ok(Some(paths))) => Self::attach_paths(paths, cx),
                // Cancelled, or the chooser went away.
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => {
                    this.refusal = Some(format!("Couldn't open a file chooser: {error}.").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Reads the files at `paths` off the UI thread, then attaches each, in
    /// order. Those that can't go are refused, each saying why.
    fn attach_paths(paths: Vec<PathBuf>, cx: &mut Context<'_, Self>) {
        if paths.is_empty() {
            return;
        }
        let read = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(|path| {
                    let read = attachments::read(&path);
                    (path, read)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| {
                let refused: Vec<String> = read
                    .into_iter()
                    .filter_map(|(path, read)| {
                        let added = read
                            .and_then(|read| this.add(read.attachment, read.why_linked.as_deref()));
                        let why = added.err()?;
                        let name = path.file_name().unwrap_or(path.as_os_str());
                        let name = name.to_string_lossy();
                        Some(format!("Couldn't attach {name}: {why}."))
                    })
                    .collect();
                this.refusal = (!refused.is_empty()).then(|| refused.join("\n").into());
                cx.notify();
            });
        })
        .detach();
    }

    /// Takes what `item` holds, if anything to attach, and says whether it
    /// did: a paste of images or copied files inserts no text. One that
    /// can't go is refused, saying why.
    fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<'_, Self>) -> bool {
        let mut took = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    took = true;
                    self.refusal = None;
                    if let Err(why) = attachments::from_clipboard(image)
                        .and_then(|image| self.add(Attachment::Image(image), None))
                    {
                        self.refusal = Some(format!("Couldn't attach the image: {why}.").into());
                    }
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    took = true;
                    Self::attach_paths(paths.paths().to_vec(), cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if took {
            cx.notify();
        }
        took
    }

    /// Adds `attachment` after the others, if one message can carry it with
    /// them, and gives its card's ID. `why_linked` says why a file goes by
    /// its path, for its chip, or for the refusal when it can't go that way.
    fn add(&mut self, attachment: Attachment, why_linked: Option<&str>) -> Result<u64, String> {
        if let (Some(machine), true) = (&self.remote, by_path(&attachment)) {
            let because = why_linked.map_or_default(|why| format!(" ({why})"));
            return Err(format!(
                "it would go by its path{because}, and Rho on {machine} can't read this machine's files"
            ));
        }
        let held = self.attachments.iter().map(|item| &item.attachment);
        check_attachment_total(held.chain([&attachment])).map_err(|error| error.to_string())?;
        let mut item = self
            .item(attachment)
            .ok_or("its data doesn't read as the image it starts as")?;
        if let Some(why) = why_linked {
            item.preview = item.preview.linked_because(why);
        }
        let id = item.id;
        self.attachments.push(item);
        Ok(id)
    }

    /// `attachment` as the composer holds it, under a new ID.
    fn item(&mut self, attachment: Attachment) -> Option<Item> {
        let preview = Preview::new(&attachment)?;
        self.next_id += 1;
        Some(Item {
            id: self.next_id,
            attachment,
            preview,
        })
    }

    /// Removes attachment `id`, and the decoded copy GPUI drew an image
    /// from. If the transcript shows the same image, it decodes it again.
    /// A resource being read stops being read.
    fn remove(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        if let Some(at) = self.attachments.iter().position(|item| item.id == id) {
            self.attachments.remove(at).preview.release(cx);
        }
        self.reads.retain(|read| read.id != id);
        cx.notify();
    }

    /// Removes the last card, as Backspace in the empty text box does: a
    /// resource being read, which shows last, or else the last
    /// attachment. Says whether there was one to remove.
    fn remove_last(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if !self.input.read(cx).value().is_empty() {
            return false;
        }
        let last = self.reads.last().map(|read| read.id);
        let Some(id) = last.or_else(|| self.attachments.last().map(|item| item.id)) else {
            return false;
        };
        self.remove(id, cx);
        true
    }
}

/// The keyboard goes to the text box.
impl Focusable for ComposerState {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Whether the composer can send, or the turn it sent is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Send,
    /// The session is being created: the box is disabled and Send spins.
    Starting,
    /// A turn is running: Stop replaces Send, and Enter steers the turn.
    Stop,
}

#[derive(IntoElement)]
pub(crate) struct Composer {
    state: Entity<ComposerState>,
    mode: ComposerMode,
    /// Session settings at the left of the footer.
    leading: Option<AnyElement>,
    /// Session settings just before Send.
    trailing: Option<AnyElement>,
    /// Marks of what needs the user's eye, before the context ring.
    status: Option<AnyElement>,
    /// Context use and cost, before the trailing settings.
    usage: Option<UsageRing>,
    on_send: Handler,
    on_stop: Handler,
}

impl Composer {
    pub(crate) fn new(
        state: &Entity<ComposerState>,
        mode: ComposerMode,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
        on_stop: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            state: state.clone(),
            mode,
            leading: None,
            trailing: None,
            status: None,
            usage: None,
            on_send: Rc::new(on_send),
            on_stop: Rc::new(on_stop),
        }
    }

    pub(crate) fn leading(mut self, controls: impl IntoElement) -> Self {
        self.leading = Some(controls.into_any_element());
        self
    }

    pub(crate) fn trailing(mut self, controls: Option<impl IntoElement>) -> Self {
        self.trailing = controls.map(IntoElement::into_any_element);
        self
    }

    pub(crate) fn status(mut self, status: Option<impl IntoElement>) -> Self {
        self.status = status.map(IntoElement::into_any_element);
        self
    }

    pub(crate) fn usage(mut self, usage: Option<UsageRing>) -> Self {
        self.usage = usage;
        self
    }

    /// The attachments as cards of one height, each with its remove
    /// control.
    fn render_attachments(&self, cx: &App) -> Option<AttachmentGroup> {
        let state = self.state.read(cx);
        if state.attachments.is_empty() && state.reads.is_empty() {
            return None;
        }
        let items = state
            .attachments
            .iter()
            .map(|item| (item.id, &item.preview));
        let reads = state.reads.iter().map(|read| (read.id, &read.preview));
        let cards = items.chain(reads).map(|(id, preview)| {
            let state = self.state.clone();
            // Wrapped so UI tests can find it: Kit's Attachment does not
            // register its ID with them.
            div()
                .id(SharedString::from(format!("composer-attachment-{id}")))
                .test_support()
                .aria_label(preview.label())
                .child(
                    // Large: a 4 rem square, enough to tell screenshots
                    // apart, with file chips as tall.
                    preview
                        .card()
                        .id(("composer-attachment-card", id))
                        .large()
                        .on_remove(move |_, _, cx| {
                            state.update(cx, |state, cx| state.remove(id, cx));
                        }),
                )
        });
        Some(
            AttachmentGroup::new("composer-attachments")
                .with_edge_fade(cx.theme().secondary)
                // With the room each card keeps for its remove control,
                // 0.75 rem between cards, as from the box's edge.
                .gap_1p5()
                .py_0()
                .children(cards),
        )
    }

    /// The quotes, each a muted passage of up to three lines with its
    /// remove control.
    fn render_quotes(&self, cx: &App) -> Option<impl IntoElement> {
        let quotes = &self.state.read(cx).quotes;
        if quotes.is_empty() {
            return None;
        }
        let rows = quotes.iter().map(|quote| {
            let (id, state) = (quote.id, self.state.clone());
            h_flex()
                .id(SharedString::from(format!("composer-quote-{id}")))
                .test_support()
                .aria_label(SharedString::from(quote.text.clone()))
                .items_start()
                .gap_2()
                .pl_2()
                .border_l_2()
                .border_color(cx.theme().muted_foreground.opacity(0.5))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .line_clamp(3)
                        .child(quote.text.clone()),
                )
                .child(
                    Button::new(SharedString::from(format!("composer-quote-{id}-remove")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Remove quote")
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.remove_quote(id, cx));
                        }),
                )
        });
        Some(v_flex().gap_1p5().children(rows))
    }

    /// The button that opens the desktop's file chooser, off while the
    /// session is being created.
    fn render_attach(&self) -> Button {
        let state = self.state.clone();
        Button::new("composer-attach")
            .ghost()
            .small()
            .icon(Lucide::Paperclip)
            .tooltip("Attach files…")
            .disabled(self.mode == ComposerMode::Starting)
            .on_click(move |_, _, cx| {
                state.update(cx, |_, cx| ComposerState::choose_files(cx));
            })
    }

    /// Why the last attachment was refused, with a button to dismiss it.
    fn render_refusal(&self, cx: &App) -> Option<impl IntoElement> {
        let why = self.state.read(cx).refusal.clone()?;
        let state = self.state.clone();
        // Wrapped so UI tests can find it, as the session's alerts are.
        Some(
            div().id("composer-refusal").test_support().child(
                Alert::warning("composer-refusal-alert", why)
                    .small()
                    .on_close(move |_, _, cx| {
                        state.update(cx, |state, cx| {
                            state.refusal = None;
                            cx.notify();
                        });
                    }),
            ),
        )
    }

    /// The text box. An open completion menu takes its keys before the box
    /// sees them, and the box passes Backspace on only when it has nothing
    /// to delete.
    fn render_text_box(&self, cx: &App) -> impl IntoElement {
        let input = self.state.read(cx).input.clone();
        let starting = self.mode == ComposerMode::Starting;
        let enter = self.state.clone();
        let backspace = self.state.clone();
        let paste_into = self.state.clone();
        div()
            .capture_action(menu_key::<MoveUp>(&self.state, MenuKey::Up))
            .capture_action(menu_key::<MoveDown>(&self.state, MenuKey::Down))
            .capture_action(menu_key::<IndentInline>(&self.state, MenuKey::Tab))
            .capture_action(menu_key::<Escape>(&self.state, MenuKey::Escape))
            .capture_action(move |action: &Enter, window, cx| {
                // Shift+Enter and Alt+Enter start a line, Ctrl+Enter sends.
                if !action.shift && !action.secondary {
                    offer_key(&enter, MenuKey::Enter, window, cx);
                }
            })
            .on_action(move |_: &Backspace, _, cx| {
                if !backspace.update(cx, ComposerState::remove_last) {
                    cx.propagate();
                }
            })
            .child(
                Textarea::new(&input)
                    .appearance(false)
                    .disabled(starting)
                    .aria_label("Prompt")
                    .on_paste(move |item, _, cx| {
                        paste_into.update(cx, |state, cx| state.paste(item, cx))
                    }),
            )
    }

    /// The names that complete the one at the cursor, floating above the
    /// box. Kit's completion menu belongs to its code editor, so this one is
    /// built from Kit's list rows on its popover surface.
    fn render_menu(&self, cx: &App) -> Option<impl IntoElement> {
        if self.mode == ComposerMode::Starting {
            return None;
        }
        let menu = self.state.read(cx).menu.as_ref()?;
        let rows: Vec<Row> = match &menu.choices {
            Choices::Names(completion, matches) => matches
                .iter()
                .map(|command| Row {
                    name: format!("{}{}", completion.kind().sigil(), command.name()),
                    hint: command.input_hint().map(str::to_owned),
                    detail: command.description().to_owned(),
                })
                .collect(),
            Choices::Resources(_, matches) => matches
                .iter()
                .map(|resource| Row {
                    name: resource
                        .title
                        .clone()
                        .unwrap_or_else(|| resource.name.clone()),
                    hint: resource.templated.then(|| "template".to_owned()),
                    detail: resource.uri.clone(),
                })
                .collect(),
            Choices::Values(_, matches) => matches
                .iter()
                .map(|value| Row {
                    name: value.clone(),
                    hint: None,
                    detail: String::new(),
                })
                .collect(),
        };
        let muted = cx.theme().muted_foreground;
        let rows = rows.into_iter().enumerate().map(|(ix, row)| {
            let state = self.state.clone();
            ListItem::new(SharedString::from(menu.choices.id(ix)))
                .selected(ix == menu.selected)
                .accessibility_label(row.name.clone())
                .px_2()
                .text_sm()
                .rounded(cx.theme().radius)
                .on_click(move |_, window, cx| {
                    state.update(cx, |state, cx| {
                        state.complete(ix, window, cx);
                        window.focus(&state.input.focus_handle(cx), cx);
                    });
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_shrink_0().child(row.name))
                        .children(
                            row.hint
                                .map(|hint| div().flex_shrink_0().text_color(muted).child(hint)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(muted)
                                .child(row.detail),
                        ),
                )
        });
        let label = menu.choices.label();
        // Deferred, so it paints over the transcript rather than pushing it.
        Some(
            deferred(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_full()
                    .mb_2()
                    .child(
                        v_flex()
                            .id("completion-menu")
                            .test_support()
                            .aria_label(label)
                            .occlude()
                            .popover_style(cx)
                            .p_1()
                            .max_h(rems(18.))
                            .overflow_y_scroll()
                            .track_scroll(&menu.scroll)
                            .children(rows),
                    ),
            )
            .with_priority(1),
        )
    }
}

/// One row of the completion menu: a name, what to type after it, and a
/// muted line on it.
struct Row {
    name: String,
    hint: Option<String>,
    detail: String,
}

/// Offers the text box's `A` key to the completion menu first, as `key`;
/// the menu keeps it only while open.
fn menu_key<A: Action>(
    state: &Entity<ComposerState>,
    key: MenuKey,
) -> impl Fn(&A, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_, window, cx| offer_key(&state, key, window, cx)
}

fn offer_key(state: &Entity<ComposerState>, key: MenuKey, window: &mut Window, cx: &mut App) {
    if state.update(cx, |state, cx| state.menu_key(key, window, cx)) {
        cx.stop_propagation();
    }
}

impl RenderOnce for Composer {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let quotes = self.render_quotes(cx);
        let attachments = self.render_attachments(cx);
        let refusal = self.render_refusal(cx);
        let menu = self.render_menu(cx);
        let unsendable = !self.state.read(cx).can_send(cx);
        let starting = self.mode == ComposerMode::Starting;
        let text_box = self.render_text_box(cx);
        let attach = self.render_attach();
        let action = if self.mode == ComposerMode::Stop {
            let on_stop = self.on_stop;
            Button::new("composer-stop")
                .danger()
                .icon(IconName::Square)
                .rounded(ButtonRounded::Large)
                .tooltip("Stop (Esc)")
                .on_click(move |_, window, cx| on_stop(window, cx))
        } else {
            let on_send = self.on_send;
            Button::new("composer-send")
                .primary()
                .icon(IconName::ArrowUp)
                .rounded(ButtonRounded::Large)
                .tooltip("Send (Enter)")
                .disabled(unsendable)
                .loading(starting)
                .on_click(move |_, window, cx| on_send(window, cx))
        };
        let drop_into = self.state.clone();
        // Kit's remove control rides 6px, 0.375 rem at the default rem,
        // above and right of each card, in room the card keeps for it. When
        // the cards come first, that room is part of the box's 0.75 rem
        // padding.
        let top = if quotes.is_none() && attachments.is_some() {
            rems(0.375)
        } else {
            rems(0.75)
        };
        let input_box = v_flex()
            .id("composer")
            .w_full()
            .gap_2()
            .pt(top)
            .px_3()
            .pb_2()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().input)
            .bg(cx.theme().secondary)
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                if !starting {
                    let paths = paths.paths().to_vec();
                    drop_into.update(cx, |_, cx| ComposerState::attach_paths(paths, cx));
                }
            })
            .relative()
            .children(menu)
            .children(quotes)
            .children(attachments)
            .child(text_box)
            .child(
                h_flex()
                    .gap_1()
                    // A browser's file chooser gives the page's files, not
                    // the machine's paths a prompt names; paste still works.
                    .children(cfg!(not(target_family = "wasm")).then_some(attach))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .children(self.leading),
                    )
                    .children(self.status)
                    .children(self.usage)
                    .children(self.trailing)
                    .child(action),
            );
        v_flex().w_full().gap_2().children(refusal).child(input_box)
    }
}

/// `text` without the blank lines around it, its first line keeping its
/// indent, as code does; `None` if it is blank.
fn trim_quote(text: &str) -> Option<&str> {
    let first = text.find(|c: char| !c.is_whitespace())?;
    let start = text[..first].rfind('\n').map_or(0, |newline| newline + 1);
    Some(text[start..].trim_end())
}

/// `text` and `quotes`, each a Markdown block quote of its own, a blank
/// line apart. The quotes go first, unless `text` is a slash command, which
/// rho runs only from the start of a prompt.
fn with_quotes<'a>(quotes: impl IntoIterator<Item = &'a str>, text: &str) -> String {
    let blocks = quotes.into_iter().map(|quote| {
        quote
            .lines()
            .map(|line| {
                if line.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    let text = (!text.is_empty()).then(|| text.to_owned());
    let blocks: Vec<String> = if text.as_deref().is_some_and(|text| text.starts_with('/')) {
        text.into_iter().chain(blocks).collect()
    } else {
        blocks.chain(text).collect()
    };
    blocks.join("\n\n")
}

/// Whether `attachment` goes as its path, for the agent to read itself.
fn by_path(attachment: &Attachment) -> bool {
    matches!(attachment, Attachment::File(file) if file.text().is_none())
}

#[cfg(test)]
mod tests {
    use super::{trim_quote, with_quotes};

    /// Each quote is a block quote of its own, every line marked, so a
    /// blank line inside one doesn't end it; a slash command stays first.
    #[test]
    fn quotes_go_before_the_text_as_block_quotes() {
        let cases: [(&[&str], &str, &str); 4] = [
            (&[], "just text", "just text"),
            (&["one\n\ntwo"], "why?", "> one\n>\n> two\n\nwhy?"),
            (&["a", "b"], "", "> a\n\n> b"),
            (&["a"], "/review", "/review\n\n> a"),
        ];
        for (quotes, text, sent) in cases {
            assert_eq!(with_quotes(quotes.iter().copied(), text), sent);
        }
    }

    /// A selection loses the blank lines around it, not the indent of its
    /// first line.
    #[test]
    fn a_quote_keeps_its_first_indent() {
        let cases = [
            ("  \n\n", None),
            ("\n  let x = 1;\n  x\n\n", Some("  let x = 1;\n  x")),
            ("plain ", Some("plain")),
        ];
        for (selected, quoted) in cases {
            assert_eq!(trim_quote(selected), quoted, "{selected:?}");
        }
    }
}
