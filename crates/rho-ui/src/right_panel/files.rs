//! The Files surface: the files in the folder of the session on screen,
//! as a tree, and any one of them opened read-only, its syntax
//! highlighted. In a git repository it lists what git sees: tracked files
//! that are there, and untracked ones `.gitignore` doesn't leave out;
//! outside one, every file under the folder. It reads them on the
//! session's machine when it opens, when another session comes on screen,
//! when the session's turn ends, and on Refresh, and the open file with
//! them.

use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    path::Path,
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::{Editor, EditorState},
    list::ListItem,
    spinner::Spinner,
    tree::{tree, TreeEntry, TreeEvent, TreeItem, TreeState},
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::{
    div, rems, AnyElement, AppContext as _, BackgroundExecutor, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window,
};

use crate::{
    folders::Reader,
    machines::Place,
    session_view::{SessionEvent, SessionView},
    workspaces::Workspaces,
};

/// The most of the file list the surface reads, in bytes of paths; past
/// it the list says it stopped. 8 MiB held 114,000 paths copied from a
/// 32,000-file monorepo (t3code); in a debug build, parsing and sorting
/// them off the UI thread took 235 ms and making the tree's items on it
/// 46 ms.
const LIST_LIMIT: usize = 8 << 20;

/// The largest file the surface opens, in bytes. Kit's editor is made for
/// up to about 50,000 lines: 2 MiB of rho-ui's own Rust is 55,000, which
/// the editor took in 215 ms in a debug build.
const FILE_LIMIT: usize = 2 << 20;

/// How much of a file is searched for a NUL to call it binary, as git
/// does.
const BINARY_PROBE: usize = 8000;

/// Printed before the file list, so whatever a login file prints comes
/// first and is skipped.
const LISTED: &[u8] = b"rho-ui-files\0";

/// Printed before a file's bytes.
const OPENED: &[u8] = b"rho-ui-file\0";

/// Printed instead when the file isn't there.
const GONE: &[u8] = b"rho-ui-file-gone\0";

/// The POSIX `sh` script that lists the files under the folder given as
/// its first argument, each path from that folder ended by a NUL, after
/// [`LISTED`]. In a git work tree, first the tracked files missing from it,
/// then a field `/` (which no path from the folder can be), then the
/// tracked and untracked files git doesn't ignore, those missing among
/// them. Outside one, `/` and then every file below the folder, each path
/// starting `./`. The list stops after as many bytes as the second
/// argument says. Read with [`parse_listing`].
pub(super) const LIST_SCRIPT: &str = r#"unset CDPATH
cd -- "$1" || exit
printf 'rho-ui-files\0'
if [ "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = true ]; then
  git ls-files -z --deleted || exit
  printf '/\0'
  git ls-files -z --cached --others --exclude-standard | head -c "$2"
else
  printf '/\0'
  find . ! -type d -print0 | head -c "$2"
fi
"#;

/// The POSIX `sh` script that prints the file at the path given as its
/// second argument, from the folder given as its first: [`OPENED`], then
/// as many of its bytes as the third argument says. [`GONE`] if it isn't a
/// file. Read with [`parse_file`].
pub(super) const READ_SCRIPT: &str = r#"unset CDPATH
cd -- "$1" || exit
if [ -f "$2" ]; then
  printf 'rho-ui-file\0'
  head -c "$3" -- "$2"
else
  printf 'rho-ui-file-gone\0'
fi
"#;

/// The files [`LIST_SCRIPT`] found.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listing {
    /// Each file's path from the folder, in the order listed.
    paths: Vec<String>,
    /// Whether the list stopped at its limit, leaving files out.
    cut: bool,
}

/// A folder or file of the tree, as [`nodes`] sorts them.
#[derive(Debug, PartialEq, Eq)]
struct Node {
    name: String,
    /// From the session's folder: the tree's ID for it.
    path: String,
    /// What a folder holds; `None` for a file.
    children: Option<Vec<Node>>,
}

/// What a read of the open file found.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Text(String),
    /// It holds a NUL, so it isn't text.
    Binary,
    /// Over [`FILE_LIMIT`].
    TooLarge,
    /// It isn't there, or isn't a file.
    Gone,
}

