use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

use crate::config::TOOL_IGNORE_FILENAMES;
use crate::report::ExclusionReason;

/// Outcome of consulting the ignore rules that apply at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreDecision {
    /// No rule matched, or the deepest matching rule was a negation.
    Allowed,
    Ignored {
        reason: ExclusionReason,
        rule: String,
    },
}

#[derive(Debug, Default)]
struct LevelMatchers {
    /// Rules from the version-control ignore files at this level.
    vcs: Option<Gitignore>,
    /// Rules from this tool's own ignore file at this level.
    tool: Option<Gitignore>,
}

#[derive(Debug)]
struct DirState {
    /// Set when the directory itself, or an ancestor, was excluded by a rule.
    ignored: Option<(ExclusionReason, String)>,
    /// Matchers from the outermost level down to this directory.
    levels: Vec<Arc<LevelMatchers>>,
}

/// Resolves version-control and tool-specific ignore rules for one source root.
///
/// Deeper rules override shallower ones, and at a single level the tool's own rules are
/// consulted before the version-control ones, so the documented precedence holds
/// wherever a file sits in the hierarchy.
pub struct IgnoreResolver {
    root: PathBuf,
    use_vcs: bool,
    use_tool: bool,
    case_insensitive: bool,
    global: Option<Gitignore>,
    base: Arc<DirState>,
    cache: RwLock<HashMap<PathBuf, Arc<DirState>>>,
}

/// One directory's resolved state, remembered by a single walker thread.
///
/// A traversal visits a directory's files consecutively, so this hits for every file
/// after the first and keeps the shared cache — and its lock — out of the per-file path
/// entirely. Without it, every file in the tree contends on one lock.
#[derive(Debug, Default)]
pub struct DirCache {
    last: Option<(PathBuf, Arc<DirState>)>,
}

