//! Images a text shows on their own: a paragraph of only `![alt](plot.png)`
//! images or `<img>` tags, or a block of `<img>` tags, each drawn as a
//! picture in the text's measure, at its own size (an `<img>`'s `width`,
//! when it says) or scaled down to fit. Clicking one opens it in the
//! [`lightbox`](crate::lightbox).
//!
//! A session's messages show pictures from files ([`Pictures::files`]): a
//! path is read on the session's machine, from the session's folder when
//! relative, so a remote session's pictures come over its ssh link. The
//! Pull request surface shows them from the web ([`Pictures::web`]),
//! fetched on the machine that reads the PR (see
//! [`forge::picture`](crate::forge::picture)). A picture that can't be
//! shown says so in its place, with its address and why; one that
//! couldn't be read, as a file not written yet, is read again when a turn
//! ends or it is clicked. `data:` images are GPUI Kit's own, as are images
//! inside a line of text, which Kit shows only from a `data:` URL.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::text::{
    markdown_ast as mdast, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::component::{
    h_flex, link::Link, spinner::Spinner, v_flex, ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, App, BackgroundExecutor, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
};

use crate::{
    attachments::ImagePreview,
    folders::{FileRead, Reader},
    forge::picture::{self as web, FetchError, Forge},
    lightbox,
    machines::{MachineId, Place},
    workspaces::Workspaces,
};

/// The largest image file read, in bytes: what rho's own `read_file`
/// previews (rho's `docs/tools-workspace/documents-and-images.md`).
const FILE_LIMIT: usize = 32 << 20;

/// The most different pictures one view fetches from the web: a PR's
/// text, which anyone who can comment on it writes. Of rho's and rho-ui's
/// 200 newest PRs, 105 show images, at most 14 different ones (a review
/// bot's icons, mostly). Those past it say so in their place.
const WEB_LIMIT: usize = 32;

/// The most bytes one view's web pictures may take decoded, as GPUI keeps
/// every frame of an animated GIF or WebP: ten demo GIFs in cli/cli's,
/// Zed's and VS Code's PRs took 10 to 841 MiB each, so the largest fits
/// with the stills around it, and a crafted one of thousands of large
/// frames doesn't take the machine's memory.
const WEB_DECODED: u64 = 1 << 30;

/// The longest side of an image shown, in pixels: what rho's own
/// `read_file` decodes (the same doc). A larger one can take hundreds of
/// MiB decoded, and pass the GPU's largest texture, which draws nothing.
/// Attached and tool-result images keep to it too; see
/// [`crate::attachments::Preview::of_image`].
pub(crate) const MAX_SIDE: u32 = 4096;

/// The tallest a picture shows in a message, in rems: 384 px at 100%, so a
/// tall screenshot leaves the text around it in view.
const MAX_HEIGHT_REMS: f32 = 24.;

/// One text's picture: the text's entry, and its address as written.
/// Each message reads its own, so a file rewritten between replies shows
/// as it was when each reply named it; the Pull request surface's texts
/// are all entry 0, so a PR fetches an image once.
type Key = (usize, String);

/// The pictures one view's texts show, read once each.
pub(crate) struct Pictures {
    source: Source,
    workspaces: Entity<Workspaces>,
    loads: HashMap<Key, Load>,
    /// Reads waiting their turn, with the path or address each reads. One
    /// runs at a time: on a remote machine each is an ssh session on the
    /// shared connection, which runs only a few scripts at once, and the
    /// Files surface and git need some too.
    queue: VecDeque<(Key, String)>,
    reading: bool,
    /// Bumped by [`Self::clear`], so a read started before it lands
    /// nowhere.
    epoch: u64,
}

/// How [`Pictures::read_next`] reads the next picture.
enum Job {
    /// From a file, from this folder when relative.
    File(PathBuf),
    /// From the web, for this forge, decoded up to this many bytes.
    Web(Forge, u64),
}

