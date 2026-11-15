//! The forges rho-ui reaches through their command-line tools, such as
//! GitHub through `gh`, on each machine: where the tool is, its version,
//! and the accounts it is signed in to, which Settings → Source control
//! shows. A machine's tools are checked once something asks
//! ([`Forges::ensure_checked`]), again on Refresh, and afresh after its
//! daemon reconnects; never on a timer.
//!
//! To add a CLI: add a [`ForgeCli`] variant, with its check script and
//! parser in a module of its own, as [`gh`] has.

mod gh;
mod gh_pulls;

use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fmt,
    path::PathBuf,
    pin::pin,
    time::Duration,
};

use futures::future::{self, Either};
use gpui_kit::{Context, Entity, SharedString, Task};

use gpui_kit::BackgroundExecutor;

use crate::{
    folders::Reader,
    machines::MachineId,
    pull_requests::model::{Batch, FetchPlan, PrAction, PrId},
    settings::Settings,
    workspaces::Workspaces,
};

/// How long a CLI may take to answer before rho-ui calls it stuck: a
/// tripwire, as long as a script over ssh gets. Checking gh took 0.35 s
/// here (53 ms for `--version`, 283 ms for `auth status`, which asks
/// GitHub), and a batched PR query 0.6 to 0.9 s. A run on this machine that
/// goes past it, as when packets to GitHub are dropped, keeps its `gh`
/// running until that gives up.
const DEADLINE: Duration = Duration::from_secs(60);

/// Runs `cli`'s `script` with `args` through `reader`, giving up after
/// [`DEADLINE`].
async fn run(
    cli: ForgeCli,
    reader: Reader,
    script: &'static str,
    args: Vec<OsString>,
    executor: &BackgroundExecutor,
) -> Result<Vec<u8>, String> {
    let deadline = executor.timer(DEADLINE);
    let run = reader.run_script(script, args, executor);
    match future::select(pin!(run), deadline).await {
        Either::Left((output, _)) => output,
        Either::Right(_) => Err(format!(
            "{} didn't answer within {} s.",
            cli.program(),
            DEADLINE.as_secs()
        )),
    }
}

/// Why a PR query got no answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QueryError {
    /// The forge refused the CLI's token, or it has none: a new check of
    /// the CLI would see that.
    SignedOut(String),
    Failed(String),
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::SignedOut(why) | QueryError::Failed(why) => f.write_str(why),
        }
    }
}

/// Asks `host`, through `gh` at `program` run by `reader`, what `plan`
/// asks: one query, however much it holds. Pull requests are GitHub's
/// alone so far, so they take no [`ForgeCli`].
///
/// # Errors
///
/// When nothing in the query was answered, and why.
pub(crate) async fn query_prs(
    reader: Reader,
    program: PathBuf,
    host: &str,
    plan: &FetchPlan,
    executor: &BackgroundExecutor,
) -> Result<Batch, QueryError> {
    let query = gh_pulls::query(plan);
    let args = vec![program.into_os_string(), host.into(), query.into()];
    let output = run(ForgeCli::Gh, reader, gh_pulls::SCRIPT, args, executor)
        .await
        .map_err(QueryError::Failed)?;
    gh_pulls::parse(plan, &output)
}

/// Does `action` to PR `pr` through `gh` at `program`, run by `reader`.
///
/// # Errors
///
/// When it didn't: what `gh` said.
pub(crate) async fn act(
    reader: Reader,
    program: PathBuf,
    pr: &PrId,
    action: &PrAction,
    executor: &BackgroundExecutor,
) -> Result<(), String> {
    let repo = &pr.repo;
    let mut args = vec![
        program.into_os_string(),
        repo.host.clone().into(),
        format!("{}/{}", repo.owner, repo.name).into(),
        pr.number.to_string().into(),
    ];
    args.extend(gh_pulls::action_args(action));
    let output = run(
        ForgeCli::Gh,
        reader,
        gh_pulls::ACTION_SCRIPT,
        args,
        executor,
    )
    .await?;
    gh_pulls::parse_action(&output)
}

