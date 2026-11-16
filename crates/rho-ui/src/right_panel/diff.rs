//! The Diff surface: the uncommitted changes in the repository of the
//! session its tab belongs to, staged or not, with its untracked files, as
//! `git` on the session's machine reports them, drawn with Kit's diff view.
//! It reads them when it opens, when its session comes back on screen or
//! its turn ends ([`super::RightPanel`] says when), and on Refresh, and
//! parses them off the UI thread.
//! A patch over [`PATCH_LIMIT`] shows the whole files that fit, and says
//! where it stopped.

use std::path::{Path, PathBuf};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffFile, DiffFileStatus, DiffState},
    h_flex,
    spinner::Spinner,
    v_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, AppContext as _, BackgroundExecutor, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window,
};

use rho_ui_core::platform;

use super::message;
use crate::{
    folders::{find, Deadline, Reader},
    local,
    machines::Place,
    workspaces::Workspaces,
};

/// Printed before the patch, so whatever a login file prints comes first
/// and is skipped. A patch holds no NUL (git calls a file with one binary),
/// so the marker can't appear in it.
const CHANGES: &[u8] = b"rho-ui-diff\0";

/// Printed instead when the folder isn't in a git repository.
const NO_GIT: &[u8] = b"rho-ui-diff-none\0";

/// The most of the patch the surface parses and shows, in bytes; past it,
/// the whole files that fit, and the summary says where it stopped.
/// In a debug build, an 8 MiB patch of 363 new Rust files parsed in 79 ms
/// off the UI thread, and giving it to Kit's diff view took 26 ms on it,
/// with 7 ms frames after; 2 MiB took 27 ms and 5 ms.
const PATCH_LIMIT: usize = 8 << 20;

/// Where each file's part of a patch starts, after the newline that ends
/// the file before.
const FILE_START: &[u8] = b"\ndiff --git ";

/// `git diff` as [`SCRIPT`] runs it, for a patch Kit's diff view reads.
const DIFF: &[&str] = &[
    "-c",
    "core.quotepath=off",
    "diff",
    "--no-color",
    "--no-ext-diff",
];

/// The POSIX `sh` script that prints the changes in the repository of the
/// folder given as its argument: from its top, every change to tracked
/// files against `HEAD` (against the empty tree before the first commit),
/// then each untracked file that isn't ignored, as a new file. After
/// [`CHANGES`], or [`NO_GIT`] outside a repository. Read with [`parse`];
/// [`diff_here`] does the same here.
/// GNU `xargs` runs its command once even with no files, so the command
/// skips an empty name; `git diff --no-index` exits 1 when the files
/// differ, which is every time here.
pub(super) const SCRIPT: &str = r#"unset CDPATH
if ! top=$(cd -- "$1" 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null); then
  printf 'rho-ui-diff-none\0'
  exit 0
fi
cd -- "$top" || exit
base=HEAD
git rev-parse -q --verify HEAD >/dev/null || base=$(git hash-object -t tree /dev/null)
printf 'rho-ui-diff\0'
git -c core.quotepath=off diff --no-color --no-ext-diff "$base" -- || exit
git ls-files -z --others --exclude-standard |
  xargs -0 -n 1 sh -c '[ -n "$1" ] || exit 0
    git -c core.quotepath=off diff --no-color --no-ext-diff --no-index -- /dev/null "$1"
    [ $? -le 1 ]' sh
"#;

/// A repository's changes, whole files only, as [`cut`] keeps them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Patch {
    text: String,
    /// Whether files past [`PATCH_LIMIT`] were left out.
    cut: bool,
}

/// What a read of the changes found.
enum Changes {
    /// The session's folder isn't in a git repository.
    NotGit,
    /// The patch, and the files parsed from it.
    Patch(Patch, Vec<DiffFile>),
}

/// What the surface shows.
enum Status {
    /// Nothing read yet.
    Loading,
    /// The session's folder isn't in a git repository.
    NotGit,
    /// The last read failed, and why.
    Failed(SharedString),
    /// The changes in [`DiffSurface::state`], which may be none.
    Ready,
}