/// Where a view's pictures come from.
enum Source {
    /// Files in a session's folder, on its machine.
    Files(Place),
    /// The web, fetched on its machine.
    Web(Web),
}

/// What a view of web pictures keeps besides its pictures.
struct Web {
    machine: MachineId,
    /// Its PR's forge, once [`Pictures::set_site`] says: until then none
    /// is fetched, nor counted.
    site: Option<Site>,
    /// The pictures let through to be fetched, up to [`WEB_LIMIT`]: one
    /// read again stays let through.
    admitted: HashSet<Key>,
    /// The bytes those read take decoded, up to [`WEB_DECODED`].
    decoded: u64,
}

/// The forge whose PR's pictures a view fetches: its host, and how its
/// machine's `gh` signs in there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) host: String,
    pub(crate) signing: Signing,
}

/// Whether a forge's pictures are fetched signed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Signing {
    /// The machine's `gh` is being checked: fetches wait for it, as one
    /// sent without a token would fail for a private repository.
    Checking,
    /// With this `gh`, signed in to the host.
    With(PathBuf),
    /// Without: only pictures anyone may see load.
    Without,
}

/// Where one picture is.
#[derive(Clone)]
enum Load {
    Reading,
    Read(ImagePreview),
    /// It can't be shown, and why: it isn't an image rho-ui shows, or
    /// its address is of another kind.
    Failed(SharedString),
    /// It couldn't be read, and why: it isn't there yet, or the machine
    /// or the web didn't answer. Read again when a turn ends, or when
    /// clicked.
    Unread(SharedString),
}

/// A read of a picture of text `entry` ended or starts over, which
/// changes the text's height.
pub(crate) struct PictureRead {
    pub(crate) entry: usize,
}

impl EventEmitter<PictureRead> for Pictures {}

impl Pictures {
    /// Pictures from files in `place`, a session's folder.
    pub(crate) fn files(place: Place, workspaces: Entity<Workspaces>) -> Self {
        Self::new(Source::Files(place), workspaces)
    }

    /// Pictures from the web, fetched on `machine`. They wait for
    /// [`Self::set_site`].
    pub(crate) fn web(machine: MachineId, workspaces: Entity<Workspaces>) -> Self {
        let web = Web {
            machine,
            site: None,
            admitted: HashSet::new(),
            decoded: 0,
        };
        Self::new(Source::Web(web), workspaces)
    }

    fn new(source: Source, workspaces: Entity<Workspaces>) -> Self {
        Self {
            source,
            workspaces,
            loads: HashMap::new(),
            queue: VecDeque::new(),
            reading: false,
            epoch: 0,
        }
    }

    /// Fetches web pictures for the forge `site` names, from the next
    /// fetch on.
    pub(crate) fn set_site(&mut self, site: Site, cx: &mut Context<'_, Self>) {
        let Source::Web(web) = &mut self.source else {
            return;
        };
        if web.site.as_ref() != Some(&site) {
            if web.site.is_none() {
                // Its texts ask for their pictures again.
                cx.emit(PictureRead { entry: 0 });
            }
            web.site = Some(site);
            self.read_next(cx);
        }
    }

