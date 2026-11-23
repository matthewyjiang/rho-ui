//! The pictures of the people on a forge, fetched with `curl` from where
//! the forge says each is, on the machine whose CLI reads the forge: the
//! one sure to reach it. Not with `gh api`, which sends its token to the
//! forge's own host alone, and took 60 s here before asking another.
//!
//! A picture that needs a sign-in, as a private GitHub Enterprise's may,
//! doesn't load, and the person's initials show instead.

use std::{ffi::OsString, sync::Arc};

use gpui_kit::{BackgroundExecutor, Image};
use rho_ui_core::platform;

use crate::{
    attachments::ImagePreview,
    folders::{self, Deadline, Reader},
    local,
};

/// How long `curl` may take to fetch a picture, in seconds: one is about
/// 5 kB (5,732 bytes for a 72 px PNG) and took 40 ms here, so this only
/// stops a fetch that hangs.
const LIMIT_SECS: &str = "20";

/// `curl`'s arguments to print the picture at `url`: quiet but for
/// errors, failing on an HTTP error, following redirects, and loading web
/// addresses only, so a forge can't have it read a file. `--` ends the
/// options, so an address can't pass for one.
fn arguments(url: &str) -> Vec<OsString> {
    let options = ["-fsSL", "--proto", "=http,https", "--max-time", LIMIT_SECS];
    options
        .into_iter()
        .chain(["--", url])
        .map(OsString::from)
        .collect()
}

/// The POSIX `sh` script that runs `curl` with its arguments, and prints
/// [`MARKER`] before what it prints.
const SCRIPT: &str = r#"printf 'rho-ui-avatar\0'
exec curl "$@" </dev/null
"#;

/// Printed before the picture, so whatever a login file prints comes
/// first and is skipped.
const MARKER: &[u8] = b"rho-ui-avatar\0";

/// The picture at `url`, fetched through `reader`.
///
/// # Errors
///
/// Says why when `curl` can't run or fetch it, or it isn't an image
/// rho-ui draws.
pub(crate) async fn fetch(
    reader: Reader,
    url: &str,
    executor: &BackgroundExecutor,
) -> Result<Arc<Image>, String> {
    let arguments = arguments(url);
    let here = arguments.clone();
    reader
        .query(
            executor,
            move || {
                let mut curl = platform::background_command("curl");
                picture(local::stdout(curl.args(here))?)
            },
            SCRIPT,
            arguments,
            Deadline::Script,
            |output| {
                let at = folders::find(&output, MARKER)
                    .ok_or_else(|| "curl printed nothing.".to_owned())?;
                picture(output[at + MARKER.len()..].to_vec())
            },
        )
        .await
}

/// `bytes` as the image they are.
fn picture(bytes: Vec<u8>) -> Result<Arc<Image>, String> {
    ImagePreview::from_file(bytes).map(|preview| preview.image())
}