/// A forge's command-line tool that rho-ui knows how to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ForgeCli {
    /// GitHub's CLI.
    Gh,
}

impl ForgeCli {
    /// Every CLI, in the order Settings lists them.
    pub(crate) const ALL: [ForgeCli; 1] = [ForgeCli::Gh];

    /// The program's usual name, which is also its key in the settings
    /// file and its part of element IDs.
    pub(crate) fn program(self) -> &'static str {
        match self {
            ForgeCli::Gh => "gh",
        }
    }

    /// The CLI named `program`, as the settings file writes it.
    pub(crate) fn from_program(program: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cli| cli.program() == program)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            ForgeCli::Gh => "GitHub CLI",
        }
    }

    /// The forge it reaches.
    pub(crate) fn forge_name(self) -> &'static str {
        match self {
            ForgeCli::Gh => "GitHub",
        }
    }

    /// Where to get it.
    pub(crate) fn install_url(self) -> &'static str {
        match self {
            ForgeCli::Gh => "https://cli.github.com",
        }
    }

    /// The command that signs it in, to `host` or to the forge's own
    /// site, which a person runs in a terminal, as it asks questions.
    pub(crate) fn login_command(self, host: Option<&str>) -> String {
        match (self, host) {
            (ForgeCli::Gh, Some(host)) => format!("gh auth login --hostname {host}"),
            (ForgeCli::Gh, None) => "gh auth login".to_owned(),
        }
    }

    fn script(self) -> &'static str {
        match self {
            ForgeCli::Gh => gh::SCRIPT,
        }
    }

    fn parse(self, output: &[u8]) -> Result<CliProbe, String> {
        match self {
            ForgeCli::Gh => gh::parse(output),
        }
    }
}

/// The program of each CLI on one machine, as Settings → Source control
/// sets it: an absolute or `~/` path, or a name to find on the `PATH`. A
/// CLI with none is found on the machine; see [`FoundBy`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CliPaths(BTreeMap<ForgeCli, String>);

impl CliPaths {
    /// `cli`'s program; empty when none is set.
    pub(crate) fn get(&self, cli: ForgeCli) -> &str {
        self.0.get(&cli).map_or("", String::as_str)
    }

    /// Sets `cli`'s program; empty clears it.
    pub(crate) fn set(&mut self, cli: ForgeCli, path: String) {
        if path.is_empty() {
            self.0.remove(&cli);
        } else {
            self.0.insert(cli, path);
        }
    }

    /// Each CLI that has a program set, and its program.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (ForgeCli, &str)> {
        self.0.iter().map(|(cli, path)| (*cli, path.as_str()))
    }
}

/// How the program was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FoundBy {
    /// Settings name it.
    Setting,
    /// On the `PATH` that scripts on the machine run with.
    Path,
    /// Only through the user's login shell, whose `PATH` a non-interactive
    /// ssh session may lack.
    LoginShell,
}

/// What checking a CLI on a machine found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CliProbe {
    /// Not on the `PATH`, nor through the login shell, and Settings name
    /// no program.
    Missing,
    /// Found, but it doesn't run or doesn't answer as rho-ui needs, and why.
    Broken {
        path: PathBuf,
        found_by: FoundBy,
        why: SharedString,
    },
    Found(CliStatus),
}

/// A CLI that runs, and what it says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CliStatus {
    pub(crate) path: PathBuf,
    pub(crate) found_by: FoundBy,
    /// Its version, such as `2.99.0`.
    pub(crate) version: SharedString,
    /// Every account it knows, on every host.
    pub(crate) accounts: Vec<Account>,
}

