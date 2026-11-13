//! Attachments as the composer and the transcript draw them, as GPUI Kit
//! attachment cards: a file as a chip with an icon for its kind, and an
//! image as a square tile as tall as a chip, or in the transcript, larger
//! and in its own proportions. Also reads what the user attaches, from the
//! clipboard or from disk.

use std::{ffi::OsStr, fs, io::Cursor, path::Path, sync::Arc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::attachment::{
    Attachment as Card, AttachmentContent, AttachmentDescription, AttachmentMedia, AttachmentTitle,
};
use gpui_kit::component::Icon;
use gpui_kit::{rems, App, Axis, ParentElement as _, SharedString, Styled as _};
use rho_ui_core::{
    AttachedFile, Attachment, Image, ImageFormat, MAX_ATTACHED_TEXT_CHARS,
    MAX_PROMPT_ATTACHMENT_BYTES,
};

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
        /// Where it is, for its tooltip.
        place: SharedString,
        /// What kind of file it is, by its name.
        icon: Lucide,
    },
}

impl Preview {
    /// The preview of `attachment`; `None` for an image whose header
    /// doesn't read as its format.
    pub(crate) fn new(attachment: &Attachment) -> Option<Self> {
        Some(match attachment {
            Attachment::Image(image) => Self::Image(ImagePreview::new(image)?),
            Attachment::File(file) => Self::File {
                name: file.name().to_owned().into(),
                detail: match file.text() {
                    Some(text) => match text.lines().count() {
                        0 | 1 => "1 line".into(),
                        lines => format!("{lines} lines").into(),
                    },
                    None => "Sent by path".into(),
                },
                place: file
                    .path()
                    .map_or_else(|| file.uri().to_owned(), |path| path.display().to_string())
                    .into(),
                icon: file_icon(file.name()),
            },
        })
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
    /// a chip whose tooltip says where it is. Give a row of them one size,
    /// and it lines up.
    pub(crate) fn card(&self) -> Card {
        match self {
            Self::Image(image) => Card::new()
                .axis(Axis::Vertical)
                .media(AttachmentMedia::new().src(Arc::clone(&image.image))),
            Self::File {
                name,
                detail,
                place,
                icon,
            } => Card::new()
                .tooltip(place.clone())
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

/// An image ready to draw: a GPUI image of its bytes, and its proportions.
#[derive(Clone, Debug)]
pub(crate) struct ImagePreview {
    image: Arc<gpui_kit::Image>,
    /// Width over height, within [`ASPECT_RANGE`].
    aspect: f32,
    /// What it is, such as "PNG image, 1920 × 1080".
    label: SharedString,
}

impl ImagePreview {
    /// The preview of `image`; `None` when its header doesn't read as its
    /// format.
    fn new(image: &Image) -> Option<Self> {
        let reader =
            image::ImageReader::with_format(Cursor::new(image.bytes()), decoder(image.format()));
        let (width, height) = reader.into_dimensions().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let aspect = (f64::from(width) / f64::from(height)).clamp(ASPECT_RANGE.0, ASPECT_RANGE.1);
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "a ratio within ASPECT_RANGE fits f32 with room to spare"
        )]
        let aspect = aspect as f32;
        let format = image.format();
        Some(Self {
            image: Arc::new(gpui_kit::Image::from_bytes(
                gpui_format(format),
                image.bytes().to_vec(),
            )),
            aspect,
            label: format!("{} image, {width} × {height}", format.name()).into(),
        })
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
            "rho takes PNG, JPEG, GIF and WebP images, and this is {mime_type}"
        )),
        Sniffed::NotImage => Err("the clipboard's image data isn't an image".to_owned()),
    }
}

