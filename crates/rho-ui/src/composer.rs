//! The prompt box under the transcript. Enter sends, Shift+Enter starts a new
//! line; while a turn runs Send becomes Stop, and Enter steers the turn. Its
//! footer holds the attach button and the session settings on the left, then
//! context use and cost, the model, and the round Send button.
//!
//! What the user attaches shows above the text and goes with it, after it,
//! as one message, as rho's terminal UI sends one: an image pasted, or any
//! file chosen with the attach button, dropped on the box, or pasted as a
//! copied file. Each has a remove control on hover; Backspace in the empty
//! text box removes the last.
//!
//! Typing a slash command at the start of the text, or a `$skill` anywhere
//! in it, opens a menu of the session's names that complete it, floating
//! above the box. Up and Down move the highlight, Tab completes, and Enter
//! completes unless the name is typed out already, or reads as an amount
//! such as `$20`: then it sends. Escape closes the menu.

use std::{path::PathBuf, rc::Rc};

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
    check_attachment_total, Attachment, CommandInfo, CommandKind, Completion, Prompt,
};

use crate::attachments::{self, Preview};

/// Grows from one line to this many before it scrolls: enough for a short
/// multi-paragraph prompt without pushing the transcript off a laptop screen.
const MAX_ROWS: usize = 8;

/// What the user is writing: the text, and what is attached to it. Its
/// owner subscribes to [`Self::input`] for Enter, and observes this entity,
/// which notifies when the attachments change.
pub(crate) struct ComposerState {
    input: Entity<TextareaState>,
    attachments: Vec<Item>,
    /// Names the next attachment. Never reused, so a card keeps its
    /// identity as others come and go.
    next_id: u64,
    /// Why the last attachment was refused, until it is dismissed, or the
    /// next attachment or send.
    refusal: Option<SharedString>,
    /// The session's slash commands and skills, which the menu offers.
    commands: Vec<CommandInfo>,
    /// The name being typed at the cursor, as last seen.
    typing: Option<Completion>,
    /// The names that complete it, while their menu is open.
    menu: Option<Menu>,
    /// The name Escape closed the menu on; typing on opens it again.
    dismissed: Option<Completion>,
    _subscription: Subscription,
}

/// An attachment in the composer.
struct Item {
    id: u64,
    attachment: Attachment,
    preview: Preview,
}

