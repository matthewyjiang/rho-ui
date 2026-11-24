//! Images a message shows by path: a paragraph of only `![alt](plot.png)`
//! images, each drawn as a picture in the message's measure, at its own
//! size or scaled down to fit. Clicking one opens it in the
//! [`lightbox`](crate::lightbox).
//!
//! A path is read on the session's machine, from the session's folder when
//! relative, so a remote session's pictures come over its ssh link. A
//! picture that can't be shown says so in its place, with its path and
//! why; one that couldn't be read, as a file not written yet, is read
//! again when a turn ends or it is clicked. `data:` images are GPUI Kit's
//! own, as are images inside a line of text, which Kit shows only from a
//! `data:` URL.

use std::collections::{HashMap, VecDeque};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::text::{
    markdown_ast as mdast, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::component::{h_flex, spinner::Spinner, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::{
    div, px, AnyElement, App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};

use crate::{
    attachments::ImagePreview, folders::FileRead, lightbox, machines::Place, workspaces::Workspaces,
};

/// The largest image file read, in bytes: what rho's own `read_file`
/// previews (rho's `docs/tools-workspace/documents-and-images.md`).
const FILE_LIMIT: usize = 32 << 20;

/// The longest side of an image shown, in pixels: what rho's own
/// `read_file` decodes (the same doc). A larger one can take hundreds of
/// MiB decoded, and pass the GPU's largest texture, which draws nothing.
/// Attached and tool-result images keep to it too; see
/// [`crate::attachments::Preview::of_image`].
pub(crate) const MAX_SIDE: u32 = 4096;

/// The tallest a picture shows in a message, in rems: 384 px at 100%, so a
/// tall screenshot leaves the text around it in view.
const MAX_HEIGHT_REMS: f32 = 24.;

/// One message's picture: the message's entry, and its path as written.
/// Each message reads its own, so a file rewritten between replies shows
/// as it was when each reply named it.
type Key = (usize, String);

/// The pictures one session's messages show, read once each.
pub(crate) struct Pictures {
    /// The session's folder, on its machine.
    place: Place,
    workspaces: Entity<Workspaces>,
    loads: HashMap<Key, Load>,
    /// Reads waiting their turn, with the path each reads. One runs at a
    /// time: on a remote machine each is an ssh session on the shared
    /// connection, which runs only a few scripts at once, and the Files
    /// surface and git need some too.
    queue: VecDeque<(Key, String)>,
    reading: bool,
}

/// Where one picture is.
#[derive(Clone)]
enum Load {
    Reading,
    Read(ImagePreview),
    /// It can't be shown, and why: the file is there but isn't an image
    /// rho-ui shows, or the path is a link of another kind.
    Failed(SharedString),
    /// It couldn't be read, and why: it isn't there yet, or the machine
    /// didn't answer. Read again when a turn ends, or when clicked.
    Unread(SharedString),
}

/// A read of a picture of message `entry` ended or starts over, which
/// changes the message's height.
pub(crate) struct PictureRead {
    pub(crate) entry: usize,
}

impl EventEmitter<PictureRead> for Pictures {}

impl Pictures {
    pub(crate) fn new(place: Place, workspaces: Entity<Workspaces>) -> Self {
        Self {
            place,
            workspaces,
            loads: HashMap::new(),
            queue: VecDeque::new(),
            reading: false,
        }
    }

    /// Where the picture at `src` in message `entry` is, queueing a read of
    /// it the first time. [`PictureRead`] says when the read ends.
    fn load(&mut self, entry: usize, src: &str, cx: &mut Context<'_, Self>) -> Load {
        let key = (entry, src.to_owned());
        if let Some(load) = self.loads.get(&key) {
            return load.clone();
        }
        let load = match file_path(src) {
            Ok(path) => {
                self.queue.push_back((key.clone(), path));
                Load::Reading
            }
            Err(why) => Load::Failed(why.into()),
        };
        self.loads.insert(key, load.clone());
        self.read_next(cx);
        load
    }

    /// Reads again every picture that couldn't be read, as a turn that
    /// may have written them has ended.
    pub(crate) fn retry(&mut self, cx: &mut Context<'_, Self>) {
        let unread: Vec<Key> = self
            .loads
            .iter()
            .filter(|(_, load)| matches!(load, Load::Unread(_)))
            .map(|(key, _)| key.clone())
            .collect();
        for key in unread {
            self.loads.remove(&key);
            cx.emit(PictureRead { entry: key.0 });
        }
    }

    /// Reads the picture at `src` in message `entry` again.
    fn retry_one(&mut self, entry: usize, src: &str, cx: &mut Context<'_, Self>) {
        if self.loads.remove(&(entry, src.to_owned())).is_some() {
            cx.emit(PictureRead { entry });
        }
    }

    /// Forgets the pictures of messages from `entry` on, which went or
    /// moved.
    pub(crate) fn forget_from(&mut self, entry: usize) {
        self.loads.retain(|(at, _), _| *at < entry);
        self.queue.retain(|((at, _), _)| *at < entry);
    }

    /// Starts the next queued read, unless one runs.
    fn read_next(&mut self, cx: &mut Context<'_, Self>) {
        if self.reading {
            return;
        }
        let Some((key, path)) = self.queue.pop_front() else {
            return;
        };
        self.reading = true;
        let reader = self.workspaces.read(cx).reader(&self.place.machine);
        let folder = self.place.path.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let read = reader
                .read_file(&folder, &path, FILE_LIMIT, &executor)
                .await;
            let load = match read {
                Ok(FileRead::Bytes(bytes)) => executor
                    .spawn(async move { ImagePreview::from_file(bytes) })
                    .await
                    .map_or_else(|why| Load::Failed(why.into()), Load::Read),
                Ok(FileRead::TooLarge) => {
                    Load::Failed(format!("It's over {} MiB.", FILE_LIMIT >> 20).into())
                }
                Ok(FileRead::Gone) => Load::Unread("There's no file there.".into()),
                Err(why) => Load::Unread(why.into()),
            };
            let _ = this.update(cx, |this, cx| {
                this.reading = false;
                // A picture forgotten while it was read: its message went.
                if let Some(slot) = this.loads.get_mut(&key) {
                    *slot = load;
                    cx.emit(PictureRead { entry: key.0 });
                }
                this.read_next(cx);
            });
        })
        .detach();
    }
}

