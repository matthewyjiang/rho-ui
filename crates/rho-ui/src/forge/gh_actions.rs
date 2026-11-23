//! What rho-ui does to a pull request through `gh` on the session's
//! machine: comment, mark ready or draft, merge. Reading pull requests is
//! each machine's rho daemon's job, not rho-ui's.

use std::ffi::OsString;

use rho_ui_core::PullRequestMergeMethod;

use super::Job;
use crate::{folders, local};

/// What the surface asks the forge to do to a PR, as the session's account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Comment(String),
    /// Marks a draft ready for review.
    Ready,
    /// Turns it back into a draft.
    Draft,
    /// Merges it, if its head is still commit `head`, the one the user saw:
    /// the session may push more. It doesn't ask to delete the branch, a
    /// session may work on it; a repository set to delete head branches
    /// on merge still does.
    Merge {
        method: PullRequestMergeMethod,
        head: String,
    },
}

/// The POSIX `sh` script that acts on a PR: `gh` is its first argument,
/// then the host, the repository as `owner/name`, the PR's number, and the
/// `gh pr` subcommand with its arguments ([`action_args`]). With `--repo`,
/// `gh` touches no checkout. After [`ACTION_MARKER`] it prints `gh`'s exit
/// status, a NUL, and its error output.
pub(super) const ACTION_SCRIPT: &str = r#"unset CDPATH
export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_SPINNER_DISABLED=1 NO_COLOR=1
printf 'rho-ui-gh-pr\0'
gh=$1 repo=$2/$3 number=$4 verb=$5
shift 5
err=$("$gh" pr "$verb" "$number" --repo "$repo" "$@" </dev/null 2>&1 >/dev/null)
printf '%s\0%s\0' "$?" "$err"
"#;

const ACTION_MARKER: &[u8] = b"rho-ui-gh-pr\0";

/// Acts on a PR: [`ACTION_SCRIPT`] on a remote machine, [`action_here`]
/// here.
pub(super) const ACTION: Job = Job {
    script: ACTION_SCRIPT,
    here: action_here,
};

/// [`ACTION_SCRIPT`] in Rust, with the same arguments, printing what it
/// prints.
fn action_here(args: &[OsString]) -> Result<Vec<u8>, String> {
    let [gh, host, repo, number, verb, rest @ ..] = args else {
        return Err("A gh pr action takes gh, a repository, a number and a verb.".to_owned());
    };
    let mut full = host.clone();
    full.push("/");
    full.push(repo);
    let mut command = super::gh::command(gh);
    command
        .arg("pr")
        .arg(verb)
        .arg(number)
        .arg("--repo")
        .arg(full)
        .args(rest);
    let output = local::run(&mut command)?;
    // As `$?` reads, so 0 is success.
    let code = output.status.code().unwrap_or(1).to_string();
    Ok([ACTION_MARKER, code.as_bytes(), b"\0", &output.stderr, b"\0"].concat())
}

/// The `gh pr` subcommand and arguments that do `action`. A merge never
/// passes `--delete-branch`: a session may still work on the branch.
///
/// # Errors
///
/// For a merge method this rho-ui doesn't know, which the surface never
/// offers.
pub(super) fn action_args(action: &PrAction) -> Result<Vec<OsString>, String> {
    let args: Vec<&str> = match action {
        PrAction::Comment(body) => vec!["comment", "--body", body],
        PrAction::Ready => vec!["ready"],
        PrAction::Draft => vec!["ready", "--undo"],
        PrAction::Merge { method, head } => {
            let how = match method {
                PullRequestMergeMethod::Merge => "--merge",
                PullRequestMergeMethod::Squash => "--squash",
                PullRequestMergeMethod::Rebase => "--rebase",
                // `Unknown`, and any method a later rho-protocol adds.
                _ => return Err("This rho-ui doesn't know that merge method.".to_owned()),
            };
            vec!["merge", how, "--match-head-commit", head]
        }
    };
    Ok(args.into_iter().map(OsString::from).collect())
}

/// Whether [`ACTION_SCRIPT`] did it, from what it printed.
///
/// # Errors
///
/// When it didn't: what `gh` said.
pub(super) fn parse_action(output: &[u8]) -> Result<(), String> {
    let at =
        folders::find(output, ACTION_MARKER).ok_or_else(|| "gh printed nothing.".to_owned())?;
    let mut fields = output[at + ACTION_MARKER.len()..].splitn(3, |&byte| byte == 0);
    let code = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
    if code == "0" {
        return Ok(());
    }
    let error = String::from_utf8_lossy(fields.next().unwrap_or_default());
    let line = error.lines().map(str::trim).find(|line| !line.is_empty());
    Err(match line {
        Some(line) => line.strip_prefix("gh: ").unwrap_or(line).to_owned(),
        None => format!("gh exited with status {code}."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An action done reads as such; one refused says what gh said.
    #[test]
    fn actions_say_why_they_failed() {
        let printed = |code: &str, error: &str| {
            let mut output = b"motd\n".to_vec();
            output.extend(ACTION_MARKER);
            output.extend(format!("{code}\0{error}\0").as_bytes());
            output
        };
        assert_eq!(parse_action(&printed("0", "")), Ok(()));
        assert_eq!(
            parse_action(&printed(
                "1",
                "\ngh: GraphQL: Could not add comment (addComment)\n"
            )),
            Err("GraphQL: Could not add comment (addComment)".to_owned())
        );
        assert_eq!(
            parse_action(&printed("4", "")),
            Err("gh exited with status 4.".to_owned())
        );
        assert!(parse_action(b"motd only").is_err());
    }
}
