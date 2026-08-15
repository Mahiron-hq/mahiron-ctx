//! Removal of the paths a run created but does not want to outlive it.
//!
//! Destructors already cover every ordinary path, including every error return: a
//! `NamedTempFile` or `TempDir` dropped during unwinding removes itself. What destructors
//! cannot cover is termination that skips them — a signal, or a handler that calls
//! `std::process::exit`. Everything registered here is removed on that path too.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
}

fn registry() -> &'static Mutex<Vec<(PathBuf, Kind)>> {
    static REGISTRY: OnceLock<Mutex<Vec<(PathBuf, Kind)>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock() -> std::sync::MutexGuard<'static, Vec<(PathBuf, Kind)>> {
    match registry().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Register a file to be removed if the process dies without unwinding.
pub fn register_file(path: &Path) {
    lock().push((path.to_path_buf(), Kind::File));
}

/// Register a directory to be removed if the process dies without unwinding.
pub fn register_directory(path: &Path) {
    lock().push((path.to_path_buf(), Kind::Directory));
}

/// Forget a path, because its owner has removed or deliberately retained it.
pub fn deregister(path: &Path) {
    lock().retain(|(candidate, _)| candidate != path);
}

/// Remove every registered path, for a termination that will not run destructors.
pub fn purge() {
    for (path, kind) in lock().drain(..) {
        let _ = match kind {
            Kind::File => std::fs::remove_file(&path),
            Kind::Directory => std::fs::remove_dir_all(&path),
        };
    }
}

/// Install the signal handler that upholds the cleanup guarantee on interruption.
///
/// Idempotent, and called unconditionally at the start of every run rather than only by
/// the paths that happen to create a temporary directory: a purely local run creates a
/// staging file too, and without a handler it is left behind on the first Ctrl-C.
pub fn install_handler() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = ctrlc::set_handler(|| {
            purge();
            std::process::exit(130);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_file_is_removed_and_a_deregistered_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept");
        let purged = dir.path().join("purged");
        std::fs::write(&kept, "k").unwrap();
        std::fs::write(&purged, "p").unwrap();

        register_file(&kept);
        register_file(&purged);
        deregister(&kept);
        purge();

        assert!(kept.exists(), "a deregistered path was removed anyway");
        assert!(!purged.exists(), "a registered path survived the purge");
    }
}