/// The file at `path` as an attachment: an image rho takes as an image, a
/// text file of at most [`MAX_ATTACHED_TEXT_CHARS`] whole, and any other
/// file, or one too large to send, as a link to it. Blocking: run it off
/// the UI thread.
///
/// # Errors
///
/// Says why when `path` isn't a file that can be read.
pub(crate) fn read(path: &Path) -> Result<Attachment, String> {
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
    if usize::try_from(metadata.len()).map_or(true, |len| len > sendable) {
        return Ok(Attachment::File(AttachedFile::linked(&path)));
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    if let Sniffed::Image(format) = sniff(&bytes) {
        let image = Image::new(format, bytes);
        if ImagePreview::new(&image).is_some() {
            return Ok(Attachment::Image(image));
        }
        return Ok(Attachment::File(AttachedFile::linked(&path)));
    }
    let file = match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') && text.chars().count() <= MAX_ATTACHED_TEXT_CHARS => {
            AttachedFile::with_text(&path, text)
        }
        _ => AttachedFile::linked(&path),
    };
    Ok(Attachment::File(file))
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
        let preview = image_preview(&Attachment::Image(image));
        assert_eq!(preview.label.as_ref(), "PNG image, 4 × 2");
        assert!((preview.aspect - 2.).abs() < f32::EPSILON, "{preview:?}");
    }

    #[test]
    fn an_image_rho_does_not_take_is_refused_with_its_type() {
        // A BMP starts with "BM"; only the start is read.
        let pasted = gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Bmp, b"BM\0\0".to_vec());
        assert_eq!(
            from_clipboard(&pasted),
            Err("rho takes PNG, JPEG, GIF and WebP images, and this is image/bmp".to_owned())
        );
    }

    /// An icon missing from the app's assets compiles, then draws nothing.
    #[test]
    fn every_file_icon_ships_with_the_app() {
        use gpui_kit::AssetSource as _;
        let names = [
            "a.rs", "a.json", "a.csv", "a.zip", "a.bmp", "a.PDF", "a", "a.bin",
        ];
        for name in names {
            let icon = file_icon(name);
            let svg = crate::Assets.load(&icon.path()).expect("the assets load");
            assert!(svg.is_some(), "{name}'s icon, {icon:?}, ships");
        }
        assert_eq!(file_icon("REPORT.PDF"), Lucide::FileText);
    }

    #[test]
    fn a_thin_image_is_cropped_to_the_aspect_range() {
        let image = Attachment::Image(Image::new(ImageFormat::Png, png(100, 1)));
        let preview = image_preview(&image);
        assert!((preview.aspect - 4.).abs() < f32::EPSILON, "{preview:?}");
    }

    /// An image goes as one, text goes whole, and anything else, or text
    /// too long to send, by its path.
    #[test]
    fn a_file_is_read_as_what_it_is() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).expect("write a test file");
            path
        };
        let shot = write("shot.dat", &png(2, 2));
        let notes = write("notes.md", b"# Notes\n\nhello\n");
        let binary = write("tool.bin", b"\x7fELF\0\0\0");
        let long = write(
            "big.log",
            "x".repeat(MAX_ATTACHED_TEXT_CHARS + 1).as_bytes(),
        );

        assert!(
            matches!(read(&shot), Ok(Attachment::Image(image)) if image.format() == ImageFormat::Png)
        );
        let Ok(Attachment::File(file)) = read(&notes) else {
            panic!("a text file is a file");
        };
        assert_eq!(file.text(), Some("# Notes\n\nhello\n"));
        assert_eq!(file.path(), Some(notes.as_path()));
        let chip = Preview::new(&Attachment::File(file)).expect("a file previews");
        assert_eq!(chip.label().as_ref(), "notes.md, 3 lines");
        for linked in [&binary, &long] {
            let Ok(Attachment::File(file)) = read(linked) else {
                panic!("{} is a file", linked.display());
            };
            assert_eq!(file.text(), None, "{} goes by path", linked.display());
        }
        assert_eq!(read(dir.path()), Err("it isn't a file".to_owned()));
    }
}
