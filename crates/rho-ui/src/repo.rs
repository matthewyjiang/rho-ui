//! What a session's workspace is: project name and branch. On this machine,
//! read straight from `.git` files: no `git` process, no git library. On a
//! remote machine, from [`REMOTE_SCRIPT`], which runs `git` there.

use std::path::{Path, PathBuf};

use rho_ui_core::platform;

use crate::folders;

/// A workspace's project, as the sidebar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo {
    /// `owner/name` from the origin remote, else the folder name.
    pub(crate) name: String,
    /// The origin remote's URL, which clones it on another machine.
    pub(crate) origin: Option<String>,
    /// What is checked out. `None` outside a git repository.
    pub(crate) head: Option<Head>,
    /// The project the workspace belongs to: the repository's main checkout,
    /// so a linked worktree or a subfolder names the same project as the
    /// checkout itself. Outside a git repository, the folder.
    pub(crate) root: PathBuf,
}

/// What a checkout has checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    Branch(String),
    /// No branch: a commit, by its short hash.
    Detached(String),
}

impl Head {
    /// The branch, or the short hash, as the sidebar shows it.
    pub(crate) fn name(&self) -> &str {
        match self {
            Head::Branch(name) | Head::Detached(name) => name,
        }
    }
}

impl Repo {
    /// Two letters for the project badge: the first letters of the first
    /// two words of the repository name ("rho-ui" is "RU"), or the first and
    /// last letter of a one-word name ("rho" is "RO").
    pub(crate) fn initials(&self) -> String {
        let mut words = self
            .short_name()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty());
        let first = words.next().unwrap_or("?");
        let mut letters = first.chars();
        let initials: String = match words.next() {
            Some(second) => letters
                .next()
                .into_iter()
                .chain(second.chars().next())
                .collect(),
            None => letters.next().into_iter().chain(letters.last()).collect(),
        };
        initials.to_uppercase()
    }

    /// The repository name without its owner: what the badge and the title
    /// bar show.
    pub(crate) fn short_name(&self) -> &str {
        without_owner(&self.name)
    }
}

/// `name`, an `owner/name` repository name, without its owner: the name of
/// the folder a clone of it makes.
pub(crate) fn without_owner(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// What to show for `cwd` before [`inspect`] has read it: the folder name.
pub(crate) fn folder(cwd: &Path) -> Repo {
    Repo {
        name: folder_name(cwd),
        origin: None,
        head: None,
        root: cwd.to_path_buf(),
    }
}

/// The last part of `path`, or all of it when it has none, as `/`.
pub(crate) fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Reads the project for `cwd`. Does file I/O; call it off the UI thread.
/// A checkout's root is resolved, as a remote machine's `pwd -P` and the
/// project picker give it, so one project has one path however a session
/// reached it, as through a symlink.
pub(crate) fn inspect(cwd: &Path) -> Repo {
    let Some((work_tree, git_dir)) = find_git_dir(cwd) else {
        return folder(cwd);
    };
    let work_tree = platform::canonicalize(&work_tree).unwrap_or(work_tree);
    let git_dir = platform::canonicalize(&git_dir).unwrap_or(git_dir);
    // A linked worktree keeps its config in the main repository's git dir.
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|dir| git_dir.join(dir.trim()))
        .and_then(|dir| platform::canonicalize(&dir).ok())
        .unwrap_or_else(|| git_dir.clone());
    let origin = std::fs::read_to_string(common_dir.join("config"))
        .ok()
        .and_then(|config| origin_url(&config));
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok();
    checkout(work_tree, &common_dir, origin, head.as_deref())
}

/// The project of a checkout at `work_tree`, whose repository keeps what
/// all its worktrees share in `common_dir`. `origin`, the origin remote's
/// URL, names the repository; without it, the project is named after its
/// main checkout's folder, so a linked worktree is not named after its own.
/// `head` is what its `HEAD` file holds.
fn checkout(
    work_tree: PathBuf,
    common_dir: &Path,
    origin: Option<String>,
    head: Option<&str>,
) -> Repo {
    let root = project_root(work_tree, common_dir);
    Repo {
        name: origin
            .as_deref()
            .and_then(url_name)
            .unwrap_or_else(|| folder_name(&root)),
        origin,
        head: head.and_then(parse_head),
        root,
    }
}

