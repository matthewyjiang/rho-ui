//! A lead's folders on its machine. A lead is a session opened as rho's
//! [`LEAD_AGENT`](rho_ui_core::LEAD_AGENT), which works in a folder of its
//! own, `<rho home>/leads/<name>`, named from its first message
//! ([`slug`]). Its draft works in the leads root, `<rho home>/leads`, which
//! holds the lead agent's settings until the first message goes. rho's
//! protocol makes no folder, so rho-ui makes them: [`make_here`] on this
//! machine, and [`SCRIPT`] on a remote one, over its link, by the same
//! rules.

use std::{
    ffi::OsString,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use gpui_kit::BackgroundExecutor;
use rho_ui_core::{platform, Daemon, SessionSetup};

use crate::folders::{Deadline, Reader};

/// The longest a lead's folder name runs, in characters, before [`slug`]
/// cuts it at a word: about six words, which a terminal's listing and the
/// sidebar show whole. A choice for reading, not a limit of any file
/// system.
const SLUG_CHARS: usize = 40;

/// A lead's folder name when its first message has no ASCII letter or
/// digit.
const NO_SLUG: &str = "lead";

/// Which folder [`Leads::open`] makes before it opens the lead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Folder {
    /// The leads root, made if missing, where a lead's draft works.
    Root,
    /// A lead's own folder in the root, named this, or with `-2`, `-3`, ...
    /// after it when that is taken.
    Lead(String),
}

impl Folder {
    /// The folder of a lead whose first message is `text`.
    pub(crate) fn named_from(text: &str) -> Self {
        Self::Lead(slug(text))
    }
}

/// The POSIX `sh` script that makes a folder of [`Folder`] on a remote
/// machine, given a lead's folder name as its argument, or an empty one for
/// the root, in the rho home found as rho-ui's ssh setup finds it. A taken
/// name gets `-2`, `-3`, ... after it. It prints a NUL, then the folder's
/// resolved path and a NUL, so whatever a login file prints comes before
/// them.
pub(crate) const SCRIPT: &str = r#"name=$1
cd || exit
root=${RHO_HOME:-$HOME/.rho}/leads
mkdir -p -- "$root" && root=$(cd -P -- "$root" && pwd -P) || exit
dest=$root
if [ -n "$name" ]; then
  dest=$root/$name
  n=1
  while [ -e "$dest" ] || [ -L "$dest" ]; do
    n=$((n + 1))
    dest=$root/$name-$n
  done
  mkdir -- "$dest" || exit
fi
printf '\0%s\0' "$dest"
"#;

/// A lead's folder name from its first message, `text`: its runs of ASCII
/// letters and digits, lowercased, joined by `-`, as many whole as fit in
/// [`SLUG_CHARS`]. A first run longer than that is cut; with none, it is
/// [`NO_SLUG`].
fn slug(text: &str) -> String {
    let mut slug = String::new();
    let words = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty());
    for word in words {
        if slug.is_empty() && word.len() > SLUG_CHARS {
            // ASCII, so a byte is a character.
            slug = word[..SLUG_CHARS].to_ascii_lowercase();
            break;
        }
        let joined = if slug.is_empty() { 0 } else { slug.len() + 1 };
        if joined + word.len() > SLUG_CHARS {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }
    if slug.is_empty() {
        NO_SLUG.to_owned()
    } else {
        slug
    }
}

/// A machine's leads root, and how rho-ui makes folders in it.
pub(crate) struct Leads {
    reader: Reader,
    /// This machine's rho home, for a machine `reader` reads here.
    rho_home: Option<PathBuf>,
    executor: BackgroundExecutor,
}