/// The path of the file a picture's `src` names: itself, or a `file://`
/// URL's path, with `%` escapes decoded.
///
/// # Errors
///
/// Says why when `src` is a link of another kind.
fn file_path(src: &str) -> Result<String, String> {
    let path = match src.strip_prefix("file://") {
        // `file://localhost/x` is `/x` on this host, as `file:///x` is,
        // and `file:///C:/x` is Windows' `C:/x`.
        Some(path) => {
            let path = path.strip_prefix("localhost").unwrap_or(path);
            match path.as_bytes() {
                [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => &path[1..],
                _ => path,
            }
        }
        None => match scheme(src) {
            Some(scheme) => {
                return Err(format!("Rho shows images from files, not {scheme}: links."))
            }
            None => src,
        },
    };
    let path = percent_decoded(path).ok_or_else(|| "Its path isn't text.".to_owned())?;
    if path.is_empty() {
        return Err("It names no file.".to_owned());
    }
    Ok(path)
}

/// The scheme `src` starts with, as in `https:`, if it is a URL.
fn scheme(src: &str) -> Option<&str> {
    let (scheme, _) = src.split_once(':')?;
    let mut chars = scheme.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    // One letter is a Windows drive, as in `C:\plot.png`.
    (starts && rest && scheme.len() > 1).then_some(scheme)
}

/// `path` with each `%XX` escape decoded; `None` if that isn't UTF-8.
fn percent_decoded(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let escaped = (byte == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        let (byte, read) = escaped.map_or((byte, 1), |escaped| (escaped, 3));
        decoded.push(byte);
        at += read;
    }
    String::from_utf8(decoded).ok()
}

/// One picture a paragraph shows.
struct Wanted {
    src: String,
    alt: SharedString,
}

impl Wanted {
    /// What names it: its alt text, or its path without one.
    fn name(&self) -> SharedString {
        if self.alt.is_empty() {
            self.src.clone().into()
        } else {
            self.alt.clone()
        }
    }
}

/// The Markdown plugin that draws a paragraph of pictures in message
/// `entry`, read by `pictures`.
pub(crate) struct Plugin {
    pub(crate) pictures: Entity<Pictures>,
    pub(crate) entry: usize,
}

impl MarkdownPlugin for Plugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "pictures"
    }

    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::Paragraph(paragraph) = node else {
            return None;
        };
        let mut wanted = Vec::new();
        for child in &paragraph.children {
            match child {
                mdast::Node::Image(image) if !image.url.starts_with("data:") => {
                    wanted.push(Wanted {
                        src: image.url.clone(),
                        alt: image.alt.clone().into(),
                    });
                }
                mdast::Node::Text(text) if text.value.trim().is_empty() => {}
                mdast::Node::Break(_) => {}
                _ => return None,
            }
        }
        if wanted.is_empty() {
            return None;
        }
        let alts: Vec<&str> = wanted.iter().map(|wanted| wanted.alt.as_ref()).collect();
        let alts = alts.join("\n");
        Some(
            MarkdownNode::new("pictures", wanted)
                .text(alts.clone())
                .markdown(cx.node_source(node)?.to_owned())
                .accessibility_label(alts),
        )
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(wanted) = node.data::<Vec<Wanted>>() else {
            return div().into_any_element();
        };
        let start = node.source_range().map_or(0, |range| range.start);
        let pictures: Vec<AnyElement> = wanted
            .iter()
            .enumerate()
            .map(|(n, wanted)| {
                let load = self.pictures.update(cx, |pictures, cx| {
                    pictures.load(self.entry, &wanted.src, cx)
                });
                let id = SharedString::from(format!("image-{start}-{n}"));
                self.picture(id, wanted, load, window, cx)
            })
            .collect();
        v_flex()
            .w_full()
            .my_2()
            .gap_2()
            .children(pictures)
            .into_any_element()
    }
}

