//! Pictures from the web that a forge's pages show: the people who post
//! and the images in a pull request's text. Each is fetched with `curl`
//! from where its address says, on the machine whose CLI reads the forge:
//! the one sure to reach it, as rho-ui itself may not.
//!
//! That machine may sit in a private network, and anyone who can comment
//! writes a PR's text, so a fetch goes only to the forge's own hosts and
//! GitHub's image hosts ([`allowed`]), over https: GitHub's own pages
//! proxy other images for the same reason. `curl` follows no redirect:
//! each one is checked as the first address was, and fetched as a fetch
//! of its own.
//!
//! A picture in a private repository's text needs a sign-in: GitHub
//! answers `github.com/user-attachments/assets/…` with a 404 without one,
//! and with a redirect to a signed, five-minute storage URL with the
//! token of an account that can read the repository (a public one
//! redirects either way). So a fetch from the forge's own host, over
//! https, sends the token `gh auth token` prints on that machine, read by
//! the script and handed to `curl` on its standard input: never in an
//! argument, a log, or anything rho-ui keeps. A person's picture is
//! fetched without: one that needs a sign-in, as a private GitHub
//! Enterprise's may, doesn't load, and the person's initials show instead.
//!
//! Not with `gh api`, which sends its token to the forge's own host alone,
//! took 60 s here before asking another, and keeps no size limit.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    future::Future,
    io::{Read as _, Write as _},
    path::PathBuf,
    pin::pin,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use futures::future::{self, Either};
use gpui_kit::{BackgroundExecutor, Image};
use rho_ui_core::platform;
use web_time::Instant;

use super::gh;
use crate::{
    attachments::ImagePreview,
    folders::{self, Deadline, Reader},
    local,
};

/// The most bytes a picture may have: GitHub's own limit on an image
/// attached to an issue or PR, 10 MB (GitHub Docs, "Attaching files"), so
/// any image uploaded there loads. Those in rho's and rho-ui's 200 newest
/// PRs were 216 bytes to 445 kB; a video, as `user-attachments` also
/// serves, was 87 MB.
pub(crate) const MAX_BYTES: usize = 10 << 20;

/// The most bytes of headers a response may have before its picture:
/// GitHub's redirect from `user-attachments` had 5,950 here, the storage's
/// picture 585, and `user-images`' 766.
const HEAD_ROOM: usize = 64 << 10;

/// The most redirects a fetch follows: `user-attachments` and a
/// repository's `assets` redirected once here, to storage, and
/// `user-images` and `avatars` not at all.
const MAX_REDIRECTS: usize = 3;

/// How long `curl` may take to fetch a picture, in seconds: one of 445 kB
/// took 0.9 s here and 87 MB took 4.2 s, so this only stops a fetch that
/// hangs, or [`MAX_BYTES`] on a link under 210 kB/s. It is under the
/// 60 s a script over ssh gets (rho-ui-core's `SCRIPT_DEADLINE`), so a
/// first fetch that hangs says why in `curl`'s words; [`super::DEADLINE`]
/// bounds all of a picture's fetches together.
const LIMIT_SECS: &str = "50";

/// How long `gh auth token` may take here: it took 0.08 s, so this only
/// stops one waiting on a locked keyring, whose fetch then goes unsigned.
/// Over ssh the script's own deadline stops it.
const TOKEN_DEADLINE: Duration = Duration::from_secs(10);

/// Where GitHub serves an image uploaded to it from, besides the forge's
/// own host: `user-images`, `private-user-images`, `camo`, `raw` and
/// `avatars` under this…
const GITHUB_IMAGES: &str = ".githubusercontent.com";

/// …and the storage `user-attachments` and a repository's `assets`
/// redirect to.
const GITHUB_ATTACHMENTS: &str = "github-production-user-asset-6210df.s3.amazonaws.com";

/// The forge a fetch is for: its host, and the `gh` that signs in there,
/// if one is: one its check found signed in, as `gh auth token` for a host
/// it doesn't know took 60 s here to fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Forge {
    pub(crate) host: String,
    pub(crate) gh: Option<PathBuf>,
}