/// The project folder of a checkout at `work_tree` whose repository keeps
/// what all its worktrees share in `common_dir`: the main checkout, which
/// holds the common dir as its `.git`. A bare repository has no main
/// checkout, so its worktree stands alone.
pub(crate) fn project_root(work_tree: PathBuf, common_dir: &Path) -> PathBuf {
    common_dir
        .parent()
        .filter(|_| common_dir.file_name().is_some_and(|name| name == ".git"))
        .map_or(work_tree, Path::to_path_buf)
}

/// The POSIX `sh` script that reads the repositories of the folders given
/// as its arguments, on a remote machine with `git`. After [`READ`], for
/// each it prints four fields, each ended by a NUL: the work tree root, the
/// common git dir, what `HEAD` holds (as its file would), and the origin
/// remote's URL. All four are empty outside a git repository. Read with
/// [`parse_remote`].
pub(crate) const REMOTE_SCRIPT: &str = r#"unset CDPATH
printf 'rho-ui-repos\0'
for dir do
  if top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null); then
    common=$(cd "$dir" && cd "$(git rev-parse --git-common-dir)" && pwd -P)
    if ref=$(git -C "$dir" symbolic-ref -q HEAD); then
      head="ref: $ref"
    else
      head=$(git -C "$dir" rev-parse -q --verify HEAD)
    fi
    url=$(git -C "$dir" config --get remote.origin.url)
  else
    top= common= head= url=
  fi
  printf '%s\0%s\0%s\0%s\0' "$top" "$common" "$head" "$url"
done
"#;

/// Printed before [`REMOTE_SCRIPT`]'s fields, so whatever a login file
/// prints comes first and is skipped.
const READ: &[u8] = b"rho-ui-repos\0";

/// The projects of `dirs`, from what [`REMOTE_SCRIPT`] printed for them.
/// `None` if it printed something else, or the wrong number of fields.
pub(crate) fn parse_remote(dirs: &[PathBuf], output: &[u8]) -> Option<Vec<Repo>> {
    let fields = nul_fields(folders::after(output, READ)?)?;
    let (facts, []) = fields.as_chunks::<4>() else {
        return None;
    };
    if facts.len() != dirs.len() {
        return None;
    }
    let path = platform::path_from_bytes;
    let text =
        |bytes: &[u8]| (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned());
    let repos = dirs
        .iter()
        .zip(facts)
        .map(|(dir, [top, common, head, url])| {
            if top.is_empty() || common.is_empty() {
                return folder(dir);
            }
            checkout(path(top), &path(common), text(url), text(head).as_deref())
        })
        .collect();
    Some(repos)
}

/// The fields of a remote script's `output`, each of which it ended with a
/// NUL. `None` if the output doesn't end with one.
pub(crate) fn nul_fields(output: &[u8]) -> Option<Vec<&[u8]>> {
    let mut fields: Vec<&[u8]> = output.split(|&byte| byte == 0).collect();
    // Each field ends with a NUL, so splitting leaves one empty piece last.
    fields.pop().filter(|last| last.is_empty())?;
    Some(fields)
}

/// The work tree root and git dir for `start`, searching upward. `.git` is a
/// directory in a normal checkout and a `gitdir: <path>` file in a linked
/// worktree or submodule.
fn find_git_dir(start: &Path) -> Option<(PathBuf, PathBuf)> {
    start
        .ancestors()
        .find_map(|dir| Some((dir.to_path_buf(), git_dir_at(dir)?)))
}

/// The git dir of `dir` when it is a work tree's root: its `.git`
/// directory, or where its `.git` file points.
pub(crate) fn git_dir_at(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = read_small(&dot_git)?;
    let target = pointer.strip_prefix("gitdir:")?.trim();
    Some(dir.join(target))
}

/// The most a git pointer file (`.git`, `HEAD`) is read of: a path, which
/// Linux caps at 4096 bytes (`PATH_MAX`), after a short tag.
const SMALL_FILE: u64 = 4096 + 64;

/// The start of the regular file at `path`, as git's small pointer files
/// are read. Nothing for anything else, such as a FIFO, which would block,
/// or `/dev/zero`, which never ends, in a folder another user shares.
pub(crate) fn read_small(path: &Path) -> Option<String> {
    use std::io::Read as _;

    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(SMALL_FILE)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Branch `main` for `ref: refs/heads/main`, a short hash for a detached
/// HEAD.
pub(crate) fn parse_head(head: &str) -> Option<Head> {
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            Some(Head::Branch(name.to_owned()))
        }
        None => head.get(..7).map(|hash| Head::Detached(hash.to_owned())),
    }
}

