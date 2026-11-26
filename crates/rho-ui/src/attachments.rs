//! Attachments as the composer and the transcript draw them, as GPUI Kit
//! attachment cards: a file as a chip with an icon for its kind, and an
//! image as a square tile as tall as a chip, or in the transcript, larger
//! and in its own proportions. An image with a side over [`MAX_SIDE`] px
//! shows as a chip saying it is too large to preview, and is sent all the
//! same. Also reads what the user attaches, from the clipboard or from
//! disk.

use std::{ffi::OsStr, fs, io::Cursor, path::Path, sync::Arc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::attachment::{
    Attachment as Card, AttachmentContent, AttachmentDescription, AttachmentMedia, AttachmentTitle,
};
use gpui_kit::component::Icon;
use gpui_kit::{rems, App, Axis, ParentElement as _, SharedString, Styled as _};
use rho_ui_core::{
    mebibytes, AttachedFile, Attachment, Image, ImageFormat, MAX_ATTACHED_TEXT_CHARS,
    MAX_PROMPT_ATTACHMENT_BYTES,
};

use crate::pictures::MAX_SIDE;

/// The narrowest and widest a tile gets, as width over height: a phone's
/// 9:19.5 screenshot (0.46) and a 32:9 ultrawide one (3.56) both show
/// whole, and a thinner image is cropped to fit.
const ASPECT_RANGE: (f64, f64) = (0.25, 4.);

/// An attachment ready to draw.
#[derive(Clone, Debug)]
pub(crate) enum Preview {
    Image(ImagePreview),
    File {
        name: SharedString,
        /// How it goes: as text, and how long, or by its path.
        detail: SharedString,
        /// Its tooltip: where it is, or why an image shows as a chip.
        tooltip: SharedString,
        /// What kind of file it is, by its name.
        icon: Lucide,
    },
}

impl Preview {
    /// The preview of `attachment`; `None` for an image whose header
    /// doesn't read as its format.
    pub(crate) fn new(attachment: &Attachment) -> Option<Self> {
        match attachment {
            Attachment::Image(image) => Self::of_image(image),
            Attachment::File(file) => Some(Self::File {
                name: file.name().to_owned().into(),
                detail: match file.text() {
                    Some(text) => match text.lines().count() {
                        0 | 1 => "1 line".into(),
                        lines => format!("{lines} lines").into(),
                    },
                    None => "Sent by path".into(),
                },
                tooltip: file
                    .path()
                    .map_or_else(|| file.uri().to_owned(), |path| path.display().to_string())
                    .into(),
                icon: file_icon(file.name()),
            }),
        }
    }

    /// The preview of `image`: a tile, or for one with a side over
    /// [`MAX_SIDE`] px, a chip saying it is too large to preview. `None`
    /// when its header doesn't read as its format.
    pub(crate) fn of_image(image: &Image) -> Option<Self> {
        let (width, height) = dimensions(image.format(), image.bytes())?;
        if width.max(height) <= MAX_SIDE {
            return Some(Self::Image(ImagePreview::new(image, width, height)));
        }
        let format = image.format().name();
        Some(Self::File {
            name: "Too large to preview".into(),
            detail: format!("{format} image, {width} × {height} px (max {MAX_SIDE} px)").into(),
            tooltip: format!(
                "rho-ui previews images up to {MAX_SIDE} px a side; this one is sent all the same."
            )
            .into(),
            icon: Lucide::FileImage,
        })
    }

    /// Says why a file goes by its path, `why`, where it would only say
    /// that it does.
    pub(crate) fn linked_because(mut self, why: &str) -> Self {
        if let Self::File { detail, .. } = &mut self {
            *detail = format!("By path: {why}").into();
        }
        self
    }

    /// Says it holds only the first `bytes` of the text it stood for, as a
    /// resource cut to fit does.
    pub(crate) fn cut(mut self, bytes: usize) -> Self {
        if let Self::File { detail, .. } = &mut self {
            *detail = format!("{detail}, cut to {} KiB", bytes >> 10).into();
        }
        self
    }

    /// What it is, for screen readers, such as "PNG image, 1920 × 1080".
    pub(crate) fn label(&self) -> SharedString {
        match self {
            Self::Image(image) => image.label.clone(),
            Self::File { name, detail, .. } => format!("{name}, {detail}").into(),
        }
    }

    /// As a Kit attachment card in Kit's own geometry: an image as a
    /// square tile as tall as a file chip, cropped to fill it, and a file as
    /// a chip whose tooltip says where it is, or for an image too large to
    /// preview, why. Give a row of them one size, and it lines up.
    pub(crate) fn card(&self) -> Card {
        match self {
            Self::Image(image) => Card::new()
                .axis(Axis::Vertical)
                .media(AttachmentMedia::new().src(Arc::clone(&image.image))),
            Self::File {
                name,
                detail,
                tooltip,
                icon,
            } => Card::new()
                .tooltip(tooltip.clone())
                .media(AttachmentMedia::new().child(Icon::new(*icon)))
                .content(
                    AttachmentContent::new()
                        .title(AttachmentTitle::new(name.clone()))
                        .description(AttachmentDescription::new(detail.clone())),
                ),
        }
    }

    /// The image, if this is one.
    pub(crate) fn image(&self) -> Option<&ImagePreview> {
        match self {
            Self::Image(image) => Some(image),
            Self::File { .. } => None,
        }
    }

    /// Frees the decoded image GPUI keeps for drawing an image, which it
    /// would otherwise keep for good. GPUI keys it by the image's bytes, so
    /// anything else that shows the same image decodes it again.
    pub(crate) fn release(&self, cx: &mut App) {
        if let Self::Image(image) = self {
            Arc::clone(&image.image).remove_asset(cx);
        }
    }
}

/// An image ready to draw: a GPUI image of its bytes, its size, and its
/// proportions for a tile. None has a side over [`MAX_SIDE`] px.
#[derive(Clone, Debug)]
pub(crate) struct ImagePreview {
    /// The image it was made from, sharing its bytes, to tell whether it
    /// still shows the same one; see [`Self::shows`]. `None` for a file's
    /// picture, whose bytes it doesn't keep twice.
    source: Option<Image>,
    image: Arc<gpui_kit::Image>,
    /// Its size in pixels, neither of them zero.
    width: u32,
    height: u32,
    /// Width over height, within [`ASPECT_RANGE`].
    aspect: f32,
    /// What it is, such as "PNG image, 1920 × 1080".
    label: SharedString,
}

impl ImagePreview {
    /// The preview of `image`, `width` × `height` px as its header says.
    /// Copies its bytes, which GPUI then hashes, so callers build one once.
    fn new(image: &Image, width: u32, height: u32) -> Self {
        Self {
            source: Some(image.clone()),
            ..Self::drawn(image.format(), image.bytes().to_vec(), width, height)
        }
    }

    /// The preview of `bytes`, an image of `format` `width` × `height` px
    /// as its header says.
    fn drawn(format: ImageFormat, bytes: Vec<u8>, width: u32, height: u32) -> Self {
        let aspect = (f64::from(width) / f64::from(height)).clamp(ASPECT_RANGE.0, ASPECT_RANGE.1);
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "a ratio within ASPECT_RANGE fits f32 with room to spare"
        )]
        let aspect = aspect as f32;
        Self {
            source: None,
            image: Arc::new(gpui_kit::Image::from_bytes(gpui_format(format), bytes)),
            width,
            height,
            aspect,
            label: format!("{} image, {width} × {height}", format.name()).into(),
        }
    }

    /// The preview of a file's `bytes`, read as the image they are.
    ///
    /// # Errors
    ///
    /// Says why when they aren't an image rho shows, or one with a side
    /// over [`MAX_SIDE`] pixels.
    pub(crate) fn from_file(bytes: Vec<u8>) -> Result<Self, String> {
        let format = match sniff(&bytes) {
            Sniffed::Image(format) => format,
            Sniffed::OtherImage(mime_type) => {
                return Err(format!(
                    "Rho shows PNG, JPEG, GIF and WebP images, and this is {mime_type}."
                ))
            }
            Sniffed::NotImage => return Err("It isn't an image.".to_owned()),
        };
        let (width, height) = dimensions(format, &bytes)
            .ok_or_else(|| format!("Its {} data doesn't read.", format.name()))?;
        if width.max(height) > MAX_SIDE {
            return Err(format!(
                "It's {width} × {height} px, and rho-ui shows up to {MAX_SIDE} px a side."
            ));
        }
        Ok(Self::drawn(format, bytes, width, height))
    }

    /// Whether it was made from `image`. Cheap when it was: clones of an
    /// image share its bytes, which compare by address first.
    pub(crate) fn shows(&self, image: &Image) -> bool {
        self.source.as_ref() == Some(image)
    }

    /// The image, for GPUI to draw.
    pub(crate) fn image(&self) -> Arc<gpui_kit::Image> {
        Arc::clone(&self.image)
    }

    /// Its width and height in pixels.
    pub(crate) fn pixels(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// What it is, such as "PNG image, 1920 × 1080".
    pub(crate) fn label(&self) -> SharedString {
        self.label.clone()
    }

    /// The image as a Kit image tile `height` rems tall and as wide as its
    /// proportions make it.
    pub(crate) fn tile(&self, height: f32) -> Card {
        Card::new()
            .axis(Axis::Vertical)
            .w(rems(height * self.aspect))
            .h(rems(height))
            .media(
                AttachmentMedia::new()
                    .src(Arc::clone(&self.image))
                    .size_full(),
            )
    }
}

