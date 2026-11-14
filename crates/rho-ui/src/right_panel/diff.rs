//! The Diff surface: the uncommitted changes in the repository of the
//! session on screen, staged or not, with its untracked files, as `git` on
//! the session's machine reports them, drawn with Kit's diff view. It reads
//! them when it opens, when another session comes on screen, when the
//! session's turn ends, and on Refresh.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffFile, DiffState},
    empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    spinner::Spinner,
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::{
    div, AnyElement, AppContext as _, BackgroundExecutor, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window,
};

use crate::{
    folders::Reader,
    machines::Place,
    session_view::{SessionEvent, SessionView},
    workspaces::Workspaces,
};

/// Printed before the patch, so whatever a login file prints comes first
/// and is skipped. A patch holds no NUL (git calls a file with one binary),
/// so the marker can't appear in it.
const CHANGES: &[u8] = b"rho-ui-diff\0";

/// Printed instead when the folder isn't in a git repository.
const NO_GIT: &[u8] = b"rho-ui-diff-none\0";

/// The POSIX `sh` script that prints the changes in the repository of the
/// folder given as its argument: from its top, every change to tracked
/// files against `HEAD` (against the empty tree before the first commit),
/// then each untracked file that isn't ignored, as a new file. After
/// [`CHANGES`], or [`NO_GIT`] outside a repository. Read with [`parse`].
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

/// The Diff surface of [`super::RightPanel`]: it follows the session the
/// panel shows.
pub(super) struct DiffSurface {
    workspaces: Entity<Workspaces>,
    /// The session's folder, with the subscription that reads it again when
    /// its turn ends.
    session: Option<(Place, Subscription)>,
    state: Entity<DiffState>,
    status: Status,
    /// The patch on screen, so a read that finds the same changes leaves
    /// the view, and its scroll position, alone.
    patch: String,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
}

impl DiffSurface {
    pub(super) fn new(
        workspaces: Entity<Workspaces>,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            workspaces,
            session: None,
            state: cx.new(|cx| DiffState::new([], cx)),
            status: Status::Loading,
            patch: String::new(),
            load: Task::ready(()),
        };
        this.set_session(session, cx);
        this
    }

    /// Shows the changes where `session` works, or nothing without one.
    pub(super) fn set_session(
        &mut self,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        self.session = session.map(|view| {
            let live = view.read(cx).live();
            let place = Place::new(live.id.machine, live.cwd);
            let ended = cx.subscribe(view, |this, _, event: &SessionEvent, cx| {
                if matches!(event, SessionEvent::Saved) {
                    this.reload(cx);
                }
            });
            (place, ended)
        });
        self.status = Status::Loading;
        self.patch.clear();
        self.state.update(cx, |state, cx| state.set_files([], cx));
        self.reload(cx);
    }

    /// Reads the session's changes again, keeping what shows until they
    /// come.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some((place, _)) = &self.session else {
            self.load = Task::ready(());
            cx.notify();
            return;
        };
        let reader = self.workspaces.read(cx).reader(&place.machine);
        let cwd = place.path.clone();
        let executor = cx.background_executor().clone();
        self.load = cx.spawn(async move |this, cx| {
            let read = read(reader, cwd, executor).await;
            let _ = this.update(cx, |this, cx| this.loaded(read, cx));
        });
    }

    fn loaded(&mut self, read: Result<Option<String>, String>, cx: &mut Context<'_, Self>) {
        self.status = match read {
            Ok(None) => Status::NotGit,
            Ok(Some(patch)) if patch == self.patch => Status::Ready,
            Ok(Some(patch)) => match DiffFile::parse(&patch) {
                Ok(files) => {
                    self.state
                        .update(cx, |state, cx| state.set_files(files, cx));
                    self.patch = patch;
                    Status::Ready
                }
                Err(error) => Status::Failed(format!("Couldn't read git's diff: {error}").into()),
            },
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
        let count = match files.len() {
            1 => "1 file changed".to_owned(),
            count => format!("{count} files changed"),
        };
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

/// A message in place of the changes.
fn message(text: impl Into<SharedString>, media: Option<AnyElement>) -> AnyElement {
    let mut header = EmptyHeader::new().description(EmptyDescription::new().child(text.into()));
    if let Some(media) = media {
        header = header.media(EmptyMedia::new().child(media));
    }
    Empty::new().h_full().header(header).into_any_element()
}

impl Render for DiffSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if self.session.is_none() {
            return message("Open a session to see its changes.", None);
        }
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
                .child(message("No uncommitted changes.", None))
                .child(h_flex().justify_center().pb_4().child(refresh_button(cx)))
                .into_any_element(),
            Status::Ready => v_flex()
                .size_full()
                .child(self.render_summary(cx))
                .child(Diff::new(&self.state).flex_1().min_h_0())
                .into_any_element(),
        }
    }
}