/// Why a picture wasn't fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FetchError {
    /// rho-ui won't fetch it, nor would on asking again: it is on a host
    /// not [`allowed`], over [`MAX_BYTES`], or moves where it won't go.
    Refused(String),
    /// It couldn't be fetched this time: `curl` failed, or the machine or
    /// the web didn't answer.
    Failed(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// The scheme and authority `url` starts with, as `https` and
/// `github.com`; none for a path, as `/a?to=https://b` is.
fn origin(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let named = !scheme.is_empty()
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'));
    named.then(|| {
        (
            scheme,
            rest.split(['/', '?', '#']).next().unwrap_or_default(),
        )
    })
}

/// The authority of an https `url`: its host, and port if it says one.
fn authority(url: &str) -> Option<&str> {
    origin(url)
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("https"))
        .map(|(_, authority)| authority)
}

/// Whether rho-ui fetches `url` for a forge on `host`: over https, from
/// that host or one under it, or GitHub's image hosts. A host is only
/// letters, digits, dots and dashes, so no user, port or escape makes
/// `curl` read another.
///
/// # Errors
///
/// Says that it won't, and why.
pub(crate) fn allowed(url: &str, host: &str) -> Result<(), FetchError> {
    // The tests' pictures, served over http on this machine.
    if cfg!(test) && url.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    let to = authority(url).unwrap_or_default().to_ascii_lowercase();
    let forge = host.to_ascii_lowercase();
    let plain = !to.is_empty()
        && to
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    let ours = !to.is_empty()
        && (to == forge
            || plain
                && (!forge.is_empty() && to.ends_with(&format!(".{forge}"))
                    || to.ends_with(GITHUB_IMAGES)
                    || to == GITHUB_ATTACHMENTS));
    if ours {
        Ok(())
    } else {
        Err(FetchError::Refused(format!(
            "rho-ui loads images over https from {host} and GitHub's image hosts alone."
        )))
    }
}

/// Whether a fetch of `url` sends the token for `host`: only to that host,
/// over https, as the forge's own pages would.
fn signs_in(url: &str, host: &str) -> bool {
    authority(url).is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

/// `curl`'s arguments to print the response at `url`, headers first: quiet
/// but for errors, failing on an HTTP error, following no redirect, taking
/// the address as written (`-g`: its brackets and braces name no others),
/// and loading web addresses only, so a forge can't have it read a file,
/// up to [`MAX_BYTES`]. `--` ends the options, so an address can't pass
/// for one.
fn curl_arguments(url: &str) -> Vec<OsString> {
    let most = MAX_BYTES.to_string();
    let options = [
        "-fsSgi",
        "--suppress-connect-headers",
        "--proto",
        "=http,https",
        "--max-time",
        LIMIT_SECS,
        "--max-filesize",
        &most,
    ];
    options
        .into_iter()
        .chain(["--", url])
        .map(OsString::from)
        .collect()
}

/// [`SCRIPT`]'s arguments: the `gh` that signs in and its host, both empty
/// for none, the most bytes to read, past [`MAX_BYTES`] and
/// [`HEAD_ROOM`], then `curl`'s.
fn arguments(url: &str, forge: &Forge) -> Vec<OsString> {
    let (gh, host) = match &forge.gh {
        Some(gh) if signs_in(url, &forge.host) => {
            (gh.clone().into_os_string(), forge.host.clone().into())
        }
        _ => Default::default(),
    };
    let most = (HEAD_ROOM + MAX_BYTES + 1).to_string().into();
    [gh, host, most]
        .into_iter()
        .chain(curl_arguments(url))
        .collect()
}

/// The POSIX `sh` script that runs `curl` with its arguments, after the
/// `gh` and host that sign in and the most bytes to read, and prints
/// [`MARKER`] before what it prints. `curl -q` reads no `.curlrc`, whose
/// `location` would have it follow redirects. The token goes to `curl` as a header
/// on its standard input, with the shell's own `printf`. It stops reading
/// past the most, as `--max-filesize` stops `curl` before 8.4 only when the
/// size is given first; `curl`'s status, 23 then, comes out on fd 3. Read
/// with [`parse`]; [`fetch_here`] does the same here.
const SCRIPT: &str = r#"export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_SPINNER_DISABLED=1 NO_COLOR=1
printf 'rho-ui-picture\0'
gh=$1 host=$2 most=$3
shift 3
fetch() {
  if [ -n "$gh" ] && token=$("$gh" auth token --hostname "$host" </dev/null 2>/dev/null) && [ -n "$token" ]; then
    printf 'Authorization: Bearer %s\n' "$token" | curl -q -H @- "$@"
  else
    curl -q "$@" </dev/null
  fi
}
exec 4>&1
status=$( { { fetch "$@"; echo "$?" >&3; } | head -c "$most" >&4; } 3>&1 )
case $status in
  0|23) ;;
  *) exit "$status" ;;