/// What the file list shows.
enum Status {
    /// Nothing read yet.
    Loading,
    /// The last read failed, and why.
    Failed(SharedString),
    /// The files in [`FilesSurface::tree`], which may be none.
    Ready,
}

/// The file opened from the tree.
struct OpenFile {
    /// From the session's folder.
    path: SharedString,
    /// `None` until the first read of it comes.
    read: Option<Result<Read, SharedString>>,
    /// Whether [`Self::read`] holds text the editor doesn't show yet. The
    /// editor takes it at the next render, as setting its text takes a
    /// window, which reads finish without.
    unshown: bool,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
}

/// The Files surface of [`super::RightPanel`]: it follows the session the
/// panel shows.
pub(super) struct FilesSurface {
    workspaces: Entity<Workspaces>,
    /// The session's folder, with the subscription that reads it again when
    /// its turn ends.
    session: Option<(Place, Subscription)>,
    tree: Entity<TreeState>,
    /// The folders opened in the tree, which a new list keeps open.
    expanded: HashSet<SharedString>,
    status: Status,
    /// The list in the tree, so a read that finds the same files leaves
    /// the tree, and its scroll position, alone.
    listing: Listing,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
    /// The file showing in place of the tree, if any.
    open: Option<OpenFile>,
    /// Shows the open file; made at the first render that needs it, as
    /// making one takes a window.
    editor: Option<Entity<EditorState>>,
    _expansions: Subscription,
}

impl FilesSurface {
    pub(super) fn new(
        workspaces: Entity<Workspaces>,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let expansions = cx.subscribe(&tree, |this: &mut Self, _, event: &TreeEvent, _| {
            match event {
                TreeEvent::Expanded(id) => this.expanded.insert(id.clone()),
                TreeEvent::Collapsed(id) => this.expanded.remove(id),
            };
        });
        let mut this = Self {
            workspaces,
            session: None,
            tree,
            expanded: HashSet::new(),
            status: Status::Loading,
            listing: Listing::default(),
            load: Task::ready(()),
            open: None,
            editor: None,
            _expansions: expansions,
        };
        this.set_session(session, cx);
        this
    }

    /// Shows the files where `session` works, or nothing without one. A
    /// session in the folder already showing keeps the tree as it is.
    pub(super) fn set_session(
        &mut self,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        let before = self.session.take().map(|(place, _)| place);
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
        if before.as_ref() != self.session.as_ref().map(|(place, _)| place) {
            self.status = Status::Loading;
            self.listing = Listing::default();
            self.expanded.clear();
            self.open = None;
            self.tree
                .update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
        }
        self.reload(cx);
    }

