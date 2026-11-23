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
/// errors, failing on an HTTP error, following redirects, taking the
/// address as written (`-g`: its brackets and braces name no others), and
/// loading web addresses only, so a forge can't have it read a file. `--`
/// ends the options, so an address can't pass for one.
fn arguments(url: &str) -> Vec<OsString> {
    let options = ["-fsSLg", "--proto", "=http,https", "--max-time", LIMIT_SECS];
    options
        .into_iter()
        .chain(["--", url])
        .map(OsString::from)
        .collect()
}

/// The POSIX `sh` script that runs `curl` with its arguments, and prints
/// [`MARKER`] before what it prints. Read with [`parse`]; [`fetch_here`]
/// does the same here.
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
            move || fetch_here(&here),
            SCRIPT,
            arguments,
            Deadline::Script,
            |output| parse(&output),
        )
        .await
}

/// [`SCRIPT`] in Rust, with the same arguments, read as [`parse`] reads
/// what it prints.
fn fetch_here(arguments: &[OsString]) -> Result<Arc<Image>, String> {
    let mut curl = platform::background_command("curl");
    picture(local::stdout(curl.args(arguments))?)
}

/// The picture [`SCRIPT`] printed.
fn parse(output: &[u8]) -> Result<Arc<Image>, String> {
    let bytes = folders::after(output, MARKER).ok_or_else(|| "curl printed nothing.".to_owned())?;
    picture(bytes.to_vec())
}

/// `bytes` as the image they are, decoded once to tell that it draws: an
/// image whose header alone reads would show as an empty disc, where one
/// that fails shows its person's initials.
fn picture(bytes: Vec<u8>) -> Result<Arc<Image>, String> {
    let preview = ImagePreview::from_file(bytes)?;
    let image = preview.image();
    image::load_from_memory(image.bytes()).map_err(|error| format!("It doesn't draw: {error}"))?;
    Ok(image)
}

/// For tests: pictures to fetch, served on this machine.
#[cfg(test)]
pub(crate) mod test {
    use std::io::{BufRead as _, BufReader, Write as _};

    /// Serves a picture to whatever asks for a path over HTTP on this
    /// machine, of a colour of the path's own, until the tests end.
    /// Returns its address, such as `http://127.0.0.1:4000`.
    pub(crate) fn serve_pictures() -> String {
        const COLOURS: [[u8; 4]; 3] = [
            [222, 120, 60, 255],
            [70, 140, 210, 255],
            [90, 170, 110, 255],
        ];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut lines = BufReader::new(&stream).lines();
                // `GET /hubot.png HTTP/1.1`, then headers up to an empty line.
                let request = lines.next().unwrap().unwrap();
                let read_headers = lines.find(|line| line.as_ref().is_ok_and(String::is_empty));
                assert!(read_headers.is_some(), "the request's end: {request}");
                let path = request.split(' ').nth(1).unwrap();
                let colour = path.bytes().map(usize::from).sum::<usize>() % COLOURS.len();
                let picture = crate::attachments::png(72, 72, COLOURS[colour]);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    picture.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&picture).unwrap();
            }
        });
        address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script a remote machine runs fetches the picture this machine
    /// does, past what a login file prints before it.
    #[test]
    #[cfg(unix)]
    fn the_script_fetches_as_here() {
        let url = format!("{}/hubot.png", test::serve_pictures());
        let arguments = arguments(&url);
        let mut printed = b"motd\n".to_vec();
        printed.extend(local::test::sh(SCRIPT, &arguments).unwrap());
        let there = parse(&printed).unwrap();
        let here = fetch_here(&arguments).unwrap();
        assert_eq!(there.bytes(), here.bytes());
        assert_eq!(there.format(), gpui_kit::ImageFormat::Png);
    }

    /// A picture cut short, whose header still reads, isn't one.
    #[test]
    fn a_picture_that_does_not_draw_fails() {
        let mut bytes = crate::attachments::png(72, 72, [0, 0, 0, 255]);
        bytes.truncate(bytes.len() / 2);
        assert!(
            ImagePreview::from_file(bytes.clone()).is_ok(),
            "its header reads"
        );
        assert!(picture(bytes).is_err());
    }
}