/// The Diff surface of [`super::RightPanel`], for one session.
pub(super) struct DiffSurface {
    workspaces: Entity<Workspaces>,
    /// The session's folder.
    place: Place,
    state: Entity<DiffState>,
    status: Status,
    /// The patch on screen, so a read that finds the same changes leaves
    /// the view, and its scroll position, alone.
    patch: Patch,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
}

impl DiffSurface {
    pub(super) fn new(
        workspaces: Entity<Workspaces>,
        place: Place,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            workspaces,
            place,
            state: cx.new(|cx| DiffState::new([], cx)),
            status: Status::Loading,
            patch: Patch::default(),
            load: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    /// Reads the session's changes again, keeping what shows until they
    /// come.
    pub(super) fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let place = &self.place;
        let reader = self.workspaces.read(cx).reader(&place.machine);
        let cwd = place.path.clone();
        let executor = cx.background_executor().clone();
        self.load = cx.spawn(async move |this, cx| {
            let read = read(reader, cwd, executor).await;
            let _ = this.update(cx, |this, cx| this.loaded(read, cx));
        });
    }

    fn loaded(&mut self, read: Result<Changes, String>, cx: &mut Context<'_, Self>) {
        self.status = match read {
            Ok(Changes::NotGit) => Status::NotGit,
            Ok(Changes::Patch(patch, _)) if patch == self.patch => Status::Ready,
            Ok(Changes::Patch(patch, files)) => {
                self.state
                    .update(cx, |state, cx| state.set_files(files, cx));
                self.patch = patch;
                Status::Ready
            }
            Err(why) => Status::Failed(why.into()),
        };
        cx.notify();
    }

    /// The files changed, for UI tests.
    #[cfg(test)]
    pub(super) fn paths(&self, cx: &gpui_kit::App) -> Vec<String> {
        let files = self.state.read(cx).files();
        files.iter().map(|file| file.path().to_string()).collect()
    }

    /// "3 files changed, +12 −4", and Refresh.
    fn render_summary(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let files = self.state.read(cx).files();
        let (added, deleted) = files.iter().fold((0, 0), |(added, deleted), file| {
            (added + file.additions(), deleted + file.deletions())
        });
        let mut count = match files.len() {
            1 => "1 file changed".to_owned(),
            count => format!("{count} files changed"),
        };
        if self.patch.cut {
            count = format!(
                "The first {count}: the diff stops at {} MiB",
                PATCH_LIMIT >> 20
            );
        }
        let theme = cx.theme();
        h_flex()
            .id("diff-summary")
            .test_support()
            .aria_label(count.clone())
            .flex_shrink_0()
            .gap_2()
            .px_4()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().child(count))
            .child(div().text_color(theme.green).child(format!("+{added}")))
            .child(div().text_color(theme.red).child(format!("−{deleted}")))
            .child(div().flex_1())
            .child(refresh_button(cx))
    }
}

fn refresh_button(cx: &Context<'_, DiffSurface>) -> Button {
    Button::new("diff-refresh")
        .ghost()
        .xsmall()
        .icon(IconName::RotateCw)
        .tooltip("Refresh")
        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
}

/// A file's header: its path (old → new for a rename), lines added in
/// green and removed in red, as the summary above shows them, and how the
/// file changed when it was added, deleted or renamed.
fn file_header(file: &DiffFile, _: &mut Window, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme();
    let name: SharedString = match (file.original_path(), file.modified_path()) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}").into(),
        _ => file.path().clone(),
    };
    let status = match file.status() {
        DiffFileStatus::Added => Some("Added"),
        DiffFileStatus::Deleted => Some("Deleted"),
        DiffFileStatus::Renamed => Some("Renamed"),
        DiffFileStatus::Copied => Some("Copied"),
        _ if file.is_binary() => Some("Binary"),
        _ => None,
    };
    let counted = file.additions() + file.deletions() > 0;
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_medium()
                .child(name),
        )
        .when(counted, |this| {
            this.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .text_color(theme.green)
                            .child(format!("+{}", file.additions())),
                    )
                    .child(
                        div()
                            .text_color(theme.red)
                            .child(format!("−{}", file.deletions())),
                    ),
            )
        })
        .children(status.map(|status| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(status)
        }))
}