    /// Reads the file list again, and the open file, keeping what shows
    /// until they come.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some((place, _)) = &self.session else {
            self.load = Task::ready(());
            cx.notify();
            return;
        };
        let reader = self.workspaces.read(cx).reader(&place.machine);
        let args = vec![
            place.path.clone().into_os_string(),
            (LIST_LIMIT + 1).to_string().into(),
        ];
        let executor = cx.background_executor().clone();
        self.load = cx.spawn(async move |this, cx| {
            let read = list(reader, args, executor).await;
            let _ = this.update(cx, |this, cx| this.listed(read, cx));
        });
        if let Some(path) = self.open.as_ref().map(|open| open.path.clone()) {
            self.read_file(path, cx);
        }
    }

    fn listed(&mut self, read: Result<(Listing, Vec<Node>), String>, cx: &mut Context<'_, Self>) {
        self.status = match read {
            Ok((listing, _)) if listing == self.listing => Status::Ready,
            Ok((listing, nodes)) => {
                let items = items(&nodes, &self.expanded);
                self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
                self.listing = listing;
                Status::Ready
            }
            Err(why) => Status::Failed(why.into()),
        };
        cx.notify();
    }

    /// Shows the file at `path`, from the session's folder, in place of
    /// the tree.
    fn open_file(&mut self, path: SharedString, cx: &mut Context<'_, Self>) {
        self.open = Some(OpenFile {
            path: path.clone(),
            read: None,
            unshown: false,
            load: Task::ready(()),
        });
        self.read_file(path, cx);
    }

    /// Reads the open file at `path` again.
    fn read_file(&mut self, path: SharedString, cx: &mut Context<'_, Self>) {
        let (Some((place, _)), Some(open)) = (&self.session, &mut self.open) else {
            return;
        };
        let reader = self.workspaces.read(cx).reader(&place.machine);
        let args = vec![
            place.path.clone().into_os_string(),
            OsString::from(path.as_ref()),
            (FILE_LIMIT + 1).to_string().into(),
        ];
        let executor = cx.background_executor().clone();
        open.load = cx.spawn(async move |this, cx| {
            let read = reader
                .run_script(READ_SCRIPT, args, &executor)
                .await
                .and_then(|output| parse_file(&output));
            let _ = this.update(cx, |this, cx| this.file_read(&path, read, cx));
        });
        cx.notify();
    }

    fn file_read(
        &mut self,
        path: &SharedString,
        read: Result<Read, String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(open) = self.open.as_mut().filter(|open| open.path == *path) else {
            return;
        };
        let read = read.map_err(SharedString::from);
        // The same text again leaves the editor, and where it was
        // scrolled, alone.
        if open.read.as_ref() != Some(&read) {
            open.unshown = matches!(read, Ok(Read::Text(_)));
            open.read = Some(read);
            cx.notify();
        }
    }

    /// Goes back from the open file to the tree.
    fn close_file(&mut self, cx: &mut Context<'_, Self>) {
        self.open = None;
        cx.notify();
    }

    /// The open file's path and the text its editor shows, for UI tests.
    #[cfg(test)]
    pub(super) fn open_file_text(&self, cx: &gpui_kit::App) -> Option<(String, String)> {
        let open = self.open.as_ref()?;
        let text = match (&open.read, &self.editor) {
            (Some(Ok(Read::Text(_))), Some(editor)) if !open.unshown => {
                editor.read(cx).value().to_string()
            }
            _ => return None,
        };
        Some((open.path.to_string(), text))
    }

    /// The folder's name and how many files it holds, and Refresh.
    fn render_summary(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let count = self.listing.paths.len();
        let mut text = match count {
            1 => "1 file".to_owned(),
            count => format!("{count} files"),
        };
        if self.listing.cut {
            text = format!(
                "The first {text}: the list stops at {} MiB of paths",
                LIST_LIMIT >> 20
            );
        }
        let folder = self
            .session
            .as_ref()
            .and_then(|(place, _)| place.path.file_name())
            .map_or_default(|name| name.to_string_lossy().into_owned());
        let theme = cx.theme();
        h_flex()
            .id("files-summary")
            .test_support()
            .aria_label(text.clone())
            .flex_shrink_0()
            .gap_2()
            .pl_4()
            .pr_3()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(folder),
            )
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(refresh_button(cx))
    }

    /// The tree of files; a click on a file opens it.
    fn render_tree(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        tree(&self.tree, move |_, entry, _, _, _| {
            render_entry(entry, &view, muted)
        })
        .flex_1()
        .min_h_0()
    }

    /// The open file: Back, its path and Refresh, over its text.
    fn render_file(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let open = self.open.as_mut()?;
        let path = open.path.clone();
        let body = match &open.read {
            None => message("Reading the file…", Some(Spinner::new().into_any_element())),
            Some(Err(why)) => message(why.clone(), None),
            Some(Ok(Read::Gone)) => message("This file isn't there anymore.", None),
            Some(Ok(Read::Binary)) => message("This file isn't text.", None),
            Some(Ok(Read::TooLarge)) => message(
                format!(
                    "This file is over {} MiB, too large to show here.",
                    FILE_LIMIT >> 20
                ),
                None,
            ),
            Some(Ok(Read::Text(text))) => {
                let editor = self.editor.get_or_insert_with(|| {
                    cx.new(|cx| EditorState::new(window, cx).folding(false))
                });
                if std::mem::take(&mut open.unshown) {
                    let language = language(&path);
                    let text = text.clone();
                    editor.update(cx, |editor, cx| {
                        editor.set_highlighter(language, cx);
                        editor.set_value(text, window, cx);
                    });
                }
                Editor::new(editor)
                    .readonly(true)
                    .bordered(false)
                    .h_full()
                    .aria_label(path.clone())
                    .into_any_element()
            }
        };
        let theme = cx.theme();
        let header = h_flex()
            .id("files-open-file")
            .test_support()
            .flex_shrink_0()
            .gap_1()
            .pl_2()
            .pr_3()
            .py_1()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("files-back")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ArrowLeft))
                    .accessibility_label("Back to the files")
                    .tooltip("Back to the files")
                    .on_click(cx.listener(|this, _, _, cx| this.close_file(cx))),
            )
            .child(div().flex_1().min_w_0().truncate().child(path))
            .child(refresh_button(cx));
        Some(
            v_flex()
                .size_full()
                .child(header)
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element(),
        )
    }
}