    /// Where the picture at `src` in text `entry` is, queueing a read of
    /// it the first time. [`PictureRead`] says when the read ends.
    fn load(&mut self, entry: usize, src: &str, cx: &mut Context<'_, Self>) -> Load {
        let key = (entry, src.to_owned());
        if let Some(load) = self.loads.get(&key) {
            return load.clone();
        }
        let located = match &mut self.source {
            Source::Files(_) => file_path(src),
            // Asked for again once it is.
            Source::Web(Web { site: None, .. }) => return Load::Reading,
            Source::Web(web) => web.admit(&key, src),
        };
        let load = match located {
            Ok(at) => {
                self.queue.push_back((key.clone(), at));
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

    /// Reads the picture at `src` in text `entry` again.
    fn retry_one(&mut self, entry: usize, src: &str, cx: &mut Context<'_, Self>) {
        if self.loads.remove(&(entry, src.to_owned())).is_some() {
            cx.emit(PictureRead { entry });
        }
    }

    /// Forgets the pictures of texts from `entry` on, which went or
    /// moved.
    pub(crate) fn forget_from(&mut self, entry: usize) {
        self.loads.retain(|(at, _), _| *at < entry);
        self.queue.retain(|((at, _), _)| *at < entry);
    }

    /// Forgets every picture, as the texts that showed them went, and the
    /// read under way with them: the next starts at once. Frees what GPUI
    /// decoded of them.
    pub(crate) fn clear(&mut self, cx: &mut App) {
        for load in self.loads.values() {
            if let Load::Read(preview) = load {
                preview.image().remove_asset(cx);
            }
        }
        self.loads.clear();
        self.queue.clear();
        if let Source::Web(web) = &mut self.source {
            web.admitted.clear();
            web.decoded = 0;
        }
        self.reading = false;
        self.epoch += 1;
    }

    /// Starts the next queued read, unless one runs or web pictures wait
    /// for their sign-in.
    fn read_next(&mut self, cx: &mut Context<'_, Self>) {
        if self.reading {
            return;
        }
        let (machine, job) = match &self.source {
            Source::Files(place) => (&place.machine, Job::File(place.path.clone())),
            Source::Web(web) => match web.forge() {
                Some(forge) => (&web.machine, Job::Web(forge, WEB_DECODED - web.decoded)),
                None => return,
            },
        };
        let reader = self.workspaces.read(cx).reader(machine);
        let Some((key, at)) = self.queue.pop_front() else {
            return;
        };
        self.reading = true;
        let epoch = self.epoch;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let load = match job {
                Job::File(folder) => (read_file(reader, &folder, &at, &executor).await, 0),
                Job::Web(forge, most) => fetch(reader, &at, &forge, most, &executor).await,
            };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.reading = false;
                // A picture forgotten while it was read: its text went.
                if let Some(slot) = this.loads.get_mut(&key) {
                    let (load, decoded) = load;
                    if let Source::Web(web) = &mut this.source {
                        web.decoded += decoded;
                    }
                    *slot = load;
                    cx.emit(PictureRead { entry: key.0 });
                }
                this.read_next(cx);
            });
        })
        .detach();
    }
}

/// The picture in the file at `path`, from `folder` when relative, read
/// through `reader`.
async fn read_file(
    reader: Reader,
    folder: &Path,
    path: &str,
    executor: &BackgroundExecutor,
) -> Load {
    match reader.read_file(folder, path, FILE_LIMIT, executor).await {
        Ok(FileRead::Bytes(bytes)) => preview(bytes, executor).await,
        Ok(FileRead::TooLarge) => {
            Load::Failed(format!("It's over {} MiB.", FILE_LIMIT >> 20).into())
        }
        Ok(FileRead::Gone) => Load::Unread("There's no file there.".into()),
        Err(why) => Load::Unread(why.into()),
    }
}

/// The picture at web address `url`, fetched through `reader` for
/// `forge`, with the bytes it takes decoded: none when that is over
/// `most`.
async fn fetch(
    reader: Reader,
    url: &str,
    forge: &Forge,
    most: u64,
    executor: &BackgroundExecutor,
) -> (Load, u64) {
    let bytes = match web::fetch(reader, url, forge, executor).await {
        Ok(bytes) => bytes,
        Err(FetchError::Refused(why)) => return (Load::Failed(why.into()), 0),
        Err(FetchError::Failed(why)) => {
            return (Load::Unread(format!("Couldn't load it: {why}").into()), 0)
        }
    };
    let decoded = executor
        .spawn(async move {
            let preview = ImagePreview::from_file(bytes)?;
            let (width, height) = preview.pixels();
            let frame = u64::from(width) * u64::from(height) * 4;
            let frames = frames(preview.image().bytes(), most / frame.max(1) + 1);
            Ok::<_, String>((preview, frame * frames))
        })
        .await;
    match decoded {
        Ok((preview, decoded)) if decoded <= most => (Load::Read(preview), decoded),
        Ok(_) => (
            Load::Failed(
                format!(
                    "rho-ui shows up to {} MiB of a pull request's images decoded, and this would pass it.",
                    WEB_DECODED >> 20
                )
                .into(),
            ),
            0,
        ),
        Err(why) => (Load::Failed(why.into()), 0),
    }
}