impl Render for DiffSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        match &self.status {
            Status::Loading => message(
                "Reading the changes…",
                Some(Spinner::new().into_any_element()),
            ),
            Status::NotGit => message("This session's folder isn't in a git repository.", None),
            Status::Failed(why) => v_flex()
                .size_full()
                .child(message(why.clone(), None))
                .child(h_flex().justify_center().pb_4().child(refresh_button(cx)))
                .into_any_element(),
            Status::Ready if self.state.read(cx).files().is_empty() => v_flex()
                .size_full()
                .child(if self.patch.cut {
                    message(
                        format!(
                            "The changes are over {} MiB, too large to show here.",
                            PATCH_LIMIT >> 20
                        ),
                        None,
                    )
                } else {
                    message("No uncommitted changes.", None)
                })
                .child(h_flex().justify_center().pb_4().child(refresh_button(cx)))
                .into_any_element(),
            Status::Ready => v_flex()
                .size_full()
                .child(self.render_summary(cx))
                .child(
                    Diff::new(&self.state)
                        .soft_wrap(true)
                        .render_header(file_header)
                        .flex_1()
                        .min_h_0(),
                )
                .into_any_element(),
        }
    }
}

/// Reads the changes at `cwd` with `reader`, and parses them, both on
/// `executor` (`git` runs there on this machine).
async fn read(
    reader: Reader,
    cwd: PathBuf,
    executor: BackgroundExecutor,
) -> Result<Changes, String> {
    let args = vec![cwd.as_os_str().to_owned()];
    let patch = reader
        .query(
            &executor,
            move || diff_here(&cwd, PATCH_LIMIT),
            SCRIPT,
            args,
            Deadline::Script,
            |output| parse(&output, PATCH_LIMIT),
        )
        .await?;
    let Some(patch) = patch else {
        return Ok(Changes::NotGit);
    };
    executor
        .spawn(async move {
            let files = DiffFile::parse(&patch.text)
                .map_err(|error| format!("Couldn't read git's diff: {error}"))?;
            Ok(Changes::Patch(patch, files))
        })
        .await
}

/// The changes in the repository of `cwd` on this machine, as [`SCRIPT`]
/// finds them, or `None` outside one. Past `limit` bytes, it keeps the
/// whole files that fit. Blocks; call it off the UI thread.
///
/// # Errors
///
/// Says why, in git's words, if git can't diff the repository or one of
/// its untracked files.
fn diff_here(cwd: &Path, limit: usize) -> Result<Option<Patch>, String> {
    // Not a folder, no git, or not in a repository, as the script tells.
    let Ok(top) = local::line(&mut local::git(cwd, &["rev-parse", "--show-toplevel"])) else {
        return Ok(None);
    };
    let top = platform::path_from_bytes(&top);
    let base = base(&top)?;
    let mut patch = local::stdout(local::git(&top, DIFF).args([base.as_str(), "--"]))?;
    let untracked = ["ls-files", "-z", "--others", "--exclude-standard"];
    let untracked = local::stdout(&mut local::git(&top, &untracked)).unwrap_or_default();
    for name in untracked
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
    {
        // Past this, more files can't change what `cut` keeps.
        if patch.len() >= limit + FILE_START.len() {
            break;
        }
        let mut command = local::git(&top, DIFF);
        command
            .args(["--no-index", "--", "/dev/null"])
            .arg(platform::path_from_bytes(name));
        let output = local::run(&mut command)?;
        // 1 says the files differ, which they do every time here.
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(local::failure(&command, &output));
        }
        patch.extend(output.stdout);
    }
    Ok(Some(cut(&patch, limit)))
}

/// What [`diff_here`] diffs the repository at `top` against: `HEAD`, or
/// before the first commit the empty tree.
///
/// # Errors
///
/// Says why, in git's words, if git can't hash the empty tree.
fn base(top: &Path) -> Result<String, String> {
    if local::succeeds(&mut local::git(
        top,
        &["rev-parse", "-q", "--verify", "HEAD"],
    )) {
        return Ok("HEAD".to_owned());
    }
    // Hashed from an empty stdin, as the script hashes `/dev/null`.
    let empty = local::line(&mut local::git(
        top,
        &["hash-object", "-t", "tree", "--stdin"],
    ))?;
    Ok(String::from_utf8_lossy(&empty).into_owned())
}

