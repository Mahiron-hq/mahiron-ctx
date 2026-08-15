//! Retrieval of exactly one explicitly designated remote repository.
//!
//! Nothing here runs unless the user supplied a designation this module recognises without
//! ambiguity. Retrieval fetches the file tree at a single reference and nothing else, and
//! the local copy's lifetime is bound to the run that asked for it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

use crate::config::RemoteSource;
use crate::error::{Error, Result};
use crate::report::WarningRecord;

/// Schemes that identify a remote repository beyond any doubt.
const REMOTE_SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "git+https://"];

/// Recognise a source argument as a remote designation, or decline to.
///
/// Anything merely resembling one — a bare host and path, a typo'd local path — is
/// deliberately not recognised, so a mistyped path can never become a network request.
pub fn recognise(argument: &str, reference: Option<&str>) -> Option<RemoteSource> {
    let candidate = argument.trim();
    if candidate.is_empty() || Path::new(candidate).exists() {
        return None;
    }

    let scheme_match = REMOTE_SCHEMES
        .iter()
        .any(|scheme| candidate.starts_with(scheme));
    let scp_like = is_scp_like(candidate);

    if !scheme_match && !scp_like {
        return None;
    }

    Some(RemoteSource {
        url: candidate.trim_start_matches("git+").to_string(),
        reference: reference.map(str::to_string),
    })
}

/// The `user@host:path` form standard version-control clients accept.
fn is_scp_like(candidate: &str) -> bool {
    let Some((user_host, path)) = candidate.split_once(':') else {
        return false;
    };
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    match user_host.split_once('@') {
        Some((user, host)) => !user.is_empty() && host.contains('.'),
        None => false,
    }
}

/// A retrieved copy whose removal is tied to this value's lifetime.
///
/// The directory is registered for cleanup the moment it is created, so an abnormal
/// termination during transfer leaves nothing behind either.
#[derive(Debug)]
pub struct EphemeralCopy {
    directory: Option<TempDir>,
    path: PathBuf,
    retained: bool,
}

impl EphemeralCopy {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn retained_path(&self) -> Option<&Path> {
        self.retained.then_some(self.path.as_path())
    }

    /// Hand the copy to the user instead of discarding it when the run ends.
    fn retain(mut self) -> Self {
        if let Some(directory) = self.directory.take() {
            self.path = directory.keep();
            self.retained = true;
            deregister(&self.path);
        }
        self
    }
}

impl Drop for EphemeralCopy {
    fn drop(&mut self) {
        if self.directory.is_some() {
            deregister(&self.path);
        }
    }
}

#[derive(Debug)]
pub struct Retrieval {
    pub copy: EphemeralCopy,
    pub warnings: Vec<WarningRecord>,
}

use crate::cleanup::{deregister, register_directory};

fn git_version() -> Result<(u32, u32)> {
    let output = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Error::Remote(format!(
                "no usable `git` executable was found on PATH ({error}); install git, or \
                 clone the repository yourself and point the tool at the resulting directory"
            ))
        })?;

    let text = String::from_utf8_lossy(&output.stdout);
    let numbers: Vec<u32> = text
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|version| {
            version
                .split('.')
                .take(2)
                .filter_map(|part| part.parse().ok())
                .collect()
        })
        .unwrap_or_default();

    match numbers.as_slice() {
        [major, minor, ..] => Ok((*major, *minor)),
        [major] => Ok((*major, 0)),
        _ => Ok((0, 0)),
    }
}

/// Obtain a local copy of one remote source's current file tree.
pub fn retrieve(source: &RemoteSource, keep: bool) -> Result<Retrieval> {
    crate::cleanup::install_handler();

    let (major, minor) = git_version()?;
    let warnings = Vec::new();
    // Shallow, single-branch retrieval landed in 1.9. Without it the only thing the
    // client can do is a full clone, which places the repository's whole history on the
    // user's machine as a side effect of packaging its current state. That is refused
    // rather than done with a warning attached: the documentation says the run stops, and
    // "we quietly downloaded ten years of history" is not something a warning repairs.
    if (major, minor) < (1, 9) {
        return Err(Error::Remote(format!(
            "the installed git is {major}.{minor}, which has no shallow single-branch \
             clone; retrieving this way would place the repository's entire history on \
             this machine. Upgrade git to 1.9 or later, or clone the repository yourself \
             and point the tool at the resulting directory"
        )));
    }

    let directory = TempDir::with_prefix("mahiron-ctx-remote-")?;
    register_directory(directory.path());
    let checkout = directory.path().join("source");

    let outcome = clone_tree(&source.url, source.reference.as_deref(), &checkout);

    let copy = EphemeralCopy {
        directory: Some(directory),
        path: checkout,
        retained: false,
    };

    outcome?;

    // The client leaves its own metadata behind even for a depth-limited fetch; removing
    // it keeps version-control data off the user's machine and out of the packaged output.
    let _ = std::fs::remove_dir_all(copy.path().join(".git"));

    let copy = if keep { copy.retain() } else { copy };
    Ok(Retrieval { copy, warnings })
}

fn clone_tree(url: &str, reference: Option<&str>, destination: &Path) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("clone")
        .arg("--quiet")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch")
        .arg("--no-tags");
    if let Some(reference) = reference {
        command.arg("--branch").arg(reference);
    }
    command.arg("--").arg(url).arg(destination);

    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::Remote(error.to_string()))?;

    if output.status.success() {
        return Ok(());
    }

    // A commit identifier is not a branch name, so the client rejects it above; fetching
    // the single object directly is the equivalent minimal-transfer path for that case.
    match reference {
        Some(reference) if looks_like_commit(reference) => {
            fetch_commit(url, reference, destination)
        }
        _ => Err(Error::Remote(format!(
            "`git clone` failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

fn looks_like_commit(reference: &str) -> bool {
    reference.len() >= 7
        && reference.len() <= 40
        && reference.chars().all(|c| c.is_ascii_hexdigit())
}

fn fetch_commit(url: &str, commit: &str, destination: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(destination);
    std::fs::create_dir_all(destination)?;

    run_git(destination, &["init", "--quiet"])?;
    run_git(destination, &["remote", "add", "origin", url])?;
    run_git(
        destination,
        &["fetch", "--quiet", "--depth", "1", "origin", commit],
    )?;
    run_git(
        destination,
        &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
    )
}

fn run_git(directory: &Path, arguments: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::Remote(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Remote(format!(
        "`git {}` failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_schemes_are_recognised() {
        assert!(recognise("https://example.com/owner/repo.git", None).is_some());
        assert!(recognise("ssh://git@example.com/owner/repo.git", None).is_some());
        assert!(recognise("git@example.com:owner/repo.git", None).is_some());
    }

    #[test]
    fn ambiguous_arguments_stay_local() {
        for argument in [
            "example.com/owner/repo",
            "./src",
            "../sibling",
            "C:/projects/repo",
            "repo",
            "user@host",
            "notes:draft",
        ] {
            assert!(
                recognise(argument, None).is_none(),
                "{argument} should not be treated as remote"
            );
        }
    }

    #[test]
    fn an_existing_local_path_is_never_a_remote_designation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert!(recognise(&path, None).is_none());
    }

    #[test]
    fn commit_identifiers_are_distinguished_from_branch_names() {
        assert!(looks_like_commit("9f8e7d6c5b4a39281706"));
        assert!(!looks_like_commit("main"));
        assert!(!looks_like_commit("release/2.0"));
    }
}