/// The `origin` remote's URL in a git config file.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == r#"[remote "origin"]"#;
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url").map(str::trim_start) {
                return Some(url.strip_prefix('=')?.trim().to_owned());
            }
        }
    }
    None
}

/// The last two path segments of a remote URL, without `.git`:
/// `git@github.com:owner/name.git` and `https://host/owner/name` both give
/// `owner/name`.
pub(crate) fn url_name(url: &str) -> Option<String> {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    let mut segments = path
        .rsplit(['/', ':'])
        .filter(|segment| !segment.is_empty());
    let name = segments.next()?;
    Some(match segments.next() {
        Some(owner) if !owner.contains('@') && !owner.contains('.') => format!("{owner}/{name}"),
        _ => name.to_owned(),
    })
}

/// The web page of the repository at remote `url`, as the title bar links
/// it: `git@host:owner/name.git`, `ssh://git@host:22/owner/name` and
/// `https://user:token@host/owner/name.git` all give
/// `https://host/owner/name`. Any credentials in the URL are dropped, as is
/// an ssh port, which the web server doesn't share. `None` for a remote
/// with no host, such as a local path or a `file://` URL.
pub(crate) fn web_url(url: &str) -> Option<String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        // scp-like `[user@]host:path`, as git reads it: a `/` before the
        // `:` makes it a path, as does one letter, a Windows drive.
        let (authority, path) = url.split_once(':')?;
        return if authority.contains(['/', '\\']) || authority.len() == 1 {
            None
        } else {
            web_page("https", without_user(authority), path)
        };
    };
    let (authority, path) = rest.split_once('/')?;
    let host = without_user(authority);
    match scheme {
        "http" | "https" => web_page(scheme, host, path),
        "ssh" | "git" | "git+ssh" | "ssh+git" => {
            web_page("https", host.split(':').next().unwrap_or(host), path)
        }
        _ => None,
    }
}

/// `authority` without its `user[:password]@`.
fn without_user(authority: &str) -> &str {
    authority.rsplit('@').next().unwrap_or(authority)
}

/// `scheme://host/path`, without a trailing `/` or `.git` on the path.
fn web_page(scheme: &str, host: &str, path: &str) -> Option<String> {
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{scheme}://{host}/{path}"))
}