esac
"#;

/// Printed before the response, so whatever a login file prints comes
/// first and is skipped.
const MARKER: &[u8] = b"rho-ui-picture\0";

/// The bytes of the picture at `url`, fetched through `reader` for
/// `forge`, signed in where [`signs_in`] says.
///
/// # Errors
///
/// Says why when it isn't [`allowed`], nor where it redirects, `curl`
/// can't run or fetch it, or it is over [`MAX_BYTES`].
pub(crate) async fn fetch(
    reader: Reader,
    url: &str,
    forge: &Forge,
    executor: &BackgroundExecutor,
) -> Result<Vec<u8>, FetchError> {
    let fetches = follow(url, forge, |arguments| {
        let here = arguments.clone();
        reader.clone().query(
            executor,
            move || fetch_here(&here),
            SCRIPT,
            arguments,
            Deadline::Script,
            |output| parse(&output),
        )
    });
    // Here too, as a `gh` waiting on a locked keyring would wait for ever.
    let deadline = executor.timer(super::DEADLINE);
    match future::select(pin!(fetches), deadline).await {
        Either::Left((bytes, _)) => bytes,
        Either::Right(_) => Err(FetchError::Failed(format!(
            "It didn't come within {} s.",
            super::DEADLINE.as_secs()
        ))),
    }
}

/// What [`fetch`] does, with `hop` fetching each address, given its
/// [`SCRIPT`] arguments, as `curl` prints it: the address and each one it
/// redirects to are [`allowed`] first.
async fn follow<Hop, Response>(
    url: &str,
    forge: &Forge,
    mut hop: Hop,
) -> Result<Vec<u8>, FetchError>
where
    Hop: FnMut(Vec<OsString>) -> Response,
    Response: Future<Output = Result<Vec<u8>, String>>,
{
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        allowed(&url, &forge.host)?;
        let response = hop(arguments(&url, forge))
            .await
            .map_err(|why| FetchError::Failed(curl_said(&why)))?;
        match answer(&response, &url)? {
            Answer::Picture(bytes) => return Ok(bytes.to_vec()),
            Answer::Moved(to) => url = to,
        }
    }
    Err(FetchError::Refused(format!(
        "It redirected more than {MAX_REDIRECTS} times."
    )))
}

/// `why` a fetch failed, in a sentence: `curl: (22) The requested URL
/// returned error: 404` says `The requested URL returned error: 404.`
fn curl_said(why: &str) -> String {
    let said = why
        .strip_prefix("curl: (")
        .and_then(|rest| rest.split_once(") "))
        .map_or(why, |(_, said)| said)
        .trim_end_matches('.');
    format!("{said}.")
}

/// What a response `curl` printed for `url` says.
#[derive(Debug, PartialEq, Eq)]
enum Answer<'a> {
    Picture(&'a [u8]),
    /// It moved to this address.
    Moved(String),
}

