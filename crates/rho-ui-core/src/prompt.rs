//! What the user sends: a message's text and what is attached to it,
//! images and files. A prompt goes to the daemon as rho's terminal UI sends
//! one: its text as one block, then each attachment in order.
//!
//! A file goes whole when it is text, as a resource; any other file, or one
//! too long to send, goes as a link to its path, which the agent reads with
//! its own tools. The daemon hands the model either as fenced text under
//! the file's URI, and echoes it that way, so [`Prompt::from_wire`] reads
//! those fences back as files.

use std::{path::Path, sync::Arc};

use base64::Engine as _;
use rho_protocol::ContentBlock;

/// The most a prompt's attachments can come to once encoded: the daemon
/// reads a message as one line of at most [`rho_protocol::MAX_LINE_BYTES`].
/// An image counts as its base64, 4 characters for every 3 bytes, and a
/// file as its text. The prompt's own text and the envelope share the line,
/// so attachments near this fail to send; past it, they never could.
pub const MAX_PROMPT_ATTACHMENT_BYTES: usize = rho_protocol::MAX_LINE_BYTES;

/// The longest text file sent whole, in characters: what rho's terminal UI
/// sends of a document it attaches (rho-tools' `MAX_EXTRACTED_CHARACTERS`).
/// A longer one goes as a link, which the agent reads a window at a time.
pub const MAX_ATTACHED_TEXT_CHARS: usize = 200_000;

/// Attachments that together pass [`MAX_PROMPT_ATTACHMENT_BYTES`], so no
/// one prompt can carry them. Holds their encoded total, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "a message can carry {} of attachments once encoded, and these come to {}",
    mebibytes(MAX_PROMPT_ATTACHMENT_BYTES),
    mebibytes(*.0)
)]
pub struct AttachmentsTooLarge(usize);

/// Checks that one prompt can carry `attachments`.
///
/// # Errors
///
/// [`AttachmentsTooLarge`] when they pass [`MAX_PROMPT_ATTACHMENT_BYTES`].
pub fn check_attachment_total<'a>(
    attachments: impl IntoIterator<Item = &'a Attachment>,
) -> Result<(), AttachmentsTooLarge> {
    let total = attachments.into_iter().map(Attachment::wire_bytes).sum();
    if total > MAX_PROMPT_ATTACHMENT_BYTES {
        Err(AttachmentsTooLarge(total))
    } else {
        Ok(())
    }
}

/// `bytes` in MiB, to one decimal place, such as `12.0 MiB`.
pub(crate) fn mebibytes(bytes: usize) -> String {
    let tenths = bytes.saturating_mul(10) / (1024 * 1024);
    format!("{}.{} MiB", tenths / 10, tenths % 10)
}

/// An image format the daemon takes in a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// `image/png`.
    Png,
    /// `image/jpeg`.
    Jpeg,
    /// `image/gif`.
    Gif,
    /// `image/webp`.
    Webp,
}

impl ImageFormat {
    /// The MIME type the wire names it by, such as `image/png`.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    /// Its short name, such as `PNG`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Webp => "WebP",
        }
    }

    fn from_mime_type(mime_type: &str) -> Option<Self> {
        [Self::Png, Self::Jpeg, Self::Gif, Self::Webp]
            .into_iter()
            .find(|format| format.mime_type().eq_ignore_ascii_case(mime_type))
    }
}

/// An image in a prompt or a user message: its encoded bytes, as a file of
/// its format holds them. Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    format: ImageFormat,
    bytes: Arc<[u8]>,
}

impl Image {
    /// An image of `format` whose file is `bytes`. The daemon checks that
    /// they are one, and refuses a prompt whose image is not.
    pub fn new(format: ImageFormat, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            format,
            bytes: bytes.into(),
        }
    }

    /// Its format.
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// Its file's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The image a content block carries, if it is one this app can show:
    /// base64 of a format rho takes.
    fn from_wire(mime_type: &str, data: &str) -> Option<Self> {
        let format = ImageFormat::from_mime_type(mime_type)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        Some(Self::new(format, bytes))
    }

    fn to_wire(&self) -> ContentBlock {
        ContentBlock::Image {
            mime_type: self.format.mime_type().to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(&self.bytes),
        }
    }
}

