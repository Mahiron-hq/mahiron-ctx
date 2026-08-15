//! Repeat a run whenever a source changes.
//!
//! Never entered implicitly: a watching process holds the terminal and rewrites the
//! destination indefinitely, which is only ever acceptable when it was asked for.

use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::config::{Destination, Settings, SourceSpec};
use crate::error::{Error, Result};
use crate::report::RunStatus;

/// Quiet period after a change before repackaging, so one save does not cause several runs.
const SETTLE: Duration = Duration::from_millis(300);

/// Watch every local source and repackage until interrupted.
pub fn run(settings: &Settings, once: fn(&Settings) -> Result<RunStatus>) -> Result<RunStatus> {
    if settings.has_remote_source() {
        return Err(Error::config(
            "a remote source is a single retrieved snapshot and cannot be watched",
        ));
    }

    let (sender, receiver) = channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|e| Error::config(format!("could not start watching: {e}")))?;

    for source in &settings.sources {
        if let SourceSpec::Local(path) = source {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|e| Error::config(format!("could not watch {}: {e}", path.display())))?;
        }
    }

    // Later passes replace the document the earlier ones wrote; asking each time would
    // make the mode unusable.
    let mut settings = settings.clone();
    settings.overwrite = true;
    let settings = &settings;

    let mut status = once(settings)?;
    eprintln!("mahiron-ctx: watching for changes; press Ctrl-C to stop");

    loop {
        // Re-resolved on every pass. Resolving once at start-up canonicalised a path that
        // did not exist yet, which fell back to whatever the user typed — usually a
        // relative path, which never equals the absolute path an event carries, so even
        // the document itself looked like a change worth reacting to.
        let ours = Delivered::of(settings);

        match receiver.recv() {
            Ok(Ok(event)) if ours.is_relevant(&event.paths) => {}
            Ok(_) => continue,
            Err(_) => break,
        }

        // Drain whatever else arrives while the editor finishes writing.
        loop {
            match receiver.recv_timeout(SETTLE) {
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Ok(status),
            }
        }

        status = match once(settings) {
            Ok(status) => status,
            Err(error) => {
                eprintln!("mahiron-ctx: {error}");
                RunStatus::Failure
            }
        };
    }
    Ok(status)
}

/// The paths a run of its own writes, which must not be mistaken for a change to watch.
///
/// Comparing against the destination alone was not enough. Delivery is atomic: the
/// document is written to a staging file beside the destination and renamed into place,
/// and both the creation of that file and the rename are changes inside the watched tree.
/// Every run therefore scheduled the next one, for ever.
#[derive(Debug, Default)]
struct Delivered {
    destination: Option<PathBuf>,
    staging_directory: Option<PathBuf>,
}

impl Delivered {
    fn of(settings: &Settings) -> Self {
        let destination = match &settings.destination {
            Destination::File(path) => Some(std::fs::canonicalize(path).unwrap_or_else(|_| {
                // Not written yet: resolve the directory, which does exist, and put
                // the name back on, so the comparison is against an absolute path
                // either way.
                let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
                match (directory, path.file_name()) {
                    (Some(directory), Some(name)) => std::fs::canonicalize(directory)
                        .map(|resolved| resolved.join(name))
                        .unwrap_or_else(|_| path.clone()),
                    _ => path.clone(),
                }
            })),
            _ => None,
        };
        Self {
            staging_directory: crate::delivery::staging_directory(settings),
            destination,
        }
    }

    fn is_ours(&self, path: &std::path::Path) -> bool {
        // Both sides are compared through `paths::same`, because they arrive by different
        // routes: these were canonicalised, while a notification event's path was not. On
        // Windows that difference is a verbatim `\\?\` prefix, so a plain comparison
        // never matched and every run scheduled the next one all over again.
        if self
            .destination
            .as_deref()
            .is_some_and(|destination| crate::paths::same(path, destination))
        {
            return true;
        }
        let staged = path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(crate::delivery::STAGING_PREFIX)
        });
        staged && crate::paths::same_option(path.parent(), self.staging_directory.as_deref())
    }

    fn is_relevant(&self, paths: &[PathBuf]) -> bool {
        paths.iter().any(|path| !self.is_ours(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_to_the_document_itself_is_ignored() {
        let destination = PathBuf::from("/tmp/mhrn-output.md");
        let ours = Delivered {
            destination: Some(destination.clone()),
            staging_directory: Some(PathBuf::from("/tmp")),
        };
        assert!(!ours.is_relevant(std::slice::from_ref(&destination)));
        assert!(ours.is_relevant(&[PathBuf::from("/tmp/src/main.rs")]));
    }

    #[test]
    fn the_staging_file_a_run_delivers_through_is_ignored_too() {
        // Every run creates one of these beside the destination and renames it into
        // place. Both events land inside the watched tree, so treating them as changes
        // made each run schedule the next and the mode never settled.
        let ours = Delivered {
            destination: Some(PathBuf::from("/tmp/mhrn-output.md")),
            staging_directory: Some(PathBuf::from("/tmp")),
        };
        let staging = PathBuf::from(format!("/tmp/{}AbCdEf", crate::delivery::STAGING_PREFIX));
        assert!(!ours.is_relevant(std::slice::from_ref(&staging)));

        // A rename event carries both paths, and neither of them is a source change.
        assert!(!ours.is_relevant(&[staging, PathBuf::from("/tmp/mhrn-output.md")]));
    }
}