/// How many frames GPUI decodes the image `bytes` to, as it keeps every
/// frame of a GIF or WebP, counted up to `most`.
fn frames(bytes: &[u8], most: u64) -> u64 {
    let cursor = std::io::Cursor::new(bytes);
    let frames = if bytes.starts_with(b"GIF8") {
        image::codecs::gif::GifDecoder::new(cursor)
            .ok()
            .map(image::AnimationDecoder::into_frames)
    } else if bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        image::codecs::webp::WebPDecoder::new(cursor)
            .ok()
            .filter(image::codecs::webp::WebPDecoder::has_animation)
            .map(image::AnimationDecoder::into_frames)
    } else {
        None
    };
    let most = usize::try_from(most).unwrap_or(usize::MAX);
    frames
        .map_or(1, |frames| frames.take(most).count())
        .max(1)
        .try_into()
        .unwrap_or(u64::MAX)
}

/// `bytes` read as the image they are, off the UI thread.
async fn preview(bytes: Vec<u8>, executor: &BackgroundExecutor) -> Load {
    executor
        .spawn(async move { ImagePreview::from_file(bytes) })
        .await
        .map_or_else(|why| Load::Failed(why.into()), Load::Read)
}

impl Web {
    /// The forge to fetch for, unless its `gh` is still being checked.
    fn forge(&self) -> Option<Forge> {
        let site = self.site.as_ref()?;
        let gh = match &site.signing {
            Signing::Checking => return None,
            Signing::With(gh) => Some(gh.clone()),
            Signing::Without => None,
        };
        Some(Forge {
            host: site.host.clone(),
            gh,
        })
    }

    /// The address to fetch picture `key`, at `src`, from, letting it
    /// through if it is on a host fetched from and under [`WEB_LIMIT`].
    fn admit(&mut self, key: &Key, src: &str) -> Result<String, String> {
        let url = web_address(src)?;
        let host = self.site.as_ref().map_or("", |site| site.host.as_str());
        web::allowed(&url, host).map_err(|why| why.to_string())?;
        if self.admitted.contains(key)
            || self.admitted.len() < WEB_LIMIT && self.admitted.insert(key.clone())
        {
            Ok(url)
        } else {
            Err(format!(
                "rho-ui loads the first {WEB_LIMIT} different images of a pull request."
            ))
        }
    }
}

