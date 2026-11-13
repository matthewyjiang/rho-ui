//! What a session's workspace is: project name and branch, read straight from
//! `.git` files. No `git` process, no git library.

use std::path::{Path, PathBuf};

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

    /// The repository name without its owner: what the badge shows.
    fn short_name(&self) -> &str {
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
    let name = std::fs::read_to_string(common_dir.join("config"))
        .ok()
        .and_then(|config| origin_name(&config))
        .or_else(|| {
            // Name a linked worktree after its main checkout, not its folder.
            common_dir
                .parent()
                .filter(|_| common_dir.file_name().is_some_and(|name| name == ".git"))
                .or(Some(work_tree.as_path()))
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| folder_name(cwd));
    let branch = std::fs::read_to_string(git_dir.join("HEAD"))
        .ok()
        .and_then(|head| branch_name(&head));
    // The main checkout holds the common dir as its `.git`. A bare
    // repository has no main checkout, so its worktree stands alone.
    let root = common_dir
        .parent()
        .filter(|_| common_dir.file_name().is_some_and(|name| name == ".git"))
        .map_or(work_tree, Path::to_path_buf);
    Repo { name, branch, root }
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
}