/// A file attached to a prompt: its text, or, for a file that isn't text
/// or is too long to send, only where it is. Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedFile {
    /// `file://` and the path, for a file on this machine.
    uri: String,
    /// `None` for a link: the agent reads the file itself.
    text: Option<Arc<str>>,
}

impl AttachedFile {
    /// The file at `path`, an absolute one, sent whole as `text`.
    pub fn with_text(path: &Path, text: impl Into<Arc<str>>) -> Self {
        Self {
            uri: file_uri(path),
            text: Some(text.into()),
        }
    }

    /// The file at `path`, an absolute one, sent as a link to it.
    pub fn linked(path: &Path) -> Self {
        Self {
            uri: file_uri(path),
            text: None,
        }
    }

    /// Where it is, such as `file:///home/me/notes.md`.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Its path, for a file on this machine.
    pub fn path(&self) -> Option<&Path> {
        self.uri.strip_prefix("file://").map(Path::new)
    }

    /// Its name: the last part of its path.
    pub fn name(&self) -> &str {
        self.uri
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.uri)
    }

    /// Its text when it goes whole; `None` when it goes as a link.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The file a resource fence the daemon made holds: `body` under `uri`.
    fn from_fence(uri: &str, body: &str) -> Self {
        let mut file = Self {
            uri: uri.to_owned(),
            text: None,
        };
        if body != file.link_note() {
            file.text = Some(body.into());
        }
        file
    }

    /// What the model reads for a link, in place of the file.
    fn link_note(&self) -> String {
        let place = self
            .path()
            .map_or_else(|| self.uri.clone(), |path| path.display().to_string());
        format!("The user attached this file; its contents were not sent. Read it at {place}.")
    }

    fn to_wire(&self) -> ContentBlock {
        match &self.text {
            Some(text) => ContentBlock::Resource {
                uri: self.uri.clone(),
                text: text.to_string(),
            },
            None => ContentBlock::ResourceLink {
                uri: self.uri.clone(),
                name: self.name().to_owned(),
                description: Some(self.link_note()),
            },
        }
    }
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Something attached to a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attachment {
    /// An image the model sees.
    Image(Image),
    /// Any other file.
    File(AttachedFile),
}

impl Attachment {
    /// How many bytes it adds to the message that carries it.
    fn wire_bytes(&self) -> usize {
        match self {
            Self::Image(image) => image.bytes.len().div_ceil(3) * 4,
            Self::File(file) => match &file.text {
                Some(text) => text.len(),
                None => file.uri.len() + file.link_note().len(),
            },
        }
    }

    fn to_wire(&self) -> ContentBlock {
        match self {
            Self::Image(image) => image.to_wire(),
            Self::File(file) => file.to_wire(),
        }
    }
}

/// A message for the daemon: text, attachments, or both.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    text: String,
    attachments: Vec<Attachment>,
}

impl Prompt {
    /// `text` with `attachments` after it.
    pub fn new(text: String, attachments: Vec<Attachment>) -> Self {
        Self { text, attachments }
    }

    /// Its text, which may be empty.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Its attachments, in the order they were added.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// Whether it has nothing to send: no text but whitespace, and nothing
    /// attached.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    /// Its text and attachments.
    pub fn into_parts(self) -> (String, Vec<Attachment>) {
        (self.text, self.attachments)
    }

    /// Adds `later` after this: its text on a new line, its attachments
    /// after these.
    pub(crate) fn append(&mut self, later: Self) {
        if self.text.is_empty() {
            self.text = later.text;
        } else if !later.text.is_empty() {
            self.text.push('\n');
            self.text.push_str(&later.text);
        }
        self.attachments.extend(later.attachments);
    }