/// The icon for a file named `name`, by its extension. Only the icon
/// rests on this guess: what the file holds decides how it is sent. Each
/// icon but `File` and `FileText` ships in `ExtraIcons`.
fn file_icon(name: &str) -> Lucide {
    let extension = Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "c" | "cc" | "cpp" | "cs" | "css" | "go" | "h" | "hpp" | "html" | "java" | "js" | "jsx"
        | "kt" | "lua" | "php" | "py" | "rb" | "rs" | "sh" | "sql" | "swift" | "ts" | "tsx"
        | "zig" => Lucide::FileCode,
        "json" | "jsonl" | "ron" | "toml" | "xml" | "yaml" | "yml" => Lucide::FileBraces,
        "csv" | "ods" | "tsv" | "xls" | "xlsx" => Lucide::FileSpreadsheet,
        "7z" | "bz2" | "gz" | "rar" | "tar" | "tgz" | "xz" | "zip" | "zst" => Lucide::FileArchive,
        "avif" | "bmp" | "gif" | "heic" | "ico" | "jpeg" | "jpg" | "png" | "svg" | "tif"
        | "tiff" | "webp" => Lucide::FileImage,
        "doc" | "docx" | "log" | "md" | "odt" | "org" | "pdf" | "rst" | "rtf" | "tex" | "txt" => {
            Lucide::FileText
        }
        _ => Lucide::File,
    }
}