/// What `response`, headers then body, says, as fetched from `url`.
///
/// # Errors
///
/// Says why when its headers don't read, it moved to an address of a kind
/// not followed, or it is over [`MAX_BYTES`]: as told by its length
/// alone, which [`SCRIPT`] stops one past, whatever its headers say.
fn answer<'a>(mut response: &'a [u8], url: &str) -> Result<Answer<'a>, FetchError> {
    let too_big = || FetchError::Refused(format!("It's over {} MiB.", MAX_BYTES >> 20));
    if response.len() > HEAD_ROOM + MAX_BYTES {
        return Err(too_big());
    }
    loop {
        let room = &response[..response.len().min(HEAD_ROOM)];
        let end = room
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| {
                FetchError::Failed(format!(
                    "Its headers didn't end within {} KiB.",
                    HEAD_ROOM >> 10
                ))
            })?;
        let head = String::from_utf8_lossy(&response[..end]);
        let body = &response[end + 4..];
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .and_then(|line| line.split(' ').nth(1))
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| FetchError::Failed("Its answer didn't read.".to_owned()))?;
        match status {
            // Early hints, before the answer.
            100..=199 => response = body,
            200..=299 if body.len() > MAX_BYTES => return Err(too_big()),
            200..=299 => return Ok(Answer::Picture(body)),
            300..=399 => {
                let location = lines
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("location").then(|| value.trim())
                    })
                    .ok_or_else(|| {
                        FetchError::Failed(format!("It answered {status} without saying where to."))
                    })?;
                return moved(url, location).map(Answer::Moved);
            }
            _ => return Err(FetchError::Failed(format!("It answered {status}."))),
        }
    }
}

/// The address `location` names, as a redirect from `url` says it: a
/// full one, or a path on `url`'s host.
fn moved(url: &str, location: &str) -> Result<String, FetchError> {
    if origin(location).is_some() {
        return Ok(location.to_owned());
    }
    match origin(url) {
        Some((scheme, authority)) if location.starts_with('/') && !location.starts_with("//") => {
            Ok(format!("{scheme}://{authority}{location}"))
        }
        _ => Err(FetchError::Refused(format!(
            "It moved to {location}, which rho-ui doesn't follow."
        ))),
    }
}

/// The picture of a person at `url`, fetched through `reader` from its own
/// host: the forge's API names it, as nobody who comments can.
///
/// # Errors
///
/// As [`fetch`], and when it isn't an image rho-ui draws.
pub(crate) async fn avatar(
    reader: Reader,
    url: &str,
    executor: &BackgroundExecutor,
) -> Result<Arc<Image>, String> {
    let forge = Forge {
        host: authority(url).unwrap_or_default().to_owned(),
        gh: None,
    };
    let bytes = fetch(reader, url, &forge, executor)
        .await
        .map_err(|why| why.to_string())?;
    executor.spawn(async move { drawn(bytes) }).await
}

