//! What a session's workspace is: project name and branch. On this machine,
//! read straight from `.git` files: no `git` process, no git library. On a
//! remote machine, from [`REMOTE_SCRIPT`], which runs `git` there.

use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt as _,
    path::{Path, PathBuf},
};

/// A workspace's project, as the sidebar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo {
    /// `owner/name` from the origin remote, else the folder name.
    pub(crate) name: String,
    /// The checked-out branch, or a short commit hash when detached.
    /// `None` outside a git repository.
    pub(crate) branch: Option<String>,
    /// The project the workspace belongs to: the repository's main checkout,
    /// so a linked worktree or a subfolder names the same project as the
    /// checkout itself. Outside a git repository, the folder.
    pub(crate) root: PathBuf,
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
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

/// What to show for `cwd` before [`inspect`] has read it: the folder name.
pub(crate) fn folder(cwd: &Path) -> Repo {
    Repo {
        name: folder_name(cwd),
        branch: None,
        root: cwd.to_path_buf(),
    }
}

fn folder_name(cwd: &Path) -> String {
    cwd.file_name().map_or_else(
        || cwd.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Reads the project for `cwd`. Does file I/O; call it off the UI thread.
pub(crate) fn inspect(cwd: &Path) -> Repo {
    let Some((work_tree, git_dir)) = find_git_dir(cwd) else {
        return folder(cwd);
    };
    // A linked worktree keeps its config in the main repository's git dir.
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|dir| git_dir.join(dir.trim()))
        .and_then(|dir| dir.canonicalize().ok())
        .unwrap_or_else(|| git_dir.clone());
    let origin = std::fs::read_to_string(common_dir.join("config"))
        .ok()
        .and_then(|config| origin_name(&config));
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok();
    checkout(work_tree, &common_dir, origin, head.as_deref())
}

/// The project of a checkout at `work_tree`, whose repository keeps what
/// all its worktrees share in `common_dir`. `origin` names the repository;
/// without it, the project is named after its main checkout's folder, so a
/// linked worktree is not named after its own. `head` is what its `HEAD`
/// file holds.
fn checkout(
    work_tree: PathBuf,
    common_dir: &Path,
    origin: Option<String>,
    head: Option<&str>,
) -> Repo {
    let root = project_root(work_tree, common_dir);
    Repo {
        name: origin.unwrap_or_else(|| folder_name(&root)),
        branch: head.and_then(branch_name),
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
/// as its arguments, on a remote machine with `git`. For each, it prints
/// four fields, each ended by a NUL: the work tree root, the common git
/// dir, what `HEAD` holds (as its file would), and the origin remote's URL.
/// All four are empty outside a git repository. Read with [`parse_remote`].
pub(crate) const REMOTE_SCRIPT: &str = r#"unset CDPATH
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

/// The projects of `dirs`, from what [`REMOTE_SCRIPT`] printed for them.
/// `None` if it printed the wrong number of fields.
pub(crate) fn parse_remote(dirs: &[PathBuf], output: &[u8]) -> Option<Vec<Repo>> {
    let fields = nul_fields(output)?;
    let (facts, []) = fields.as_chunks::<4>() else {
        return None;
    };
    if facts.len() != dirs.len() {
        return None;
    }
    let path = |bytes: &[u8]| PathBuf::from(OsString::from_vec(bytes.to_vec()));
    let text =
        |bytes: &[u8]| (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned());
    let repos = dirs
        .iter()
        .zip(facts)
        .map(|(dir, [top, common, head, url])| {
            if top.is_empty() || common.is_empty() {
                return folder(dir);
            }
            checkout(
                path(top),
                &path(common),
                text(url).as_deref().and_then(url_name),
                text(head).as_deref(),
            )
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
    start.ancestors().find_map(|dir| {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some((dir.to_path_buf(), dot_git));
        }
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let target = pointer.strip_prefix("gitdir:")?.trim();
        Some((dir.to_path_buf(), dir.join(target)))
    })
}

/// `main` for `ref: refs/heads/main`, a short hash for a detached HEAD.
fn branch_name(head: &str) -> Option<String> {
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            Some(
                reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_owned(),
            )
        }
        None => head.get(..7).map(str::to_owned),
    }
}

/// `owner/name` from the `origin` remote's URL in a git config file.
fn origin_name(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == r#"[remote "origin"]"#;
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url").map(str::trim_start) {
                let url = url.strip_prefix('=')?.trim();
                return url_name(url);
            }
        }
    }
    None
}

/// The last two path segments of a remote URL, without `.git`:
/// `git@github.com:owner/name.git` and `https://host/owner/name` both give
/// `owner/name`.
fn url_name(url: &str) -> Option<String> {
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
            branch: None,
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
        assert_eq!(origin_name(config).as_deref(), Some("me/thing"));
    }

    #[test]
    fn head_gives_branch_or_short_hash() {
        assert_eq!(
            branch_name("ref: refs/heads/feat/x\n").as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            branch_name("0123456789abcdef\n").as_deref(),
            Some("0123456")
        );
    }

    /// A linked worktree: `.git` file pointing into the main repository. It
    /// and any subfolder belong to the main checkout's project.
    #[test]
    fn linked_worktrees_use_the_main_repository() {
        let root = std::env::temp_dir().join(format!("rho-ui-repo-test-{}", std::process::id()));
        let (main, tree) = fake_worktree(&root);

        assert_eq!(
            inspect(&main.join("docs")),
            Repo {
                name: "project".into(),
                branch: Some("main".into()),
                root: main.clone(),
            }
        );
        assert_eq!(
            inspect(&tree.join("src")),
            Repo {
                name: "project".into(),
                branch: Some("feature".into()),
                root: main.canonicalize().unwrap(),
            }
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// [`REMOTE_SCRIPT`], run with this machine's `git`, names each folder's
    /// project as reading its `.git` files here does: a main checkout with
    /// an origin, a linked worktree of it, a detached checkout, a folder
    /// outside any repository, and one that isn't there.
    #[test]
    fn the_remote_script_agrees_with_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
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
        let remote = parse_remote(&dirs, &output.stdout).expect("four fields a folder");

        let local: Vec<Repo> = dirs.iter().map(|dir| inspect(dir)).collect();
        assert_eq!(remote, local);
        assert_eq!(remote[1].name, "me/thing");
        assert_eq!(remote[1].branch.as_deref(), Some("feat"));
        assert_eq!(remote[1].root, dir.join("main"));
        assert_eq!(remote[3], folder(&dirs[3]));
    }
}