/// A row of the tree: a folder's icon, open or shut, or a file's, and its
/// name, indented by its depth. A file's row opens it in `view`.
fn render_entry(
    entry: &TreeEntry,
    view: &WeakEntity<FilesSurface>,
    muted: gpui_kit::Hsla,
) -> ListItem {
    let item = entry.item();
    let icon = match (entry.is_folder(), entry.is_expanded()) {
        (true, true) => Lucide::FolderOpen,
        (true, false) => Lucide::Folder,
        (false, _) => Lucide::File,
    };
    let depth = u16::try_from(entry.depth()).unwrap_or(u16::MAX);
    let indent = 0.75 + f32::from(depth);
    let row = ListItem::new(SharedString::from(format!("files-entry-{}", item.id)))
        .accessibility_label(item.label.clone())
        .pl(rems(indent))
        .pr_2()
        .py_0p5()
        .text_sm()
        .child(
            h_flex()
                .gap_2()
                .min_w_0()
                .child(Icon::new(icon).small().text_color(muted))
                .child(div().min_w_0().truncate().child(item.label.clone())),
        );
    if entry.is_folder() {
        // The tree opens and shuts a folder on its own.
        return row;
    }
    let (view, path) = (view.clone(), item.id.clone());
    row.on_click(move |_, _, cx| {
        let _ = view.update(cx, |this, cx| this.open_file(path.clone(), cx));
    })
}

fn refresh_button(cx: &Context<'_, FilesSurface>) -> Button {
    Button::new("files-refresh")
        .ghost()
        .xsmall()
        .icon(IconName::RotateCw)
        .tooltip("Refresh")
        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
}

/// A message in place of the files.
fn message(text: impl Into<SharedString>, media: Option<AnyElement>) -> AnyElement {
    let mut header = EmptyHeader::new().description(EmptyDescription::new().child(text.into()));
    if let Some(media) = media {
        header = header.media(EmptyMedia::new().child(media));
    }
    Empty::new().h_full().header(header).into_any_element()
}

impl Render for FilesSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if self.session.is_none() {
            return message("Open a session to see its files.", None);
        }
        if let Some(file) = self.render_file(window, cx) {
            return file;
        }
        match &self.status {
            Status::Loading => message(
                "Reading the files…",
                Some(Spinner::new().into_any_element()),
            ),
            Status::Failed(why) => v_flex()
                .size_full()
                .child(message(why.clone(), None))
                .child(h_flex().justify_center().pb_4().child(refresh_button(cx)))
                .into_any_element(),
            Status::Ready if self.listing.paths.is_empty() => v_flex()
                .size_full()
                .child(message("This folder has no files.", None))
                .child(h_flex().justify_center().pb_4().child(refresh_button(cx)))
                .into_any_element(),
            Status::Ready => v_flex()
                .size_full()
                .child(self.render_summary(cx))
                .child(self.render_tree(cx))
                .into_any_element(),
        }
    }
}

/// The token Kit's highlighter finds a syntax by: the file's extension,
/// else its name, as for a `Makefile`.
fn language(path: &str) -> String {
    let path = Path::new(path);
    path.extension()
        .or_else(|| path.file_name())
        .map_or_default(|token| token.to_string_lossy().into_owned())
}

/// Lists the files with `reader` and [`LIST_SCRIPT`]'s `args`, and sorts
/// them into a tree, both on `executor`.
async fn list(
    reader: Reader,
    args: Vec<OsString>,
    executor: BackgroundExecutor,
) -> Result<(Listing, Vec<Node>), String> {
    let output = reader.run_script(LIST_SCRIPT, args, &executor).await?;
    executor
        .spawn(async move {
            let listing = parse_listing(&output, LIST_LIMIT)?;
            let nodes = nodes(&listing.paths);
            Ok((listing, nodes))
        })
        .await
}