/// An account a CLI is signed in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    /// Such as `github.com`.
    pub(crate) host: SharedString,
    /// Empty when the forge refused the token, as it then can't say whose
    /// it was.
    pub(crate) login: SharedString,
    /// Whether the CLI uses this account for its host; a host can hold
    /// several.
    pub(crate) active: bool,
    pub(crate) auth: AuthState,
    /// How it clones: `ssh` or `https`.
    pub(crate) git_protocol: Option<SharedString>,
    pub(crate) token_source: Option<TokenSource>,
    pub(crate) scopes: Option<SharedString>,
}

/// Where an account's token comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenSource {
    /// An environment variable, such as `GH_TOKEN`. Signing in again can't
    /// replace its token, as the CLI prefers the variable.
    Variable(SharedString),
    /// The desktop's keyring.
    Keyring,
    /// A file, such as gh's `hosts.yml`.
    File(SharedString),
}

/// Whether an account's token works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AuthState {
    SignedIn,
    /// The forge refused it, and why: signing in again would help.
    Refused(SharedString),
    /// The forge couldn't be asked, as when the network is down, and why:
    /// the token may be fine.
    Unreachable(SharedString),
}

/// Where a CLI's check on a machine stands.
#[derive(Debug)]
pub(crate) enum Check {
    Checking,
    Done(CliProbe),
    /// The check couldn't run on the machine, in the script's or ssh's
    /// words.
    Failed(SharedString),
}

/// One CLI's check on one machine.
struct Entry {
    check: Check,
    /// The check under way; a newer one replaces it.
    _task: Task<()>,
}

/// Every CLI checked on every machine so far.
pub(crate) struct Forges {
    /// How to run a script on each machine.
    workspaces: Entity<Workspaces>,
    entries: HashMap<(MachineId, ForgeCli), Entry>,
}

impl Forges {
    pub(crate) fn new(workspaces: Entity<Workspaces>) -> Self {
        Self {
            workspaces,
            entries: HashMap::new(),
        }
    }

    /// Where `cli`'s check on `machine` stands; `None` before one starts.
    pub(crate) fn check(&self, machine: &MachineId, cli: ForgeCli) -> Option<&Check> {
        let key = (machine.clone(), cli);
        self.entries.get(&key).map(|entry| &entry.check)
    }

    /// Checks each CLI on `machine` that hasn't been since it last
    /// connected. A remote machine that isn't connected waits.
    pub(crate) fn ensure_checked(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        for cli in ForgeCli::ALL {
            if !self.entries.contains_key(&(machine.clone(), cli)) {
                self.start(machine, cli, cx);
            }
        }
    }

    /// Checks every CLI on `machine` again, as on Refresh or after its
    /// program changed in Settings.
    pub(crate) fn recheck(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        for cli in ForgeCli::ALL {
            self.start(machine, cli, cx);
        }
    }

    /// Drops what is known of `machine`'s CLIs, as it reconnected or is
    /// gone; the next [`Self::ensure_checked`] checks them afresh.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let before = self.entries.len();
        self.entries.retain(|(known, _), _| known != machine);
        if self.entries.len() != before {
            cx.notify();
        }
    }

    fn start(&mut self, machine: &MachineId, cli: ForgeCli, cx: &mut Context<'_, Self>) {
        let reader = self.workspaces.read(cx).reader(machine);
        if matches!(reader, Reader::Unread) {
            return;
        }
        let program = cx.global::<Settings>().cli_path(machine, cli).to_owned();
        let executor = cx.background_executor().clone();
        let key = (machine.clone(), cli);
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let args = vec![program.into()];
                let output = run(cli, reader, cli.script(), args, &executor).await;
                let check = match output.and_then(|output| cli.parse(&output)) {
                    Ok(probe) => Check::Done(probe),
                    Err(why) => Check::Failed(why.into()),
                };
                let _ = this.update(cx, |this, cx| {
                    if let Some(entry) = this.entries.get_mut(&key) {
                        entry.check = check;
                        cx.notify();
                    }
                });
            }
        });
        self.entries.insert(
            key,
            Entry {
                check: Check::Checking,
                _task: task,
            },
        );
        cx.notify();
    }
}