/// For tests: a checkout `project` with a linked worktree `wt` beside it,
/// under `dir`, made of bare `.git` files. Returns both folders; the
/// worktree has a `src` subfolder and the checkout a `docs` one.
#[cfg(test)]
pub(crate) fn fake_worktree(dir: &Path) -> (PathBuf, PathBuf) {
    let main = dir.join("project");
    let tree = dir.join("wt");
    let git = main.join(".git");
    std::fs::create_dir_all(git.join("worktrees/wt")).unwrap();
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::create_dir_all(main.join("docs")).unwrap();
    std::fs::write(git.join("config"), "[core]\n").unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(git.join("worktrees/wt/HEAD"), "ref: refs/heads/feature\n").unwrap();
    std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();
    std::fs::write(
        tree.join(".git"),
        format!("gitdir: {}\n", git.join("worktrees/wt").display()),
    )
    .unwrap();
    (main, tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_owned(),
            origin: None,
            head: None,
            root: PathBuf::from("/"),
        }
    }

    #[test]
    fn initials_follow_t3_style() {
        assert_eq!(repo("rho-ui").initials(), "RU");
        assert_eq!(repo("matthewyjiang/rho").initials(), "RO");
        assert_eq!(repo("dux-shell").initials(), "DS");
        assert_eq!(repo("x").initials(), "X");
    }

    #[test]
    fn remote_urls_give_a_web_page_without_credentials() {
        let page = Some("https://github.com/me/thing");
        for url in [
            "git@github.com:me/thing.git",
            "github.com:me/thing",
            "ssh://git@github.com:22/me/thing.git",
            "git://github.com/me/thing",
            "https://me:ghp_secret@github.com/me/thing.git/",
        ] {
            assert_eq!(web_url(url).as_deref(), page, "{url}");
        }
        assert_eq!(
            web_url("http://git.lab:8080/group/sub/thing").as_deref(),
            Some("http://git.lab:8080/group/sub/thing")
        );
        for local in [
            "/srv/git/thing.git",
            "../thing",
            "file:///srv/git/thing.git",
            r"C:\code\thing",
            "https://github.com",
        ] {
            assert_eq!(web_url(local), None, "{local}");
        }
    }

    #[test]
    fn remote_urls_give_owner_and_name() {
        assert_eq!(
            url_name("git@github.com:matthewyjiang/rho.git").as_deref(),
            Some("matthewyjiang/rho")
        );
        assert_eq!(
            url_name("https://github.com/matthewyjiang/rho").as_deref(),
            Some("matthewyjiang/rho")
        );
        assert_eq!(
            url_name("/srv/git/project.git").as_deref(),
            Some("git/project")
        );
        let config = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@x.com:a/b.git\n[remote \"origin\"]\n\turl = git@github.com:me/thing.git\n";
        assert_eq!(
            origin_url(config).as_deref(),
            Some("git@github.com:me/thing.git")
        );
    }

    #[test]
    fn head_gives_branch_or_short_hash() {
        assert_eq!(
            parse_head("ref: refs/heads/feat/x\n"),
            Some(Head::Branch("feat/x".into()))
        );
        assert_eq!(
            parse_head("0123456789abcdef\n"),
            Some(Head::Detached("0123456".into()))
        );
    }

    /// A checkout reached through a symlink belongs to the project at its
    /// resolved path.
    // Making a symlink on Windows needs Developer Mode or an administrator.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_checkout_uses_its_resolved_path() {
        let root = std::env::temp_dir().join(format!("rho-ui-link-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (main, _) = fake_worktree(&root);
        let link = root.join("link");
        std::os::unix::fs::symlink(&main, &link).unwrap();

        assert_eq!(
            inspect(&link.join("docs")),
            Repo {
                name: "project".into(),
                origin: None,
                head: Some(Head::Branch("main".into())),
                root: platform::canonicalize(&main).unwrap(),
            }
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A linked worktree: `.git` file pointing into the main repository. It
    /// and any subfolder belong to the main checkout's project, at its
    /// resolved path.
    #[test]
    fn linked_worktrees_use_the_main_repository() {
        let root = std::env::temp_dir().join(format!("rho-ui-repo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (main, tree) = fake_worktree(&root);

        assert_eq!(
            inspect(&tree.join("src")),
            Repo {
                name: "project".into(),
                origin: None,
                head: Some(Head::Branch("feature".into())),
                root: platform::canonicalize(&main).unwrap(),
            }
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// [`REMOTE_SCRIPT`], run with this machine's `git`, names each folder's
    /// project as reading its `.git` files here does: a main checkout with
    /// an origin, a linked worktree of it, a detached checkout, a folder
    /// outside any repository, and one that isn't there.
    // The script runs on remote machines, which are Unix.
    #[cfg(unix)]
    #[test]
    fn the_remote_script_agrees_with_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let dir = platform::canonicalize(dir.path()).unwrap();
        let git = |args: &[&str]| crate::local::test::git(&dir, args);
        git(&["init", "-q", "-b", "main", "main"]);
        git(&[
            "-C",
            "main",
            "remote",
            "add",
            "origin",
            "git@github.com:me/thing.git",
        ]);
        git(&["-C", "main", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&[
            "-C",
            "main",
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            "../it's a tree",
        ]);
        git(&[
            "-C",
            "main",
            "worktree",
            "add",
            "-q",
            "--detach",
            "../detached",
        ]);
        std::fs::create_dir(dir.join("plain")).unwrap();
        let dirs: Vec<PathBuf> = ["main", "it's a tree", "detached", "plain", "gone"]
            .iter()
            .map(|name| dir.join(name))
            .collect();

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(REMOTE_SCRIPT)
            .arg("sh")
            .args(&dirs)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        // As a login file that prints would have it.
        let output = [b"Welcome!\n".as_slice(), output.stdout.as_slice()].concat();
        let remote = parse_remote(&dirs, &output).expect("four fields a folder");

        let local: Vec<Repo> = dirs.iter().map(|dir| inspect(dir)).collect();
        assert_eq!(remote, local);
        assert_eq!(remote[1].name, "me/thing");
        assert_eq!(remote[1].head, Some(Head::Branch("feat".into())));
        assert_eq!(remote[1].root, dir.join("main"));
        assert_eq!(remote[3], folder(&dirs[3]));
    }
}