/// The open menu of names that complete the one at the cursor.
struct Menu {
    completion: Completion,
    matches: Vec<CommandInfo>,
    /// The highlighted match.
    selected: usize,
    /// The user moved the highlight, so Enter takes it whatever is typed.
    picked: bool,
    /// Keeps the highlighted match in view.
    scroll: ScrollHandle,
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
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, MAX_ROWS)
                .submit_on_enter(true)
                .placeholder("Ask rho to do something")
        });
        // The text box notifies when its text or cursor moves.
        let subscription = cx.observe(&input, |this, _, cx| this.update_menu(cx));
        Self {
            input,
            attachments: Vec::new(),
            next_id: 0,
            refusal: None,
            commands: Vec::new(),
            typing: None,
            menu: None,
            dismissed: None,
            _subscription: subscription,
        }
    }

    /// Offers `commands`, the session's slash commands and skills, to
    /// complete.
    pub(crate) fn set_commands(&mut self, commands: &[CommandInfo], cx: &mut Context<'_, Self>) {
        if self.commands != commands {
            self.commands = commands.to_vec();
            self.typing = None;
            self.update_menu(cx);
        }
    }

    /// Opens, updates or closes the menu for the name at the cursor.
    fn update_menu(&mut self, cx: &mut Context<'_, Self>) {
        let input = self.input.read(cx);
        let typing = Completion::at(&input.value(), input.cursor());
        if typing == self.typing {
            return;
        }
        if self.dismissed != typing {
            self.dismissed = None;
        }
        let menu = typing
            .clone()
            .filter(|_| self.dismissed.is_none())
            .and_then(|completion| {
                let matches: Vec<_> = completion.matches(&self.commands).cloned().collect();
                (!matches.is_empty()).then(|| Menu {
                    completion,
                    matches,
                    selected: 0,
                    picked: false,
                    scroll: ScrollHandle::new(),
                })
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
        let last = menu.matches.len() - 1;
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
                self.dismissed = self.menu.take().map(|menu| menu.completion);
                cx.notify();
            }
            MenuKey::Tab => self.complete(selected, window, cx),
            MenuKey::Enter => {
                let completion = &menu.completion;
                if completion.is_typed_out(&menu.matches[selected])
                    || !(menu.picked || completion.reads_as_name())
                {
                    return false;
                }
                self.complete(selected, window, cx);
            }
        }
        true
    }

    /// Completes the name at the cursor to the menu's match `ix`.
    fn complete(&mut self, ix: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        let edit = menu
            .completion
            .edit(&self.input.read(cx).value(), &menu.matches[ix]);
        self.input.update(cx, |input, cx| {
            input.set_selected_range(edit.range, cx);
            input.replace(edit.text, window, cx);
            input.set_selected_range(edit.cursor..edit.cursor, cx);
        });
        cx.notify();
    }

    /// The text box. Its `PressEnter` event (without Shift) is the send
    /// request.
    pub(crate) fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    /// Whether the user has typed or attached nothing, not even whitespace.
    pub(crate) fn is_empty(&self, cx: &App) -> bool {
        self.attachments.is_empty() && self.input.read(cx).value().is_empty()
    }

    /// The message as it stands: the text, trimmed, and the attachments.
    pub(crate) fn prompt(&self, cx: &App) -> Prompt {
        let text = self.input.read(cx).value().trim().to_owned();
        let attachments = self.attachments.iter().map(|item| item.attachment.clone());
        Prompt::new(text, attachments.collect())
    }

    /// Takes the message out, leaving the composer empty.
    pub(crate) fn take(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Prompt {
        let prompt = self.prompt(cx);
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Their decoded images stay: the transcript shows the same ones.
        self.attachments.clear();
        self.refusal = None;
        cx.notify();
        prompt
    }

    /// Puts `prompt` back, as a send that failed does: its text above any
    /// typed since, its attachments before any added since.
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
    /// order. One that can't go is refused, saying why.
    fn attach_paths(paths: Vec<PathBuf>, cx: &mut Context<'_, Self>) {
        if paths.is_empty() {
            return;
        }
        let read = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(|path| {
                    let attachment = attachments::read(&path);
                    (path, attachment)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| {
                this.refusal = None;
                for (path, attachment) in read {
                    if let Err(why) = attachment.and_then(|attachment| this.add(attachment)) {
                        let name = path.file_name().unwrap_or(path.as_os_str());
                        let name = name.to_string_lossy();
                        this.refusal = Some(format!("Couldn't attach {name}: {why}.").into());
                    }
                }
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
                        .and_then(|image| self.add(Attachment::Image(image)))
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
    /// them.
    fn add(&mut self, attachment: Attachment) -> Result<(), String> {
        let held = self.attachments.iter().map(|item| &item.attachment);
        check_attachment_total(held.chain([&attachment])).map_err(|error| error.to_string())?;
        let item = self
            .item(attachment)
            .ok_or("its data doesn't read as the image it starts as")?;
        self.attachments.push(item);
        Ok(())
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
    fn remove(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        if let Some(at) = self.attachments.iter().position(|item| item.id == id) {
            self.attachments.remove(at).preview.release(cx);
            cx.notify();
        }
    }

    /// Removes the last attachment, as Backspace in the empty text box
    /// does. Says whether there was one to remove.
    fn remove_last(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if !self.input.read(cx).value().is_empty() {
            return false;
        }
        let Some(last) = self.attachments.last() else {
            return false;
        };
        let id = last.id;
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
    /// Context use and cost, before Send.
    meta: Option<String>,
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
            meta: None,
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

    pub(crate) fn meta(mut self, meta: Option<String>) -> Self {
        self.meta = meta;
        self
    }

    /// The attachments as cards of one height, each with its remove
    /// control.
    fn render_attachments(&self, cx: &App) -> Option<AttachmentGroup> {
        let items = &self.state.read(cx).attachments;
        if items.is_empty() {
            return None;
        }
        let cards = items.iter().map(|item| {
            let (id, state) = (item.id, self.state.clone());
            // Wrapped so UI tests can find it: Kit's Attachment does not
            // register its ID with them.
            div()
                .id(SharedString::from(format!("composer-attachment-{id}")))
                .test_support()
                .aria_label(item.preview.label())
                .child(
                    // Large: a 4 rem square, enough to tell screenshots
                    // apart, with file chips as tall.
                    item.preview
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
                // Shift+Enter starts a line, Ctrl+Enter sends.
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
        let kind = menu.completion.kind();
        let muted = cx.theme().muted_foreground;
        let rows = menu.matches.iter().enumerate().map(|(ix, command)| {
            let name = format!("{}{}", kind.sigil(), command.name());
            let state = self.state.clone();
            ListItem::new(SharedString::from(format!("completion-{}", command.name())))
                .selected(ix == menu.selected)
                .accessibility_label(name.clone())
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
                        .child(div().flex_shrink_0().child(name))
                        .children(command.input_hint().map(|hint| {
                            div()
                                .flex_shrink_0()
                                .text_color(muted)
                                .child(hint.to_owned())
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(muted)
                                .child(command.description().to_owned()),
                        ),
                )
        });
        let label = match kind {
            CommandKind::Slash => "Commands",
            CommandKind::Skill => "Skills",
        };
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
        let attachments = self.render_attachments(cx);
        let refusal = self.render_refusal(cx);
        let menu = self.render_menu(cx);
        let empty = self.state.read(cx).prompt(cx).is_empty();
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
                .disabled(empty)
                .loading(starting)
                .on_click(move |_, window, cx| on_send(window, cx))
        };
        let drop_into = self.state.clone();
        // Kit's remove control rides 6px, 0.375 rem at the default rem,
        // above and right of each card, in room the card keeps for it. Above
        // the cards, that room is part of the box's 0.75 rem padding.
        let top = if attachments.is_some() {
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
            .children(attachments)
            .child(text_box)
            .child(
                h_flex()
                    .gap_1()
                    .child(attach)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .children(self.leading),
                    )
                    .children(self.meta.map(|meta| {
                        div()
                            .flex_shrink_0()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta)
                    }))
                    .children(self.trailing)
                    .child(action),
            );
        v_flex().w_full().gap_2().children(refusal).child(input_box)
    }
}