/// The web address a picture's `src` names: itself, if it is an http or
/// https URL. A path, which a forge reads against the repository's files
/// on one branch or another, isn't fetched.
///
/// # Errors
///
/// Says why when `src` is no web address.
fn web_address(src: &str) -> Result<String, String> {
    match scheme(src) {
        Some(scheme)
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") =>
        {
            Ok(src.to_owned())
        }
        Some(scheme) => Err(format!(
            "rho-ui shows images from web links here, not {scheme}: links."
        )),
        None => Err("rho-ui shows images from web links here, not from paths.".to_owned()),
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

/// One picture a text shows.
#[derive(Debug, PartialEq)]
struct Wanted {
    src: String,
    alt: SharedString,
    /// The widest it shows, in pixels, as an `<img>`'s `width` says: an
    /// image GitHub's site uploads has its width over the screen's scale,
    /// so a screenshot from a 2x screen shows at the size it was on it.
    width: Option<f32>,
}

impl Wanted {
    /// What names it: its alt text, or its address without one.
    fn name(&self) -> SharedString {
        if self.alt.is_empty() {
            self.src.clone().into()
        } else {
            self.alt.clone()
        }
    }
}

/// The pictures `html` holds when it is nothing but `<img>` tags, as
/// GitHub writes an upload (`<img width="1262" alt="image" src="…" />`),
/// and none is Kit's own `data:` image.
fn img_tags(html: &str) -> Option<Vec<Wanted>> {
    let mut rest = html.trim_start();
    let mut wanted = Vec::new();
    while !rest.is_empty() {
        let tag = rest.strip_prefix('<')?;
        let name = tag
            .get(..3)
            .filter(|name| name.eq_ignore_ascii_case("img"))?;
        let (attributes, after) = attributes(&tag[name.len()..])?;
        let attribute = |key: &str| {
            attributes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_str())
        };
        let src = attribute("src").filter(|src| !src.is_empty() && !src.starts_with("data:"))?;
        wanted.push(Wanted {
            src: src.to_owned(),
            alt: attribute("alt").unwrap_or_default().to_owned().into(),
            width: attribute("width")
                .map(|width| width.strip_suffix("px").unwrap_or(width))
                .and_then(|width| width.parse::<f32>().ok())
                .filter(|width| width.is_finite() && *width > 0.),
        });
        rest = after.trim_start();
    }
    (!wanted.is_empty()).then_some(wanted)
}

/// The attributes of the tag `tag` starts inside of, after its name, with
/// what follows its `>`: `None` if it isn't a whole tag. A value's
/// character references are decoded.
fn attributes(tag: &str) -> Option<(Vec<(&str, String)>, &str)> {
    // The name ends here: `<imgx` is another tag.
    if !tag.starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>') {
        return None;
    }
    let mut attributes = Vec::new();
    let mut rest = tag;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        if let Some(after) = rest.strip_prefix('>') {
            return Some((attributes, after));
        }
        let end = rest.find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '>' | '/'))?;
        let (name, after) = rest.split_at(end);
        if name.is_empty() {
            return None;
        }
        rest = after.trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            let (value, after) = if quote == '"' || quote == '\'' {
                let close = after[1..].find(quote)? + 1;
                (&after[1..close], &after[close + 1..])
            } else {
                let end = after
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .unwrap_or(after.len());
                after.split_at(end)
            };
            rest = after;
            decoded(value)
        } else {
            String::new()
        };
        attributes.push((name, value));
    }
}

/// `value` with the character references a URL or alt text is likely to
/// hold decoded: `&amp;` between a query's parameters, and quotes. Other
/// numeric references stay as written.
fn decoded(value: &str) -> String {
    const REFERENCES: [(&str, &str); 6] = [
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        // Last, so `&amp;lt;` is `&lt;`.
        ("&amp;", "&"),
    ];
    REFERENCES
        .into_iter()
        .fold(value.to_owned(), |value, (reference, character)| {
            value.replace(reference, character)
        })
}

/// The Markdown plugin that draws a text's pictures, of text `entry`,
/// read by `pictures`.
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
        let wanted = match node {
            mdast::Node::Paragraph(paragraph) => {
                let mut wanted = Vec::new();
                for child in &paragraph.children {
                    match child {
                        mdast::Node::Image(image) if !image.url.starts_with("data:") => {
                            wanted.push(Wanted {
                                src: image.url.clone(),
                                alt: image.alt.clone().into(),
                                width: None,
                            });
                        }
                        // Each tag in a paragraph is a node of its own.
                        mdast::Node::Html(html) => wanted.extend(img_tags(&html.value)?),
                        mdast::Node::Text(text) if text.value.trim().is_empty() => {}
                        mdast::Node::Break(_) => {}
                        _ => return None,
                    }
                }
                wanted
            }
            mdast::Node::Html(html) => img_tags(&html.value)?,
            _ => return None,
        };
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