impl Leads {
    /// The leads of the machine `reader` reads: on this machine those of
    /// `rho_home`, whose folders are made on `executor`; on a remote one
    /// those of the rho home its login finds.
    pub(crate) fn new(
        reader: Reader,
        rho_home: Option<PathBuf>,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            reader,
            rho_home,
            executor,
        }
    }

    /// Makes `folder`, with [`make_here`] on this machine and [`SCRIPT`] on
    /// a remote one, then opens a lead in it on `daemon`.
    ///
    /// # Errors
    ///
    /// Says why, in the file system's, ssh's or the daemon's words, if the
    /// machine isn't connected, the folder can't be made, or the daemon
    /// won't open the lead, as a rho without the lead agent won't.
    pub(crate) async fn open(
        self,
        daemon: &Daemon,
        folder: Folder,
    ) -> Result<SessionSetup, String> {
        let name = match &folder {
            Folder::Root => String::new(),
            Folder::Lead(name) => name.clone(),
        };
        let rho_home = self.rho_home;
        let here = move || -> Result<PathBuf, String> {
            let rho_home = rho_home.ok_or("There's no home directory to keep leads in.")?;
            make_here(&rho_home.join("leads"), &folder)
        };
        let parse = |output: Vec<u8>| {
            // The last field is the path, after whatever a login file
            // printed.
            let mut fields = output.split(|&byte| byte == 0).filter(|f| !f.is_empty());
            fields
                .next_back()
                .map(platform::path_from_bytes)
                .ok_or_else(|| "the leads script printed no path".to_owned())
        };
        let args = vec![OsString::from(name)];
        let cwd = self
            .reader
            .query(&self.executor, here, SCRIPT, args, Deadline::Script, parse)
            .await?;
        daemon
            .new_lead(&cwd)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Makes `folder` in the leads root `root` on this machine, by
/// [`SCRIPT`]'s rules, and returns its resolved path. Does file I/O; call
/// it off the UI thread.
///
/// # Errors
///
/// Says why if the root can't be made or opened, or the lead's folder
/// can't be made for any reason but that its name is taken.
fn make_here(root: &Path, folder: &Folder) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("Couldn't make {}: {error}", root.display()))?;
    let root = platform::canonicalize(root)
        .map_err(|error| format!("Couldn't open {}: {error}", root.display()))?;
    let Folder::Lead(name) = folder else {
        return Ok(root);
    };
    let mut dest = root.join(name);
    let mut n = 1;
    loop {
        match std::fs::create_dir(&dest) {
            Ok(()) => return Ok(dest),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                n += 1;
                dest = root.join(format!("{name}-{n}"));
            }
            Err(error) => return Err(format!("Couldn't make {}: {error}", dest.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_message_names_its_leads_folder() {
        let table = [
            ("Fix the parser!", "fix-the-parser"),
            ("  --Ship v2.0 -- now?? ", "ship-v2-0-now"),
            ("Café crème", "caf-cr-me"),
            // Cut at the last word that fits in 40.
            (
                "Add SSH remote daemon support for many machines",
                "add-ssh-remote-daemon-support-for-many",
            ),
            // A first word longer than 40 is cut in it.
            (
                "Pneumonoultramicroscopicsilicovolcanoconiosis research",
                "pneumonoultramicroscopicsilicovolcanocon",
            ),
            ("日本語のテキスト", "lead"),
            ("", "lead"),
        ];
        for (text, expected) in table {
            assert_eq!(slug(text), expected, "{text:?}");
        }
    }

    /// A lead's folder goes in the leads root, made if missing; a taken
    /// name gets the next free number, and the root alone is taken as it
    /// is. `make` makes a folder in a rho home and returns its path.
    fn makes_the_next_free_folder(make: impl Fn(&Path, &Folder) -> Result<PathBuf, String>) {
        let rho_home = tempfile::tempdir().unwrap();
        let rho_home = rho_home.path();
        let root = || platform::canonicalize(&rho_home.join("leads")).unwrap();
        let lead = || Folder::Lead("foo".to_owned());
        assert_eq!(make(rho_home, &lead()), Ok(root().join("foo")));
        assert_eq!(make(rho_home, &lead()), Ok(root().join("foo-2")));
        assert_eq!(make(rho_home, &lead()), Ok(root().join("foo-3")));
        assert_eq!(make(rho_home, &Folder::Root), Ok(root()));
        assert!(root().join("foo-3").is_dir());
    }

    #[test]
    fn making_a_folder_here_takes_the_next_free_name() {
        makes_the_next_free_folder(|rho_home, folder| make_here(&rho_home.join("leads"), folder));
    }

    /// [`SCRIPT`], which makes a remote machine's, follows the same rules.
    #[cfg(unix)]
    #[test]
    fn the_script_takes_the_next_free_name() {
        makes_the_next_free_folder(|rho_home, folder| {
            let name = match folder {
                Folder::Root => "",
                Folder::Lead(name) => name.as_str(),
            };
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(SCRIPT)
                .arg("sh")
                .arg(name)
                .env("RHO_HOME", rho_home)
                .output()
                .unwrap();
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned());
            }
            let path = output.stdout.split(|&b| b == 0).rfind(|f| !f.is_empty());
            Ok(platform::path_from_bytes(path.unwrap()))
        });
    }
}
