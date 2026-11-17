//! The page's files: the web build (`index.html` at `/`, its JS and wasm,
//! and fonts), and at `/assets/` GPUI Kit's icons, which the page fetches
//! as it draws each, from this binary's own.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, MethodRouter},
};
use gpui_kit::AssetSource as _;

use crate::Assets;

/// Where the web build comes from.
#[derive(Clone, Debug)]
pub(crate) enum Web {
    /// A folder as `cargo xtask web` makes it.
    Dir(PathBuf),
    /// The one `cargo xtask dist` built into this binary, each file
    /// gzipped.
    #[cfg(feature = "web")]
    BuiltIn,
}

/// The web build `cargo xtask dist` gzipped into `target/web-gz`.
#[cfg(feature = "web")]
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../target/web-gz"]
struct BuiltIn;

impl Web {
    /// The web build this binary has: built in, or in a build without one,
    /// `target/web` of the checkout it was built from, as
    /// `cargo xtask web` makes it.
    ///
    /// # Errors
    ///
    /// Says how to make one if there is none.
    pub(crate) fn built_in() -> Result<Self, String> {
        #[cfg(feature = "web")]
        return Ok(Self::BuiltIn);
        #[cfg(not(feature = "web"))]
        {
            let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/web"));
            Self::dir(dir).map_err(|_| {
                "this rho-ui has no web build: run `cargo xtask web` in its checkout, or build \
                 one with it in, with `cargo xtask dist`"
                    .to_owned()
            })
        }
    }

    /// The web build in `dir`.
    ///
    /// # Errors
    ///
    /// If `dir` has no `index.html`.
    pub(crate) fn dir(dir: PathBuf) -> Result<Self, String> {
        if dir.join("index.html").is_file() {
            Ok(Self::Dir(dir))
        } else {
            Err(format!(
                "no web build in {}: make one with `cargo xtask web`",
                dir.display()
            ))
        }
    }

    /// The file at `relative`, and whether it is gzipped; `None` if there
    /// is none.
    async fn file(&self, relative: &str) -> Option<(Cow<'static, [u8]>, Encoding)> {
        match self {
            Self::Dir(dir) => tokio::fs::read(dir.join(relative))
                .await
                .ok()
                .map(|bytes| (Cow::Owned(bytes), Encoding::Plain)),
            #[cfg(feature = "web")]
            Self::BuiltIn => BuiltIn::get(relative).map(|file| (file.data, Encoding::Gzip)),
        }
    }
}

/// How a file's bytes are stored, and so sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Plain,
    /// Every browser takes gzip, so it goes as it is.
    #[cfg_attr(
        not(feature = "web"),
        expect(dead_code, reason = "only a built-in web build is")
    )]
    Gzip,
}

/// Serves `web`'s files, `index.html` for `/`, and Kit's icons at
/// `/assets/`. A path with a `..`, or any segment that names no plain file
/// name, gets 404, so nothing outside the web build is served.
pub(crate) fn serve(web: Web) -> MethodRouter {
    get(move |uri: Uri| {
        let web = web.clone();
        async move { file(&web, uri.path()).await }
    })
}

async fn file(web: &Web, path: &str) -> Response {
    let relative = match path.trim_start_matches('/') {
        "" => "index.html",
        relative => relative,
    };
    let plain = relative
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if !plain {
        return StatusCode::NOT_FOUND.into_response();
    }
    let found = match relative.strip_prefix("assets/") {
        Some(asset) => Assets::new("")
            .load(asset)
            .ok()
            .flatten()
            .map(|bytes| (bytes, Encoding::Plain)),
        None => web.file(relative).await,
    };
    let Some((bytes, encoding)) = found else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let kind = (header::CONTENT_TYPE, content_type(Path::new(relative)));
    match encoding {
        Encoding::Plain => ([kind], bytes).into_response(),
        Encoding::Gzip => ([kind, (header::CONTENT_ENCODING, "gzip")], bytes).into_response(),
    }
}

/// The type a browser needs to use the file: wasm must be
/// `application/wasm` to compile as it streams in.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}