    /// The message a user-message event's `content` holds. A file comes
    /// back as the daemon echoes it, the fenced text it made of the file
    /// for the model, or as it was sent; an image this app can't read
    /// stays in the text as a placeholder.
    pub(crate) fn from_wire(content: &[ContentBlock]) -> Self {
        let mut prompt = Self::default();
        for block in content {
            let attachment = match block {
                ContentBlock::Image { mime_type, data } => {
                    Image::from_wire(mime_type, data).map(Attachment::Image)
                }
                ContentBlock::Text { text } => resource_fence(text)
                    .map(|(uri, body)| Attachment::File(AttachedFile::from_fence(uri, body))),
                ContentBlock::Resource { uri, text } => Some(Attachment::File(AttachedFile {
                    uri: uri.clone(),
                    text: Some(text.as_str().into()),
                })),
                ContentBlock::ResourceLink { uri, .. } => Some(Attachment::File(AttachedFile {
                    uri: uri.clone(),
                    text: None,
                })),
                _ => None,
            };
            match attachment {
                Some(attachment) => prompt.attachments.push(attachment),
                None => prompt.text.push_str(&block_text(block)),
            }
        }
        prompt
    }

    /// The content blocks of `rho/session/prompt`: the text, unless empty,
    /// then each attachment, as rho's terminal UI sends them.
    pub(crate) fn to_wire(&self) -> Vec<ContentBlock> {
        let text = (!self.text.is_empty()).then(|| ContentBlock::Text {
            text: self.text.clone(),
        });
        text.into_iter()
            .chain(self.attachments.iter().map(Attachment::to_wire))
            .collect()
    }
}

/// The URI and body of `text` if all of it is a resource fence, as the
/// daemon writes one for a resource or a resource link: a fence of three
/// or more backticks, `resource` and the URI, the body, and the fence.
fn resource_fence(text: &str) -> Option<(&str, &str)> {
    let ticks = text.bytes().take_while(|&byte| byte == b'`').count();
    if ticks < 3 {
        return None;
    }
    let (fence, rest) = text.split_at(ticks);
    let (uri, rest) = rest.strip_prefix("resource ")?.split_once('\n')?;
    let body = rest.strip_suffix(fence)?.strip_suffix('\n')?;
    Some((uri, body))
}

/// The text of a block that isn't an attachment: an image becomes a
/// placeholder, and a block a newer daemon sends, nothing.
fn block_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::Image { .. } => "[image]".to_owned(),
        _ => String::new(),
    }
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Self::new(text, Vec::new())
    }
}

impl From<&str> for Prompt {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}