/// [`SCRIPT`] in Rust, with the same arguments, giving the bytes it
/// prints after [`MARKER`].
fn fetch_here(arguments: &[OsString]) -> Result<Vec<u8>, String> {
    let [gh, host, most, curl_arguments @ ..] = arguments else {
        return Err("A picture's fetch takes gh, a host, a size and curl's arguments.".to_owned());
    };
    let most: u64 = most
        .to_str()
        .and_then(|most| most.parse().ok())
        .ok_or_else(|| "A picture's fetch takes the most bytes to read.".to_owned())?;
    let token = (!gh.is_empty()).then(|| token_here(gh, host)).flatten();
    let mut curl = platform::background_command("curl");
    curl.arg("-q");
    if token.is_some() {
        curl.args(["-H", "@-"]);
    }
    curl.args(curl_arguments)
        .stdin(if token.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = curl
        .spawn()
        .map_err(|error| format!("Couldn't run curl: {error}"))?;
    if let (Some(token), Some(mut stdin)) = (token, child.stdin.take()) {
        let header = [b"Authorization: Bearer ".as_slice(), &token, b"\n"].concat();
        // A `curl` that already failed has stopped reading: its error says why.
        let _ = stdin.write_all(&header);
    }
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("stdout is piped")
        .take(most)
        .read_to_end(&mut bytes);
    if u64::try_from(bytes.len()).is_ok_and(|read| read >= most) {
        // Past the most, as `head` stops reading: [`answer`] says so.
        let _ = child.kill();
        let _ = child.wait();
        return Ok(bytes);
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Couldn't run curl: {error}"))?;
    read.map_err(|error| format!("Couldn't read what curl printed: {error}"))?;
    if output.status.success() {
        Ok(bytes)
    } else {
        Err(local::failure(&curl, &output))
    }
}

/// The token `gh` prints for `host` here, none if it fails or takes past
/// [`TOKEN_DEADLINE`]: it is stopped then.
fn token_here(gh: &OsStr, host: &OsStr) -> Option<Vec<u8>> {
    let mut child = gh::command(gh)
        .args(["auth", "token", "--hostname"])
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + TOKEN_DEADLINE;
    // A token is a line, which the pipe holds while `gh` runs.
    let signed_in = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let mut token = Vec::new();
    child.stdout.take()?.read_to_end(&mut token).ok()?;
    let end = token
        .iter()
        .rposition(|&byte| byte != b'\n')
        .map_or(0, |at| at + 1);
    token.truncate(end);
    (signed_in && !token.is_empty()).then_some(token)
}

/// The response [`SCRIPT`] printed.
fn parse(output: &[u8]) -> Result<Vec<u8>, String> {
    let bytes = folders::after(output, MARKER).ok_or_else(|| "curl printed nothing.".to_owned())?;
    Ok(bytes.to_vec())
}

/// `bytes` as the image they are, decoded once to tell that it draws: an
/// image whose header alone reads would show as an empty disc, where one
/// that fails shows its person's initials.
fn drawn(bytes: Vec<u8>) -> Result<Arc<Image>, String> {
    let preview = ImagePreview::from_file(bytes)?;
    let image = preview.image();
    image::load_from_memory(image.bytes()).map_err(|error| format!("It doesn't draw: {error}"))?;
    Ok(image)
}

/// For tests: pictures to fetch, served on this machine.
#[cfg(test)]
pub(crate) mod test {
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        sync::{Arc, Mutex},
    };

    /// A request's path, with the `Authorization` it sent.
    type Request = (String, Option<String>);

    /// A server of pictures over HTTP on this machine, until the tests end.
    /// A path such as `/shot-640x320.png` gets a PNG of that size, `/gone`
    /// paths a 404, `/moved/x` a redirect to `/x`, `/away` one to
    /// `localhost`, and any other a 72 px PNG; each of a colour of the
    /// path's own.
    pub(crate) struct Pictures {
        /// Such as `http://127.0.0.1:4000`.
        pub(crate) address: String,
        /// Each request's path, with the `Authorization` it sent.
        requests: Arc<Mutex<Vec<Request>>>,
    }

    impl Pictures {
        /// Each request it got: its path, with the `Authorization` it sent.
        pub(crate) fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// The size a path such as `/shot-640x320.png` asks for.
    fn size(path: &str) -> Option<(u32, u32)> {
        let name = path.strip_suffix(".png")?;
        let (_, size) = name.rsplit_once('-')?;
        let (width, height) = size.split_once('x')?;
        Some((width.parse().ok()?, height.parse().ok()?))
    }

    /// Starts a [`Pictures`].
    pub(crate) fn serve() -> Pictures {
        const COLOURS: [[u8; 4]; 3] = [
            [222, 120, 60, 255],
            [70, 140, 210, 255],
            [90, 170, 110, 255],
        ];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let away = format!(
            "http://localhost:{}/hubot.png",
            listener.local_addr().unwrap().port()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut lines = BufReader::new(&stream).lines();
                // `GET /hubot.png HTTP/1.1`, then headers up to an empty line.
                let request = lines.next().unwrap().unwrap();
                let mut authorization = None;
                for line in lines.by_ref() {
                    let line = line.unwrap();
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("authorization") {
                            authorization = Some(value.trim().to_owned());
                        }
                    }
                }
                let path = request.split(' ').nth(1).unwrap().to_owned();
                seen.lock().unwrap().push((path.clone(), authorization));
                if path.starts_with("/gone") {
                    let head =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    stream.write_all(head.as_bytes()).unwrap();
                    continue;
                }
                let to = match path.strip_prefix("/moved") {
                    Some(rest) => Some(rest),
                    None => (path == "/away").then_some(away.as_str()),
                };
                if let Some(to) = to {
                    let head = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\
                         Connection: close\r\n\r\n"
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    continue;
                }
                let (width, height) = size(&path).unwrap_or((72, 72));
                let colour = path.bytes().map(usize::from).sum::<usize>() % COLOURS.len();
                let picture = crate::attachments::png(width, height, COLOURS[colour]);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    picture.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&picture).unwrap();
            }
        });
        Pictures { address, requests }
    }

    /// Serves pictures as [`serve`] does, and returns its address.
    pub(crate) fn serve_pictures() -> String {
        serve().address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a fetch over https from the forge's own host sends its token:
    /// not one over http, from another host, or from one that only starts
    /// like it.
    #[test]
    fn the_token_goes_to_the_forge_alone() {
        let cases = [
            ("https://github.com/user-attachments/assets/1", true),
            ("https://GitHub.com/a.png", true),
            ("https://github.com?x", true),
            ("http://github.com/a.png", false),
            ("https://avatars.githubusercontent.com/u/1", false),
            ("https://github.com.evil.example/a.png", false),
            ("https://github.com@evil.example/a.png", false),
            ("https://github.com:8443/a.png", false),
        ];
        for (url, sends) in cases {
            assert_eq!(signs_in(url, "github.com"), sends, "{url}");
        }
    }

    /// Only https addresses on the forge's hosts and GitHub's image hosts
    /// are fetched: no other host, nor one that `curl` could read as
    /// another.
    #[test]
    fn only_the_forges_hosts_are_fetched() {
        let cases = [
            ("https://github.com/user-attachments/assets/1", true),
            ("https://api.github.com/x", true),
            (
                "https://private-user-images.githubusercontent.com/1.png",
                true,
            ),
            (
                "https://github-production-user-asset-6210df.s3.amazonaws.com/1.png",
                true,
            ),
            ("http://github.com/a.png", false),
            ("https://example.com/a.png", false),
            ("https://169.254.169.254/latest/meta-data", false),
            ("https://localhost/a.png", false),
            ("https://github.com.evil.example/a.png", false),
            ("https://x.githubusercontent.com@10.0.0.1/a.png", false),
            ("https://10.0.0.1\\.githubusercontent.com/a.png", false),
            (
                "https://user-images.githubusercontent.com:8080/a.png",
                false,
            ),
            ("file:///etc/passwd", false),
            ("https:///a.png", false),
        ];
        for (url, fetched) in cases {
            assert_eq!(allowed(url, "github.com").is_ok(), fetched, "{url}");
        }
    }

    /// A redirect names a full address, or a path on the host that
    /// redirects, whatever its query says; nothing else is followed.
    #[test]
    fn a_redirect_goes_where_it_says() {
        let from = "https://github.com/user-attachments/assets/1";
        let cases = [
            (
                "https://a.githubusercontent.com/b",
                Some("https://a.githubusercontent.com/b"),
            ),
            (
                "/login?return_to=https://evil.example",
                Some("https://github.com/login?return_to=https://evil.example"),
            ),
            ("//evil.example/a.png", None),
            ("a.png", None),
        ];
        for (location, to) in cases {
            assert_eq!(moved(from, location).ok().as_deref(), to, "{location}");
        }
    }

    /// A redirect is followed as a fetch of its own, so it can't take the
    /// fetch to a host rho-ui doesn't fetch from.
    #[test]
    #[cfg(unix)]
    fn a_redirect_is_checked_as_the_address_was() {
        let server = test::serve();
        let forge = Forge {
            host: "github.com".to_owned(),
            gh: None,
        };
        let there = |arguments: Vec<OsString>| {
            future::ready(local::test::sh(SCRIPT, &arguments).and_then(|output| parse(&output)))
        };
        let here = |arguments: Vec<OsString>| future::ready(fetch_here(&arguments));
        let moved = format!("{}/moved/hubot.png", server.address);
        let picture = futures::executor::block_on(follow(&moved, &forge, there)).unwrap();
        assert!(drawn(picture).is_ok(), "it draws");
        let away = format!("{}/away", server.address);
        let refused = futures::executor::block_on(follow(&away, &forge, here)).unwrap_err();
        assert!(matches!(refused, FetchError::Refused(_)), "{refused:?}");
        let paths: Vec<String> = server
            .requests()
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(paths, ["/moved/hubot.png", "/hubot.png", "/away"]);
        // Nor can the user's `.curlrc`, which says to follow them.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".curlrc"), "location\n").unwrap();
        let script = format!("export CURL_HOME='{}'\n{SCRIPT}", home.path().display());
        let printed = local::test::sh(&script, &arguments(&away, &forge)).unwrap();
        let response = parse(&printed).unwrap();
        assert!(
            matches!(answer(&response, &away), Ok(Answer::Moved(_))),
            "{response:?}"
        );
    }

    /// The script a remote machine runs fetches what this machine does,
    /// past what a login file prints before it, signed in with the token
    /// `gh` gives: sent as a header, to the forge's host alone.
    #[test]
    #[cfg(unix)]
    fn the_script_fetches_as_here() {
        let server = test::serve();
        let host = server.address.trim_start_matches("http://").to_owned();
        let bin = tempfile::tempdir().unwrap();
        let gh = bin.path().join("gh");
        std::fs::write(
            &gh,
            "#!/bin/sh\n[ \"$*\" = \"auth token --hostname $RHO_UI_HOST\" ] && echo secret\n"
                .replace("$RHO_UI_HOST", &host),
        )
        .unwrap();
        let runs = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&gh, runs).unwrap();
        // The test server speaks http: the script is given the token as
        // the Rust side would for an https URL on the host.
        let url = format!("{}/hubot.png", server.address);
        let most = |bytes: usize| OsString::from(bytes.to_string());
        let mut signed = vec![
            gh.clone().into_os_string(),
            host.into(),
            most(HEAD_ROOM + MAX_BYTES + 1),
        ];
        signed.extend(curl_arguments(&url));
        let mut printed = b"motd\n".to_vec();
        printed.extend(local::test::sh(SCRIPT, &signed).unwrap());
        let there = parse(&printed).unwrap();
        let here = fetch_here(&signed).unwrap();
        assert_eq!(there, here);
        let Ok(Answer::Picture(picture)) = answer(&here, &url) else {
            panic!("no picture in {here:?}");
        };
        assert!(drawn(picture.to_vec()).is_ok(), "it draws");
        let bearer = Some("Bearer secret".to_owned());
        assert_eq!(
            server.requests(),
            vec![
                ("/hubot.png".to_owned(), bearer.clone()),
                ("/hubot.png".to_owned(), bearer)
            ],
        );
        // An address on another host gets no token.
        let forge = Forge {
            host: "github.com".to_owned(),
            gh: Some(gh),
        };
        assert!(fetch_here(&arguments(&url, &forge)).is_ok());
        assert_eq!(
            server.requests().last(),
            Some(&("/hubot.png".to_owned(), None))
        );
        // Both stop reading past the most, headers and all, which the size
        // the server gives doesn't stop, and neither fails for it.
        let mut capped = signed.clone();
        capped[2] = most(64);
        let big = format!("{}/big-640x320.png", server.address);
        *capped.last_mut().unwrap() = big.into();
        let there = parse(&local::test::sh(SCRIPT, &capped).unwrap()).unwrap();
        assert_eq!((there.len(), fetch_here(&capped).unwrap().len()), (64, 64));
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
        assert!(drawn(bytes).is_err());
    }
}