/// The width and height in pixels of `bytes`, an image of `format`, as its
/// header says; `None` when the header doesn't read as that format, or
/// gives it no pixels.
fn dimensions(format: ImageFormat, bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::with_format(Cursor::new(bytes), decoder(format));
    let (width, height) = reader.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// What a file's first bytes say it is.
enum Sniffed {
    /// An image rho takes.
    Image(ImageFormat),
    /// An image of another kind, by its MIME type.
    OtherImage(&'static str),
    NotImage,
}

fn sniff(bytes: &[u8]) -> Sniffed {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => Sniffed::Image(ImageFormat::Png),
        Ok(image::ImageFormat::Jpeg) => Sniffed::Image(ImageFormat::Jpeg),
        Ok(image::ImageFormat::Gif) => Sniffed::Image(ImageFormat::Gif),
        Ok(image::ImageFormat::WebP) => Sniffed::Image(ImageFormat::Webp),
        Ok(other) => Sniffed::OtherImage(other.to_mime_type()),
        Err(_) => Sniffed::NotImage,
    }
}

/// The image a clipboard entry holds, in the format its bytes are, which
/// may not be the one the copying app named.
///
/// # Errors
///
/// Says why when the bytes are not an image rho takes.
pub(crate) fn from_clipboard(image: &gpui_kit::Image) -> Result<Image, String> {
    match sniff(&image.bytes) {
        Sniffed::Image(format) => Ok(Image::new(format, image.bytes.as_slice())),
        Sniffed::OtherImage(mime_type) => Err(format!(
            "Rho takes PNG, JPEG, GIF and WebP images, and this is {mime_type}"
        )),
        Sniffed::NotImage => Err("the clipboard's image data isn't an image".to_owned()),
    }
}

/// A file read to attach: the attachment, and for a file that goes by its
/// path only because of a limit or a fault, why, so the user can see it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReadFile {
    pub(crate) attachment: Attachment,
    /// The limit it passed, and by how much, such as "250000 characters,
    /// over the 200000 sent whole". `None` for a file sent whole, or one
    /// that goes by its path because it isn't text.
    pub(crate) why_linked: Option<String>,
}

impl ReadFile {
    fn whole(attachment: Attachment) -> Self {
        Self {
            attachment,
            why_linked: None,
        }
    }

    fn linked(path: &Path, why: Option<String>) -> Self {
        Self {
            attachment: Attachment::File(AttachedFile::linked(path)),
            why_linked: why,
        }
    }
}