impl Plugin {
    /// Picture `wanted` as `load` has it, in a row of the message's width
    /// named `id`. The picture in it is `picture`, or while it is read
    /// `picture-loading`, or `picture-failed` with why; a click on one
    /// that couldn't be read reads it again.
    fn picture(
        &self,
        id: SharedString,
        wanted: &Wanted,
        load: Load,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let row = h_flex().id(id).test_support().w_full().justify_center();
        let failed = |why: &SharedString| {
            h_flex()
                .id("picture-failed")
                .test_support()
                .w_full()
                .gap_3()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .child(
                    Icon::new(Lucide::ImageOff)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    v_flex().min_w_0().child(wanted.name()).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {why}", wanted.src)),
                    ),
                )
        };
        match load {
            Load::Read(preview) => {
                let (width, height) = preview.pixels();
                #[expect(
                    clippy::as_conversions,
                    clippy::cast_precision_loss,
                    reason = "a side of at most MAX_SIDE px is exact in f32"
                )]
                let (width, height) = (width as f32, height as f32);
                let aspect = width / height;
                let tallest = f32::from(window.rem_size()) * MAX_HEIGHT_REMS;
                let widest = px(width.min(tallest * aspect));
                let shown = div()
                    .w_full()
                    .max_w(widest)
                    .aspect_ratio(aspect)
                    .rounded(theme.radius)
                    .overflow_hidden()
                    .child(gpui_kit::img(preview.image()).size_full());
                row.child(div().w_full().max_w(widest).child(lightbox::opens(
                    "picture",
                    &preview,
                    wanted.name(),
                    shown,
                )))
                .into_any_element()
            }
            Load::Reading => row
                .child(
                    h_flex()
                        .id("picture-loading")
                        .test_support()
                        .w_full()
                        .gap_2()
                        .p_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().xsmall())
                        .child(SharedString::from(wanted.src.clone())),
                )
                .into_any_element(),
            Load::Failed(why) => row.child(failed(&why)).into_any_element(),
            Load::Unread(why) => {
                let (pictures, entry, src) =
                    (self.pictures.clone(), self.entry, wanted.src.clone());
                row.child(
                    failed(&format!("{why} Click to try again.").into())
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            pictures.update(cx, |pictures, cx| pictures.retry_one(entry, &src, cx));
                        }),
                )
                .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture names a file by its path or a `file://` URL, with `%`
    /// escapes; a link of another kind is refused with why.
    #[test]
    fn a_picture_names_a_file() {
        let cases: [(&str, Result<&str, &str>); 10] = [
            ("plot.png", Ok("plot.png")),
            ("out/my%20plot.png", Ok("out/my plot.png")),
            ("file:///tmp/a%25b.png", Ok("/tmp/a%b.png")),
            ("file://localhost/tmp/a.png", Ok("/tmp/a.png")),
            ("C:\\plots\\a.png", Ok("C:\\plots\\a.png")),
            ("file:///C:/plots/a.png", Ok("C:/plots/a.png")),
            ("100%.png", Ok("100%.png")),
            ("100%+5.png", Ok("100%+5.png")),
            (
                "https://example.com/a.png",
                Err("Rho shows images from files, not https: links."),
            ),
            ("file://", Err("It names no file.")),
        ];
        for (src, want) in cases {
            let want = want.map(str::to_owned).map_err(str::to_owned);
            assert_eq!(file_path(src), want, "{src}");
        }
    }
}