/// A path for tests, absolute on this platform.
#[cfg(test)]
pub(crate) fn test_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Attachment {
        Attachment::Image(Image::new(
            ImageFormat::Png,
            b"\x89PNG\r\n\x1a\nfake".to_vec(),
        ))
    }

    /// What the daemon makes of a resource for the model, and echoes:
    /// rho's `fence_context`.
    fn fenced(uri: &str, body: &str) -> ContentBlock {
        ContentBlock::Text {
            text: format!("```resource {uri}\n{body}\n```"),
        }
    }

    #[test]
    fn a_prompt_sends_its_text_then_its_attachments() {
        let notes = AttachedFile::with_text(&test_path("notes.md"), "# Notes");
        let report = AttachedFile::linked(&test_path("report.pdf"));
        let prompt = Prompt::new(
            "look".to_owned(),
            vec![
                png(),
                Attachment::File(notes.clone()),
                Attachment::File(report.clone()),
            ],
        );
        let wire = prompt.to_wire();
        assert_eq!(
            wire[0],
            ContentBlock::Text {
                text: "look".to_owned()
            }
        );
        assert!(
            matches!(&wire[1], ContentBlock::Image { mime_type, .. } if mime_type == "image/png"),
            "the image follows the text: {wire:?}"
        );
        assert_eq!(
            wire[2],
            ContentBlock::Resource {
                uri: notes.uri().to_owned(),
                text: "# Notes".to_owned()
            }
        );
        let ContentBlock::ResourceLink {
            name, description, ..
        } = &wire[3]
        else {
            panic!("a file that isn't sent goes as a link: {wire:?}");
        };
        assert_eq!(name, "report.pdf");
        let path = test_path("report.pdf");
        assert!(
            description
                .as_deref()
                .is_some_and(|note| note.contains(&path.display().to_string())),
            "the link says where to read it: {description:?}"
        );

        // The daemon echoes the files as fences, which read back as files,
        // as do the blocks sent.
        let note = description.clone().unwrap_or_default();
        let echo = [
            wire[0].clone(),
            wire[1].clone(),
            fenced(notes.uri(), "# Notes"),
            fenced(report.uri(), &note),
        ];
        assert_eq!(Prompt::from_wire(&echo), prompt);
        assert_eq!(Prompt::from_wire(&wire), prompt);
    }

    #[test]
    fn a_files_name_and_path_come_from_its_uri() {
        let path = test_path("notes.md");
        let file = AttachedFile::linked(&path);
        assert_eq!(file.name(), "notes.md");
        assert_eq!(file.path(), Some(path.as_path()));
        assert_eq!(file.text(), None);
    }

    #[test]
    fn an_attachment_only_prompt_sends_no_empty_text() {
        let wire = Prompt::new(String::new(), vec![png()]).to_wire();
        assert!(
            matches!(wire.as_slice(), [ContentBlock::Image { .. }]),
            "{wire:?}"
        );
        assert!(!Prompt::new(String::new(), vec![png()]).is_empty());
        assert!(Prompt::from("  \n").is_empty());
    }

    /// An image the app can't read stays in the text as a placeholder, in
    /// its place, as does text that only looks like a fence.
    #[test]
    fn an_echo_keeps_what_it_cannot_show_as_text() {
        let wire: Vec<ContentBlock> = serde_json::from_value(serde_json::json!([
            {"type": "text", "text": "what is this?"},
            {"type": "image", "mime_type": "image/png", "data": "aGk="},
            {"type": "image", "mime_type": "image/bmp", "data": "aGk="},
            {"type": "text", "text": "```resource x\nunclosed"},
            {"type": "image", "mime_type": "image/jpeg", "data": "aGk="},
        ]))
        .expect("valid content blocks");
        let prompt = Prompt::from_wire(&wire);
        assert_eq!(prompt.text(), "what is this?[image]```resource x\nunclosed");
        let formats: Vec<_> = prompt
            .attachments()
            .iter()
            .filter_map(|attachment| match attachment {
                Attachment::Image(image) => Some(image.format()),
                Attachment::File(_) => None,
            })
            .collect();
        assert_eq!(formats, [ImageFormat::Png, ImageFormat::Jpeg]);
    }

    /// A body that holds its own fence gets a longer one, which still reads
    /// back whole.
    #[test]
    fn a_longer_fence_reads_back_whole() {
        let block = ContentBlock::Text {
            text: "````resource file:///a.md\n```rust\nfn main() {}\n```\n````".to_owned(),
        };
        let prompt = Prompt::from_wire(&[block]);
        let [Attachment::File(file)] = prompt.attachments() else {
            panic!("one file: {prompt:?}");
        };
        assert_eq!(file.text(), Some("```rust\nfn main() {}\n```"));
    }

    #[test]
    fn appending_joins_text_by_line_and_keeps_attachment_order() {
        let mut first = Prompt::new("one".to_owned(), vec![png()]);
        first.append(Prompt::new(String::new(), vec![png(), png()]));
        first.append(Prompt::from("two"));
        assert_eq!(first.text(), "one\ntwo");
        assert_eq!(first.attachments().len(), 3);

        let mut empty = Prompt::default();
        empty.append(Prompt::from("only"));
        assert_eq!(empty.text(), "only");
    }

    #[test]
    fn attachments_past_one_message_are_refused_with_the_limit() {
        // 6 MiB of image is 8 MiB encoded.
        let image = Attachment::Image(Image::new(ImageFormat::Png, vec![0; 6 * 1024 * 1024]));
        assert_eq!(check_attachment_total([&image, &image]), Ok(()));
        let text = Attachment::File(AttachedFile::with_text(&test_path("a"), "x"));
        let error =
            check_attachment_total([&image, &image, &text]).expect_err("a byte past the limit");
        assert_eq!(
            error.to_string(),
            "a message can carry 16.0 MiB of attachments once encoded, and these come to 16.0 MiB"
        );
    }

    #[test]
    fn an_image_rho_does_not_take_is_not_read() {
        assert_eq!(Image::from_wire("image/bmp", "aGk="), None);
        assert_eq!(Image::from_wire("image/png", "not base64!"), None);
        assert_eq!(
            Image::from_wire("IMAGE/PNG", "aGk=").map(|image| image.format()),
            Some(ImageFormat::Png)
        );
    }
}