/// The patch [`SCRIPT`] printed, or `None` if it found no repository. Past
/// `limit` bytes, it keeps the whole files that fit.
fn parse(output: &[u8], limit: usize) -> Result<Option<Patch>, String> {
    if let Some(at) = find(output, CHANGES) {
        return Ok(Some(cut(&output[at + CHANGES.len()..], limit)));
    }
    match find(output, NO_GIT) {
        Some(_) => Ok(None),
        None => Err("The diff script printed nothing.".to_owned()),
    }
}

/// The whole files of `patch` within `limit` bytes.
fn cut(mut patch: &[u8], limit: usize) -> Patch {
    let cut = patch.len() > limit;
    if cut {
        // Up to the newline ending the last file within the limit,
        // whose end is the next file's start.
        let end = patch[..(limit + FILE_START.len() - 1).min(patch.len())]
            .windows(FILE_START.len())
            .rposition(|window| window == FILE_START)
            .map_or(0, |at| at + 1);
        patch = &patch[..end];
    }
    let text = String::from_utf8_lossy(patch).into_owned();
    Patch { text, cut }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finds every kind of uncommitted change from any folder of the
    /// repository, staged or not, untracked files included and ignored ones
    /// left out, and says when a folder is in no repository. `read` reads
    /// the changes at a folder.
    fn lists_uncommitted_changes(read: impl Fn(&Path) -> Option<Patch>) {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let git = |args: &[&str]| local::test::git(dir, args);
        let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
        let changes = |folder: &Path| {
            let patch = read(folder).expect("a repository");
            let files = DiffFile::parse(&patch.text).unwrap();
            let mut paths: Vec<String> = files.iter().map(|f| f.path().to_string()).collect();
            paths.sort();
            paths
        };

        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir(dir.join("src")).unwrap();
        write("src/staged.rs", "fn a() {}\n");
        write(".gitignore", "target/\n");
        assert_eq!(
            changes(dir),
            [".gitignore", "src/staged.rs"],
            "before the first commit, every file is new"
        );

        git(&["add", "."]);
        git(&["commit", "-q", "-m", "x"]);
        write("kept.txt", "kept\n");
        git(&["add", "kept.txt"]);
        write("src/staged.rs", "fn b() {}\n");
        write("it's new.md", "new\n");
        std::fs::create_dir(dir.join("target")).unwrap();
        write("target/out.o", "ignored\n");
        let expected = ["it's new.md", "kept.txt", "src/staged.rs"];
        assert_eq!(changes(dir), expected);
        assert_eq!(changes(&dir.join("src")), expected, "from a subfolder");

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(read(plain.path()), None, "no repository");
    }

    #[test]
    fn diffing_here_lists_uncommitted_changes() {
        lists_uncommitted_changes(|folder| diff_here(folder, PATCH_LIMIT).unwrap());
    }

    /// [`SCRIPT`], which reads a remote machine's changes, lists the same.
    #[cfg(unix)]
    #[test]
    fn the_script_lists_uncommitted_changes() {
        lists_uncommitted_changes(|folder| {
            parse(&local::test::sh(SCRIPT, &[folder]).unwrap(), PATCH_LIMIT).unwrap()
        });
    }

    /// What a login file prints before the marker is skipped, and a patch
    /// over the limit keeps only the whole files that fit, as a file cut
    /// short won't parse.
    #[test]
    fn parse_skips_what_comes_first_and_keeps_whole_files() {
        let patch = |text: &str, cut: bool| -> Result<Option<Patch>, String> {
            let text = text.to_owned();
            Ok(Some(Patch { text, cut }))
        };
        let mut output = b"motd from .bashrc\n".to_vec();
        output.extend(CHANGES);
        output.extend(b"diff --git a/x b/x\n");
        assert_eq!(
            parse(&output, PATCH_LIMIT),
            patch("diff --git a/x b/x\n", false)
        );
        // 19 bytes a file: 30 falls in the second, 10 in the first.
        output.extend(b"diff --git a/y b/y\n");
        assert_eq!(parse(&output, 30), patch("diff --git a/x b/x\n", true));
        assert_eq!(parse(&output, 10), patch("", true));
        assert!(parse(b"nothing", PATCH_LIMIT).is_err());
    }
}