impl IgnoreResolver {
    pub fn new(root: &Path, use_vcs: bool, use_tool: bool, case_insensitive: bool) -> Self {
        let global = use_vcs.then(global_matcher).cloned();
        let mut levels = Vec::new();
        if use_vcs {
            // Rules above the source root still apply to files below it, exactly as they
            // would to any other consumer of the project's ignore configuration.
            for ancestor in ancestors_up_to_repository_root(root) {
                levels.push(Arc::new(build_level(
                    &ancestor,
                    true,
                    false,
                    case_insensitive,
                )));
            }
        }
        Self {
            root: root.to_path_buf(),
            use_vcs,
            use_tool,
            case_insensitive,
            global,
            base: Arc::new(DirState {
                ignored: None,
                levels,
            }),
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn state_for(&self, directory: &Path) -> Arc<DirState> {
        if let Some(cached) = self.read_cache().get(directory) {
            return Arc::clone(cached);
        }

        let state = if directory == self.root {
            let mut levels = self.base.levels.clone();
            levels.push(Arc::new(build_level(
                directory,
                self.use_vcs,
                self.use_tool,
                self.case_insensitive,
            )));
            Arc::new(DirState {
                ignored: None,
                levels,
            })
        } else {
            let parent = match directory.parent() {
                Some(parent) if directory.starts_with(&self.root) => self.state_for(parent),
                _ => Arc::clone(&self.base),
            };
            let ignored = parent.ignored.clone().or_else(|| {
                match self.evaluate_against(&parent, directory, true) {
                    IgnoreDecision::Ignored { reason, rule } => Some((reason, rule)),
                    IgnoreDecision::Allowed => None,
                }
            });
            let mut levels = parent.levels.clone();
            levels.push(Arc::new(build_level(
                directory,
                self.use_vcs,
                self.use_tool,
                self.case_insensitive,
            )));
            Arc::new(DirState { ignored, levels })
        };

        self.write_cache()
            .insert(directory.to_path_buf(), Arc::clone(&state));
        state
    }

    fn read_cache(&self) -> std::sync::RwLockReadGuard<'_, HashMap<PathBuf, Arc<DirState>>> {
        match self.cache.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn write_cache(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<PathBuf, Arc<DirState>>> {
        match self.cache.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn evaluate_against(&self, state: &DirState, path: &Path, is_dir: bool) -> IgnoreDecision {
        for level in state.levels.iter().rev() {
            if let Some(tool) = &level.tool {
                match tool.matched(path, is_dir) {
                    Match::Ignore(glob) => {
                        return IgnoreDecision::Ignored {
                            reason: ExclusionReason::ToolIgnoreRules,
                            rule: glob.original().to_string(),
                        }
                    }
                    Match::Whitelist(_) => return IgnoreDecision::Allowed,
                    Match::None => {}
                }
            }
            if let Some(vcs) = &level.vcs {
                match vcs.matched(path, is_dir) {
                    Match::Ignore(glob) => {
                        return IgnoreDecision::Ignored {
                            reason: ExclusionReason::IgnoreRules,
                            rule: glob.original().to_string(),
                        }
                    }
                    Match::Whitelist(_) => return IgnoreDecision::Allowed,
                    Match::None => {}
                }
            }
        }
        if let Some(global) = &self.global {
            match global.matched(path, is_dir) {
                Match::Ignore(glob) => {
                    return IgnoreDecision::Ignored {
                        reason: ExclusionReason::IgnoreRules,
                        rule: format!("{} (global)", glob.original()),
                    }
                }
                Match::Whitelist(_) => return IgnoreDecision::Allowed,
                Match::None => {}
            }
        }
        IgnoreDecision::Allowed
    }

    /// Decide one path, which must sit at or below this resolver's root.
    pub fn decide(&self, path: &Path, is_dir: bool) -> IgnoreDecision {
        self.decide_cached(&mut DirCache::default(), path, is_dir)
    }

    /// Decide one path, reusing the caller's memory of the directory it sits in.
    pub fn decide_cached(&self, cache: &mut DirCache, path: &Path, is_dir: bool) -> IgnoreDecision {
        if !self.use_vcs && !self.use_tool {
            return IgnoreDecision::Allowed;
        }
        let directory = path.parent().unwrap_or(&self.root);

        let state = match &cache.last {
            Some((remembered, state)) if remembered == directory => Arc::clone(state),
            _ => {
                let state = self.state_for(directory);
                cache.last = Some((directory.to_path_buf(), Arc::clone(&state)));
                state
            }
        };

        if let Some((reason, rule)) = &state.ignored {
            return IgnoreDecision::Ignored {
                reason: reason.clone(),
                rule: rule.clone(),
            };
        }
        self.evaluate_against(&state, path, is_dir)
    }
}

/// The user's global ignore configuration, read once per process.
///
/// Reading it per source re-parsed `core.excludesFile` for every designation on the
/// command line.
fn global_matcher() -> &'static Gitignore {
    static GLOBAL: OnceLock<Gitignore> = OnceLock::new();
    GLOBAL.get_or_init(|| Gitignore::global().0)
}

fn build_level(
    directory: &Path,
    use_vcs: bool,
    use_tool: bool,
    case_insensitive: bool,
) -> LevelMatchers {
    let mut level = LevelMatchers::default();

    if use_vcs {
        let mut builder = GitignoreBuilder::new(directory);
        builder.case_insensitive(case_insensitive).ok();
        let mut any = false;
        for candidate in [".gitignore", ".ignore"] {
            let path = directory.join(candidate);
            if path.is_file() && builder.add(&path).is_none() {
                any = true;
            }
        }
        let exclude = directory.join(".git").join("info").join("exclude");
        if exclude.is_file() && builder.add(&exclude).is_none() {
            any = true;
        }
        if any {
            level.vcs = builder.build().ok();
        }
    }

    if use_tool {
        let mut builder = GitignoreBuilder::new(directory);
        builder.case_insensitive(case_insensitive).ok();
        let mut any = false;
        for candidate in TOOL_IGNORE_FILENAMES {
            let path = directory.join(candidate);
            if path.is_file() && builder.add(&path).is_none() {
                any = true;
            }
        }
        if any {
            level.tool = builder.build().ok();
        }
    }

    level
}

/// Directories above `root`, outermost first, stopping once a repository root is passed.
///
/// Rules above a source apply to it only when the source is part of a repository that
/// those rules govern. Without the repository test the ascent ran all the way to `/`,
/// which let a `.gitignore` in the user's home directory silently govern a scan of an
/// unrelated tree — something git itself never does — and cost five `is_file` calls per
/// ancestor level to do it.
fn ancestors_up_to_repository_root(root: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut current = root.parent();
    let mut found_repository = false;
    while let Some(directory) = current {
        chain.push(directory.to_path_buf());
        if directory.join(".git").exists() {
            found_repository = true;
            break;
        }
        current = directory.parent();
    }
    if !found_repository {
        // No repository encloses this source, so nothing above it has any claim on it.
        return Vec::new();
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn tool_rules_take_precedence_over_version_control_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "!keep.log\n*.log\n");
        write(&root.join(".mahironignore"), "keep.log\n");
        write(&root.join("keep.log"), "x");

        let resolver = IgnoreResolver::new(root, true, true, false);
        assert!(matches!(
            resolver.decide(&root.join("keep.log"), false),
            IgnoreDecision::Ignored {
                reason: ExclusionReason::ToolIgnoreRules,
                ..
            }
        ));
    }

    #[test]
    fn nested_rules_override_shallower_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "*.txt\n");
        write(&root.join("src").join(".gitignore"), "!notes.txt\n");
        write(&root.join("src").join("notes.txt"), "x");

        let resolver = IgnoreResolver::new(root, true, false, false);
        assert_eq!(
            resolver.decide(&root.join("src").join("notes.txt"), false),
            IgnoreDecision::Allowed
        );
        assert!(matches!(
            resolver.decide(&root.join("other.txt"), false),
            IgnoreDecision::Ignored { .. }
        ));
    }

    #[test]
    fn contents_of_an_ignored_directory_inherit_the_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "generated/\n");
        write(&root.join("generated").join("deep").join("a.rs"), "x");

        let resolver = IgnoreResolver::new(root, true, false, false);
        assert!(matches!(
            resolver.decide(&root.join("generated").join("deep").join("a.rs"), false),
            IgnoreDecision::Ignored { .. }
        ));
    }

    #[test]
    fn rules_above_a_source_apply_only_inside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        // A stray ignore file above an unrelated tree, with no repository anywhere.
        write(&outer.join(".gitignore"), "*.rs\n");
        let source = outer.join("unrelated");
        write(&source.join("a.rs"), "x");

        let resolver = IgnoreResolver::new(&source, true, false, false);
        assert_eq!(
            resolver.decide(&source.join("a.rs"), false),
            IgnoreDecision::Allowed,
            "an ignore file outside any repository governed an unrelated tree"
        );

        // Mark the enclosing directory as a repository and the same rule now applies.
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        let inside = IgnoreResolver::new(&source, true, false, false);
        assert!(matches!(
            inside.decide(&source.join("a.rs"), false),
            IgnoreDecision::Ignored { .. }
        ));
    }

    #[test]
    fn disabling_every_mechanism_allows_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "*\n");
        write(&root.join("a.rs"), "x");

        let resolver = IgnoreResolver::new(root, false, false, false);
        assert_eq!(
            resolver.decide(&root.join("a.rs"), false),
            IgnoreDecision::Allowed
        );
    }
}