/// Where `marker` first shows in `output`.
fn find(output: &[u8], marker: &[u8]) -> Option<usize> {
    output
        .windows(marker.len())
        .position(|window| window == marker)
}

/// The files [`LIST_SCRIPT`] printed, told to stop after `limit + 1`
/// bytes.
fn parse_listing(output: &[u8], limit: usize) -> Result<Listing, String> {
    let printed = || "The file list script printed something else.".to_owned();
    let at = find(output, LISTED).ok_or_else(printed)?;
    let rest = &output[at + LISTED.len()..];
    // No path ends with `/`, so the first `/` ending a field is the
    // separator, and one that starts the output is too.
    let split = find(rest, b"/\0").ok_or_else(printed)?;
    let missing: HashSet<&[u8]> = fields(&rest[..split]).into_iter().collect();
    let mut present = &rest[split + 2..];
    let cut = present.len() > limit;
    if cut {
        // Drop the path the limit cut short.
        let end = present
            .iter()
            .rposition(|&byte| byte == 0)
            .map_or(0, |at| at + 1);
        present = &present[..end];
    }
    let paths = fields(present)
        .into_iter()
        .filter(|path| !missing.contains(path))
        .map(|path| {
            let path = path.strip_prefix(b"./").unwrap_or(path);
            String::from_utf8_lossy(path).into_owned()
        })
        .collect();
    Ok(Listing { paths, cut })
}

/// The NUL-ended fields of `bytes`, leaving out empty ones.
fn fields(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|&byte| byte == 0)
        .filter(|field| !field.is_empty())
        .collect()
}

/// `paths` as a tree: in each folder, its folders and then its files, each
/// by name, ignoring case.
fn nodes(paths: &[String]) -> Vec<Node> {
    #[derive(Default)]
    struct Folder<'a> {
        folders: BTreeMap<&'a str, Folder<'a>>,
        files: Vec<&'a str>,
    }

    fn sorted<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
        let mut names: Vec<&str> = names.collect();
        names.sort_by_cached_key(|name| (name.to_lowercase(), *name));
        names
    }

    fn into_nodes(mut folder: Folder<'_>, prefix: &str) -> Vec<Node> {
        let path = |name: &str| {
            if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            }
        };
        let names = sorted(folder.folders.keys().copied());
        let folders = names.into_iter().map(|name| {
            let inner = folder.folders.remove(name).unwrap_or_default();
            let path = path(name);
            Node {
                name: name.to_owned(),
                children: Some(into_nodes(inner, &path)),
                path,
            }
        });
        let mut nodes: Vec<Node> = folders.collect();
        nodes.extend(
            sorted(folder.files.into_iter())
                .into_iter()
                .map(|name| Node {
                    name: name.to_owned(),
                    path: path(name),
                    children: None,
                }),
        );
        nodes
    }

    let mut root = Folder::default();
    for path in paths {
        let (parents, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mut folder = &mut root;
        for part in parents.split('/').filter(|part| !part.is_empty()) {
            folder = folder.folders.entry(part).or_default();
        }
        folder.files.push(name);
    }
    into_nodes(root, "")
}

/// The tree's items for `nodes`, the folders in `expanded` open.
fn items(nodes: &[Node], expanded: &HashSet<SharedString>) -> Vec<TreeItem> {
    nodes
        .iter()
        .map(|node| {
            let id = SharedString::from(node.path.clone());
            let item = TreeItem::new(id.clone(), node.name.clone());
            match &node.children {
                None => item,
                Some(children) => item
                    .expanded(expanded.contains(&id))
                    .children(items(children, expanded)),
            }
        })
        .collect()
}