/// The card a picture that can't be shown shows in its place: `wanted`'s
/// name, its address (a link, when it's a web one, so it can still be
/// seen in the browser) and `why`.
fn failed(wanted: &Wanted, why: SharedString, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let opens = web_address(&wanted.src).is_ok();
    let said: SharedString = if opens {
        why
    } else {
        format!("{} · {why}", wanted.src).into()
    };
    let detail = || div().text_xs().text_color(theme.muted_foreground);
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
            v_flex()
                .min_w_0()
                .child(wanted.name())
                .when(opens, |this| {
                    this.child(
                        div().id("picture-link").test_support().child(
                            Link::new("picture-address")
                                .href(wanted.src.clone())
                                // Not a click on the card, which
                                // reads it again.
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .text_xs()
                                .child(wanted.src.clone()),
                        ),
                    )
                })
                .child(detail().child(said)),
        )
}

impl Plugin {
    /// Picture `wanted` as `load` has it, in a row of the text's width
    /// named `id`. The picture in it is `picture`, or while it is read
    /// `picture-loading`, or `picture-failed` with why, and a web
    /// picture's address as `picture-link`; one that couldn't be read is
    /// in `picture-retry`, a click on which reads it again.
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
                let widest = px(wanted
                    .width
                    .map_or(width, |most| width.min(most))
                    .min(tallest * aspect));
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
            Load::Failed(why) => row.child(failed(wanted, why, cx)).into_any_element(),
            Load::Unread(why) => {
                let (pictures, entry, src) =
                    (self.pictures.clone(), self.entry, wanted.src.clone());
                row.child(
                    div()
                        .id("picture-retry")
                        .test_support()
                        .w_full()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            pictures.update(cx, |pictures, cx| pictures.retry_one(entry, &src, cx));
                        })
                        .child(failed(
                            wanted,
                            format!("{why} Click to try again.").into(),
                            cx,
                        )),
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

    /// GPUI keeps every frame of an animated GIF, so each counts, up to
    /// the most asked; a still image is one.
    #[test]
    fn every_frame_of_a_gif_counts() {
        use image::{codecs::gif::GifEncoder, Frame, RgbaImage};

        let mut gif = Vec::new();
        GifEncoder::new(&mut gif)
            .encode_frames((0..5).map(|_| Frame::new(RgbaImage::new(4, 4))))
            .unwrap();
        let still = crate::attachments::png(4, 4, [0, 0, 0, 255]);
        assert_eq!(
            (frames(&gif, 100), frames(&gif, 3), frames(&still, 100)),
            (5, 3, 1)
        );
    }

    /// HTML of nothing but `<img>` tags, as GitHub's site writes an
    /// upload, names its pictures, their alt text and width; HTML of
    /// anything else, or Kit's own `data:` images, names none.
    #[test]
    fn img_tags_name_pictures() {
        let picture = |src: &str, alt: &str, width: Option<f32>| Wanted {
            src: src.to_owned(),
            alt: alt.to_owned().into(),
            width,
        };
        let cases = [
            (
                r#"<img width="1262" height="636" alt="image" src="https://github.com/user-attachments/assets/a1" />"#,
                Some(vec![picture(
                    "https://github.com/user-attachments/assets/a1",
                    "image",
                    Some(1262.),
                )]),
            ),
            (
                "<IMG SRC='a.png?x=1&amp;y=2' width=50% alt=\"&quot;A&quot;\">\n<img src=b.png width=40px>",
                Some(vec![
                    picture("a.png?x=1&y=2", "\"A\"", None),
                    picture("b.png", "", Some(40.)),
                ]),
            ),
            ("<img alt=\"no source\">", None),
            ("<img src=\"data:image/png;base64,iVBORw0KGgo=\">", None),
            ("<imgx src=\"a.png\">", None),
            ("<img src=\"a.png\"> and text", None),
            ("<p>text</p>", None),
            ("<img src=\"a.png", None),
        ];
        for (html, want) in cases {
            assert_eq!(img_tags(html), want, "{html}");
        }
    }
}
