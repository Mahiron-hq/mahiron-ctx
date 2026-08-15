//! Comparing two paths that reached the program by different routes.
//!
//! `std::fs::canonicalize` on Windows returns a verbatim path — `\\?\C:\dir` rather than
//! `C:\dir`. A path that was canonicalised and a path that was not therefore compare
//! unequal even when they name the same file, and neither side is wrong: the traversal
//! canonicalises its roots, while a filesystem-notification event does not.
//!
//! Every comparison between paths of possibly different provenance goes through [`same`].

use std::borrow::Cow;
use std::path::Path;

/// The same path without a verbatim prefix, where one can be removed safely.
///
/// Only the drive form is simplified. `\\?\UNC\server\share` is left alone, as are the
/// verbatim forms that exist precisely to escape the ordinary path rules — rewriting
/// those would change which file is named.
#[cfg(windows)]
pub fn simplify(path: &Path) -> Cow<'_, Path> {
    use std::ffi::OsString;
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Cow::Borrowed(path);
    };
    let Prefix::VerbatimDisk(drive) = prefix.kind() else {
        return Cow::Borrowed(path);
    };
    let mut simplified = OsString::from(format!("{}:", drive as char));
    // What remains begins with the root separator, so this rebuilds `C:\rest`.
    simplified.push(components.as_path().as_os_str());
    Cow::Owned(std::path::PathBuf::from(simplified))
}

#[cfg(not(windows))]
pub fn simplify(path: &Path) -> Cow<'_, Path> {
    Cow::Borrowed(path)
}

/// Whether two paths name the same location, allowing for verbatim prefixes.
///
/// Purely textual: it does not touch the filesystem, so it is safe to call on a path that
/// does not exist and cheap enough for a comparison guarded by something cheaper still.
pub fn same(left: &Path, right: &Path) -> bool {
    left == right || simplify(left) == simplify(right)
}

/// [`same`], for the optional paths that comparisons against a parent directory produce.
pub fn same_option(left: Option<&Path>, right: Option<&Path>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same(left, right),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_path_is_the_same_as_itself() {
        let path = PathBuf::from("some/relative/path");
        assert!(same(&path, &path));
        assert!(!same(&path, Path::new("some/other/path")));
    }

    #[test]
    fn two_absent_paths_are_not_the_same_path() {
        // Nothing has a parent directory in common with nothing.
        assert!(!same_option(None, None));
    }

    #[test]
    #[cfg(windows)]
    fn a_canonicalised_path_matches_the_one_it_came_from() {
        // `canonicalize` produces the first form; a notification event produces the
        // second, and treating them as different paths is what let a run react to its own
        // staging file and never settle.
        assert!(same(
            Path::new(r"\\?\C:\Users\x\AppData\Local\Temp\out.md"),
            Path::new(r"C:\Users\x\AppData\Local\Temp\out.md")
        ));
    }

    #[test]
    #[cfg(windows)]
    fn a_verbatim_path_that_is_not_a_plain_drive_is_left_alone() {
        // Simplifying this would change which file is named.
        let unc = Path::new(r"\\?\UNC\server\share\file");
        assert_eq!(simplify(unc).as_ref(), unc);
    }
}