/// The file [`READ_SCRIPT`] printed, told to stop after
/// [`FILE_LIMIT`]` + 1` bytes.
fn parse_file(output: &[u8]) -> Result<Read, String> {
    if let Some(at) = find(output, OPENED) {
        let bytes = &output[at + OPENED.len()..];
        return Ok(if bytes.len() > FILE_LIMIT {
            Read::TooLarge
        } else if bytes[..bytes.len().min(BINARY_PROBE)].contains(&0) {
            Read::Binary
        } else {
            Read::Text(String::from_utf8_lossy(bytes).into_owned())
        });
    }
    match find(output, GONE) {
        Some(_) => Ok(Read::Gone),
        None => Err("The file script printed something else.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use crate::folders::run_here;

    use super::*;

    /// [`LIST_SCRIPT`] lists what git sees in a work tree, from any folder
    /// of it: tracked files that are there and untracked ones not ignored.
    /// Outside one it lists every file, and it stops at its limit with
    /// whole paths only.
    #[test]
    fn the_list_script_lists_what_git_sees() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        let write = |path: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x\n").unwrap();
        };
        let list = |folder: &Path, limit: usize| {
            let output = run_here(
                LIST_SCRIPT,
                &[folder.as_os_str(), (limit + 1).to_string().as_ref()],
            );
            let mut listing = parse_listing(&output.unwrap(), limit).unwrap();
            listing.paths.sort();
            listing
        };
        let listing = |paths: &[&str], cut| Listing {
            paths: paths.iter().map(|&path| path.to_owned()).collect(),
            cut,
        };

        write("plain/a.txt");
        write("plain/sub/b.txt");
        assert_eq!(
            list(&dir.join("plain"), LIST_LIMIT),
            listing(&["a.txt", "sub/b.txt"], false),
            "outside git, every file"
        );

        git(&["init", "-q", "-b", "main", "repo"]);
        for path in [
            ".gitignore",
            "src/main.rs",
            "src/gone.rs",
            "target/out.o",
            "new.md",
        ] {
            write(&format!("repo/{path}"));
        }
        std::fs::write(dir.join("repo/.gitignore"), "target/\n").unwrap();
        git(&["-C", "repo", "add", ".gitignore", "src"]);
        git(&["-C", "repo", "commit", "-q", "-m", "x"]);
        std::fs::remove_file(dir.join("repo/src/gone.rs")).unwrap();
        let repo = dir.join("repo");
        assert_eq!(
            list(&repo, LIST_LIMIT),
            listing(&[".gitignore", "new.md", "src/main.rs"], false)
        );
        assert_eq!(
            list(&repo.join("src"), LIST_LIMIT),
            listing(&["main.rs"], false),
            "from a subfolder, its files"
        );
        // ".gitignore\0new.md\0" is 18 bytes: 20 cuts `src/main.rs`.
        assert_eq!(list(&repo, 20), listing(&[".gitignore", "new.md"], true));
    }

    /// Folders come before files, and names sort ignoring case.
    #[test]
    fn the_tree_puts_folders_first_by_name() {
        let paths = ["b.txt", "src/z.rs", "A.txt", "Docs/x.md", "src/a/b.rs"];
        let paths: Vec<String> = paths.iter().map(|&path| path.to_owned()).collect();
        let names = |nodes: &[Node]| -> Vec<String> {
            nodes.iter().map(|node| node.path.clone()).collect()
        };
        let tree = nodes(&paths);
        assert_eq!(names(&tree), ["Docs", "src", "A.txt", "b.txt"]);
        let src = tree[1].children.as_deref().unwrap();
        assert_eq!(names(src), ["src/a", "src/z.rs"]);
        assert_eq!(names(src[0].children.as_deref().unwrap()), ["src/a/b.rs"]);
    }

    /// [`READ_SCRIPT`] reads a text file whole, and tells a binary file,
    /// one over the limit, and one that isn't there apart.
    #[test]
    fn the_read_script_tells_text_from_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("bin"), b"\x7fELF\0\0").unwrap();
        std::fs::write(dir.path().join("big"), vec![b'x'; FILE_LIMIT + 1]).unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        let read = |path: &str| {
            let limit = (FILE_LIMIT + 1).to_string();
            let args = [dir.path().as_os_str(), path.as_ref(), limit.as_ref()];
            parse_file(&run_here(READ_SCRIPT, &args).unwrap())
        };
        assert_eq!(read("a.rs"), Ok(Read::Text("fn main() {}\n".to_owned())));
        assert_eq!(read("bin"), Ok(Read::Binary));
        assert_eq!(read("big"), Ok(Read::TooLarge));
        assert_eq!(read("gone"), Ok(Read::Gone));
        assert_eq!(read("folder"), Ok(Read::Gone));
    }
}