/// The file at `path` as an attachment: an image rho takes as an image, a
/// text file of at most [`MAX_ATTACHED_TEXT_CHARS`] whole, and any other
/// file, or one too large to send, as a link to it, saying why when a
/// limit or a fault made it one. Blocking: run it off the UI thread.
///
/// # Errors
///
/// Says why when `path` isn't a file that can be read.
pub(crate) fn read(path: &Path) -> Result<ReadFile, String> {
    let path = std::path::absolute(path).map_err(|error| error.to_string())?;
    if path.to_string_lossy().contains('\n') {
        // A link's path is a line of its own; this one would break it.
        return Err("its path has a line break in it".to_owned());
    }
    let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("it isn't a file".to_owned());
    }
    // An image no message can carry is linked unread.
    let sendable = MAX_PROMPT_ATTACHMENT_BYTES / 4 * 3;
    let len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if len > sendable {
        let why = format!(
            "{}, over the {} a message carries",
            mebibytes(len),
            mebibytes(sendable)
        );
        return Ok(ReadFile::linked(&path, Some(why)));
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    if let Sniffed::Image(format) = sniff(&bytes) {
        if dimensions(format, &bytes).is_some() {
            return Ok(ReadFile::whole(Attachment::Image(Image::new(
                format, bytes,
            ))));
        }
        let why = format!("its {} data doesn't read", format.name());
        return Ok(ReadFile::linked(&path, Some(why)));
    }
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => {
            let chars = text.chars().count();
            if chars <= MAX_ATTACHED_TEXT_CHARS {
                let file = AttachedFile::with_text(&path, text);
                return Ok(ReadFile::whole(Attachment::File(file)));
            }
            let why = format!("{chars} characters, over the {MAX_ATTACHED_TEXT_CHARS} sent whole");
            Ok(ReadFile::linked(&path, Some(why)))
        }
        _ => Ok(ReadFile::linked(&path, None)),
    }
}

fn decoder(format: ImageFormat) -> image::ImageFormat {
    match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
    }
}

fn gpui_format(format: ImageFormat) -> gpui_kit::ImageFormat {
    match format {
        ImageFormat::Png => gpui_kit::ImageFormat::Png,
        ImageFormat::Jpeg => gpui_kit::ImageFormat::Jpeg,
        ImageFormat::Gif => gpui_kit::ImageFormat::Gif,
        ImageFormat::Webp => gpui_kit::ImageFormat::Webp,
    }
}

/// A `width` × `height` PNG of one colour, encoded for real.
#[cfg(test)]
pub(crate) fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba(rgba))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode a PNG");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        super::png(width, height, [0, 0, 0, 255])
    }

    fn image_preview(attachment: &Attachment) -> ImagePreview {
        match Preview::new(attachment) {
            Some(Preview::Image(preview)) => preview,
            other => panic!("an image preview: {other:?}"),
        }
    }

    #[test]
    fn a_pasted_image_is_read_by_its_bytes_not_its_label() {
        let pasted = gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Jpeg, png(4, 2));
        let image = from_clipboard(&pasted).expect("a PNG is taken");
        assert_eq!(image.format(), ImageFormat::Png);
    }

    #[test]
    fn a_thin_image_is_cropped_to_the_aspect_range() {
        let image = Attachment::Image(Image::new(ImageFormat::Png, png(100, 1)));
        let preview = image_preview(&image);
        assert!((preview.aspect - 4.).abs() < f32::EPSILON, "{preview:?}");
    }

    /// An image too large for a texture shows as a chip, not a tile that
    /// would draw nothing; it still goes as an image.
    #[test]
    fn an_image_too_large_to_preview_shows_as_a_chip() {
        let image = Attachment::Image(Image::new(ImageFormat::Png, png(MAX_SIDE + 1, 1)));
        assert!(
            matches!(Preview::new(&image), Some(Preview::File { .. })),
            "a chip"
        );
    }

    /// Binary files and text too long to send go by their path, the long
    /// text saying why, and a directory is refused. The UI tests own images and short text.
    #[test]
    fn a_file_is_read_as_what_it_is() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).expect("write a test file");
            path
        };
        let binary = write("tool.bin", b"\x7fELF\0\0\0");
        let long = write(
            "big.log",
            "x".repeat(MAX_ATTACHED_TEXT_CHARS + 1).as_bytes(),
        );

        // Only the limit says why; a binary file goes by path as a rule.
        for (linked, says_why) in [(&binary, false), (&long, true)] {
            let Ok(ReadFile {
                attachment: Attachment::File(file),
                why_linked,
            }) = read(linked)
            else {
                panic!("{} is a file", linked.display());
            };
            assert_eq!(file.text(), None, "{} goes by path", linked.display());
            assert_eq!(why_linked.is_some(), says_why, "{}", linked.display());
        }
        assert_eq!(read(dir.path()), Err("it isn't a file".to_owned()));
    }
}