/// Reads the changes at `cwd` with `reader`, running `git` on `executor`
/// on this machine. `None` outside a git repository.
async fn read(
    reader: Reader,
    cwd: PathBuf,
    executor: BackgroundExecutor,
) -> Result<Option<String>, String> {
    let output = match reader {
        Reader::Here => executor.spawn(async move { run_here(&cwd) }).await?,
        Reader::There(ssh) => ssh
            .run_script(SCRIPT, &[&cwd])
            .await
            .map_err(|error| error.to_string())?,
        Reader::Unread => return Err("The session's machine isn't connected.".to_owned()),
    };
    parse(&output)
}

/// Runs [`SCRIPT`] on this machine for `cwd`. Blocks until git is done;
/// call it off the UI thread.
fn run_here(cwd: &Path) -> Result<Vec<u8>, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("sh")
        .arg(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Couldn't run sh: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(match stderr.trim() {
            "" => format!("git ended with {}", output.status),
            why => why.to_owned(),
        })
    }
}

/// The patch [`SCRIPT`] printed, or `None` if it found no repository.
fn parse(output: &[u8]) -> Result<Option<String>, String> {
    let find = |marker: &[u8]| {
        output
            .windows(marker.len())
            .position(|window| window == marker)
    };
    if let Some(at) = find(CHANGES) {
        let patch = &output[at + CHANGES.len()..];
        return Ok(Some(String::from_utf8_lossy(patch).into_owned()));
    }
    match find(NO_GIT) {
        Some(_) => Ok(None),
        None => Err("The diff script printed nothing.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`SCRIPT`] finds every kind of uncommitted change from any folder of
    /// the repository, staged or not, untracked files included and ignored
    /// ones left out, and says when a folder is in no repository.
    #[test]
    fn the_script_lists_uncommitted_changes() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
        let changes = |folder: &PathBuf| {
            let patch = parse(&run_here(folder).unwrap()).unwrap();
            let files = DiffFile::parse(&patch.expect("a repository")).unwrap();
            let mut paths: Vec<String> = files.iter().map(|f| f.path().to_string()).collect();
            paths.sort();
            paths
        };

        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir(dir.join("src")).unwrap();
        write("src/staged.rs", "fn a() {}\n");
        write(".gitignore", "target/\n");
        assert_eq!(
            changes(&dir),
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
        assert_eq!(changes(&dir), expected);
        assert_eq!(changes(&dir.join("src")), expected, "from a subfolder");

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(parse(&run_here(plain.path()).unwrap()), Ok(None));
    }

    #[test]
    fn output_before_the_marker_is_skipped() {
        let mut output = b"motd from .bashrc\n".to_vec();
        output.extend(CHANGES);
        output.extend(b"diff --git a/x b/x\n");
        assert_eq!(parse(&output), Ok(Some("diff --git a/x b/x\n".to_owned())));
        assert!(parse(b"nothing").is_err());
    }
}
