use globset::{Glob, GlobBuilder, GlobMatcher};

use crate::error::{Error, Result};

/// A single compiled invocation-time pattern together with its specificity score.
#[derive(Debug, Clone)]
struct CompiledPattern {
    source: String,
    matcher: GlobMatcher,
    /// Set when the pattern names no directory component, so it may also be matched
    /// against a bare filename regardless of where in the tree the file sits.
    name_matcher: Option<GlobMatcher>,
    specificity: u32,
    /// The pattern's leading literal path segments, up to the first metacharacter, or
    /// `None` when the pattern names no directory component and so may match at any
    /// depth. Used to decide whether a directory could hold anything this pattern wants.
    literal_prefix: Option<String>,
    /// Set when the pattern is a literal directory followed by `/**`, and so claims
    /// everything beneath that directory without exception. `node_modules/**` does not
    /// match `node_modules` itself, so without this the directory is never recognised as
    /// wholly excluded and the traversal walks all of it to reject each file in turn.
    owned_subtree: Option<String>,
}

impl CompiledPattern {
    fn compile(pattern: &str, case_insensitive: bool) -> Result<Self> {
        let matcher = build(pattern, case_insensitive)?;
        let name_matcher = if pattern.contains('/') {
            None
        } else {
            Some(matcher.clone())
        };
        Ok(Self {
            source: pattern.to_string(),
            matcher,
            name_matcher,
            specificity: specificity(pattern),
            literal_prefix: literal_prefix(pattern),
            owned_subtree: owned_subtree(pattern),
        })
    }

    fn matches(&self, relative_path: &str, file_name: &str) -> bool {
        if self.matcher.is_match(relative_path) {
            return true;
        }
        self.name_matcher
            .as_ref()
            .is_some_and(|m| m.is_match(file_name))
    }
}

/// The leading run of literal path segments in a pattern.
///
/// `src/generated/*.rs` yields `src/generated`; `**/*.rs` and `*.toml` yield `None`,
/// because they name no directory and could match anywhere. Everything after the first
/// metacharacter is discarded, and a partial final segment with it: `src/gen*/a.rs`
/// yields `src`, since `gen*` may name any number of directories.
fn literal_prefix(pattern: &str) -> Option<String> {
    let stop = pattern
        .find(['*', '?', '[', '{', '!'])
        .unwrap_or(pattern.len());
    let head = &pattern[..stop];
    let boundary = if stop == pattern.len() {
        // A fully literal pattern names a file; its directory is everything above it.
        head.rfind('/')?
    } else {
        head.rfind('/')?
    };
    let prefix = head[..boundary].trim_end_matches('/');
    (!prefix.is_empty()).then(|| prefix.to_string())
}

/// The directory a `dir/**` pattern claims in its entirety, if it is of that shape.
fn owned_subtree(pattern: &str) -> Option<String> {
    let head = pattern.strip_suffix("/**")?;
    let literal = !head.contains(['*', '?', '[', '{', '!']);
    (literal && !head.is_empty()).then(|| head.trim_end_matches('/').to_string())
}

fn build(pattern: &str, case_insensitive: bool) -> Result<GlobMatcher> {
    let glob: Glob = GlobBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        // `*` is expected to span directory boundaries so that `*.rs` reaches nested files,
        // which is the convention users of this class of tool already have.
        .literal_separator(false)
        .empty_alternates(true)
        .build()
        .map_err(|e| Error::Pattern {
            pattern: pattern.to_string(),
            message: e.to_string(),
        })?;
    Ok(glob.compile_matcher())
}

/// Score expressing how deliberately a pattern names the files it matches.
///
/// Literal characters and explicit path segments raise it; wildcards lower it. The exact
/// arithmetic matters less than that it is total, stable and documented, since it decides
/// include/exclude conflicts within a single invocation.
fn specificity(pattern: &str) -> u32 {
    let literals = pattern
        .chars()
        .filter(|c| !matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '!' | '/'))
        .count() as u32;
    let segments = pattern.split('/').filter(|s| !s.is_empty()).count() as u32;
    let wildcards = pattern.chars().filter(|c| matches!(c, '*' | '?')).count() as u32;
    let recursive = pattern.matches("**").count() as u32;

    (literals * 4 + segments * 8).saturating_sub(wildcards * 3 + recursive * 6)
}

/// The pattern that decided a file's fate, for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    pub pattern: String,
    pub specificity: u32,
}

/// Outcome of evaluating the invocation-time patterns against one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No pattern had anything to say; earlier rule sources decide.
    Neutral,
    Included(PatternMatch),
    Excluded(PatternMatch),
    /// Inclusion patterns were supplied and none of them matched.
    NotIncluded,
}

/// Invocation-time inclusion and exclusion patterns, resolved against one another.
#[derive(Debug, Clone, Default)]
pub struct PatternSet {
    include: Vec<CompiledPattern>,
    exclude: Vec<CompiledPattern>,
}

impl PatternSet {
    pub fn compile(include: &[String], exclude: &[String], case_insensitive: bool) -> Result<Self> {
        let compile_all = |patterns: &[String]| -> Result<Vec<CompiledPattern>> {
            patterns
                .iter()
                .map(|p| CompiledPattern::compile(p, case_insensitive))
                .collect()
        };
        Ok(Self {
            include: compile_all(include)?,
            exclude: compile_all(exclude)?,
        })
    }

    /// Whether any inclusion pattern could match something inside `directory`.
    ///
    /// Traversal prunes directories before it ever sees the files inside them, so a
    /// pruned directory would otherwise make an explicit `--include` unsatisfiable —
    /// `--include '.github/**'` would return nothing because `.github` is hidden. This is
    /// deliberately generous: a false positive costs one directory read, a false negative
    /// silently loses files the user asked for by name.
    pub fn may_include_below(&self, directory: &str) -> bool {
        self.include
            .iter()
            .any(|pattern| match &pattern.literal_prefix {
                // Names no directory, so it may match at any depth beneath this one.
                None => true,
                Some(prefix) => {
                    prefix.starts_with(directory) || directory.starts_with(prefix.as_str())
                }
            })
    }

    /// Resolve the patterns against a directory, so that an explicit inclusion or
    /// exclusion of a whole subtree is honoured at the boundary rather than per file.
    pub fn resolve_directory(&self, relative_path: &str, name: &str) -> Resolution {
        // `node_modules/**` is written to mean the whole subtree, but it does not match
        // `node_modules` itself, so resolving the directory the way a file is resolved
        // would descend into all of it and produce one exclusion record per file inside.
        // An inclusion reaching below still wins, and is checked first.
        if !self.may_include_below(relative_path) {
            if let Some(matched) = self.owner_of(relative_path) {
                return Resolution::Excluded(matched);
            }
        }
        match self.resolve(relative_path, name) {
            // A directory matching no inclusion pattern is not itself excluded: something
            // beneath it may still match, and `may_include_below` is what decides that.
            Resolution::NotIncluded => Resolution::Neutral,
            other => other,
        }
    }

    /// The exclusion pattern that claims this directory and everything under it.
    fn owner_of(&self, directory: &str) -> Option<PatternMatch> {
        if directory.is_empty() {
            return None;
        }
        self.exclude
            .iter()
            .filter(|pattern| {
                pattern.owned_subtree.as_deref().is_some_and(|owned| {
                    directory == owned
                        || directory
                            .strip_prefix(owned)
                            .is_some_and(|rest| rest.starts_with('/'))
                })
            })
            .max_by_key(|pattern| pattern.specificity)
            .map(|pattern| PatternMatch {
                pattern: pattern.source.clone(),
                specificity: pattern.specificity,
            })
    }

    fn best<'a>(
        candidates: &'a [CompiledPattern],
        relative_path: &str,
        file_name: &str,
    ) -> Option<&'a CompiledPattern> {
        // `max_by_key` returns the *last* maximum, so the iteration is reversed to make
        // ties resolve toward the pattern written first, which is what the rule says and
        // what keeps the reported attribution stable for an unchanged argument list.
        candidates
            .iter()
            .rev()
            .filter(|p| p.matches(relative_path, file_name))
            .max_by_key(|p| p.specificity)
    }

    /// Resolve inclusion against exclusion for one file.
    ///
    /// The more specific pattern wins; where both are equally specific, exclusion wins.
    pub fn resolve(&self, relative_path: &str, file_name: &str) -> Resolution {
        let included = Self::best(&self.include, relative_path, file_name);
        let excluded = Self::best(&self.exclude, relative_path, file_name);

        match (included, excluded) {
            (None, None) => {
                if self.include.is_empty() {
                    Resolution::Neutral
                } else {
                    Resolution::NotIncluded
                }
            }
            (Some(inc), None) => Resolution::Included(PatternMatch {
                pattern: inc.source.clone(),
                specificity: inc.specificity,
            }),
            (None, Some(exc)) => Resolution::Excluded(PatternMatch {
                pattern: exc.source.clone(),
                specificity: exc.specificity,
            }),
            (Some(inc), Some(exc)) => {
                if inc.specificity > exc.specificity {
                    Resolution::Included(PatternMatch {
                        pattern: inc.source.clone(),
                        specificity: inc.specificity,
                    })
                } else {
                    Resolution::Excluded(PatternMatch {
                        pattern: exc.source.clone(),
                        specificity: exc.specificity,
                    })
                }
            }
        }
    }
}

/// Patterns used to override content classification in either direction.
#[derive(Debug, Clone, Default)]
pub struct ClassificationOverrides {
    force_text: Vec<CompiledPattern>,
    force_binary: Vec<CompiledPattern>,
}

impl ClassificationOverrides {
    pub fn compile(
        force_text: &[String],
        force_binary: &[String],
        case_insensitive: bool,
    ) -> Result<Self> {
        let compile_all = |patterns: &[String]| -> Result<Vec<CompiledPattern>> {
            patterns
                .iter()
                .map(|p| CompiledPattern::compile(p, case_insensitive))
                .collect()
        };
        Ok(Self {
            force_text: compile_all(force_text)?,
            force_binary: compile_all(force_binary)?,
        })
    }

    /// `Some(true)` forces text, `Some(false)` forces binary, `None` leaves the heuristic alone.
    ///
    /// A file named by both lists is resolved the same way an include/exclude conflict is:
    /// the more specific pattern wins, and an exact tie goes to binary, which is the more
    /// conservative of the two. [`Self::is_contested`] reports the overlap.
    pub fn decide(&self, relative_path: &str, file_name: &str) -> Option<bool> {
        let binary = PatternSet::best(&self.force_binary, relative_path, file_name);
        let text = PatternSet::best(&self.force_text, relative_path, file_name);
        match (text, binary) {
            (None, None) => None,
            (Some(_), None) => Some(true),
            (None, Some(_)) => Some(false),
            // Named by both: the more specific instruction wins, and an exact tie goes to
            // the more conservative of the two.
            (Some(text), Some(binary)) => Some(text.specificity > binary.specificity),
        }
    }

    /// Whether a path is named by both lists, so the caller can say so rather than
    /// resolving the overlap silently.
    pub fn is_contested(&self, relative_path: &str, file_name: &str) -> bool {
        PatternSet::best(&self.force_text, relative_path, file_name).is_some()
            && PatternSet::best(&self.force_binary, relative_path, file_name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(include: &[&str], exclude: &[&str]) -> PatternSet {
        let inc: Vec<String> = include.iter().map(|s| s.to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| s.to_string()).collect();
        PatternSet::compile(&inc, &exc, false).unwrap()
    }

    #[test]
    fn more_specific_inclusion_beats_broader_exclusion() {
        let s = set(&["src/keep/config.toml"], &["*.toml"]);
        assert!(matches!(
            s.resolve("src/keep/config.toml", "config.toml"),
            Resolution::Included(_)
        ));
    }

    #[test]
    fn more_specific_exclusion_beats_broader_inclusion() {
        let s = set(&["*.rs"], &["src/generated/schema.rs"]);
        assert!(matches!(
            s.resolve("src/generated/schema.rs", "schema.rs"),
            Resolution::Excluded(_)
        ));
    }

    #[test]
    fn equal_specificity_resolves_to_exclusion() {
        let s = set(&["*.rs"], &["*.rs"]);
        assert!(matches!(
            s.resolve("src/main.rs", "main.rs"),
            Resolution::Excluded(_)
        ));
    }

    #[test]
    fn inclusion_list_turns_unmatched_files_into_non_candidates() {
        let s = set(&["*.rs"], &[]);
        assert_eq!(s.resolve("README.md", "README.md"), Resolution::NotIncluded);
    }

    #[test]
    fn bare_name_pattern_matches_at_any_depth() {
        let s = set(&[], &["Cargo.lock"]);
        assert!(matches!(
            s.resolve("crates/inner/Cargo.lock", "Cargo.lock"),
            Resolution::Excluded(_)
        ));
    }

    #[test]
    fn a_subtree_exclusion_is_recognised_at_the_directory_itself() {
        let s = set(&[], &["node_modules/**"]);
        // The pattern does not match the bare directory name, which is why resolving a
        // directory the way a file is resolved never pruned anything.
        assert!(matches!(
            s.resolve("node_modules", "node_modules"),
            Resolution::Neutral
        ));
        assert!(matches!(
            s.resolve_directory("node_modules", "node_modules"),
            Resolution::Excluded(_)
        ));
        assert!(matches!(
            s.resolve_directory("node_modules/a", "a"),
            Resolution::Excluded(_)
        ));
        assert!(matches!(
            s.resolve_directory("src", "src"),
            Resolution::Neutral
        ));
    }

    #[test]
    fn an_inclusion_reaching_below_outranks_a_subtree_exclusion() {
        let s = set(&["node_modules/keep/**"], &["node_modules/**"]);
        assert!(matches!(
            s.resolve_directory("node_modules", "node_modules"),
            Resolution::Neutral
        ));
    }

    #[test]
    fn a_pruned_directory_is_still_descended_when_an_inclusion_reaches_into_it() {
        let s = set(&[".github/**"], &[]);
        assert!(s.may_include_below(".github"));
        assert!(!s.may_include_below("src"));

        // A pattern naming no directory may match at any depth, so nothing may be pruned.
        let anywhere = set(&["*.rs"], &[]);
        assert!(anywhere.may_include_below("target"));

        // No inclusion patterns at all: pruning is unconstrained.
        let none = set(&[], &["*.rs"]);
        assert!(!none.may_include_below("target"));
    }

    #[test]
    fn literal_prefixes_stop_at_the_first_metacharacter() {
        assert_eq!(
            literal_prefix("src/generated/*.rs").as_deref(),
            Some("src/generated")
        );
        assert_eq!(literal_prefix("src/gen*/a.rs").as_deref(), Some("src"));
        assert_eq!(literal_prefix("src/main.rs").as_deref(), Some("src"));
        assert_eq!(literal_prefix("*.toml"), None);
        assert_eq!(literal_prefix("**/*.rs"), None);
    }

    #[test]
    fn a_tie_reports_the_pattern_written_first() {
        let s = set(&["first/a.rs", "second/a.rs"], &[]);
        // Both score identically; the earlier one is the one reported.
        let Resolution::Included(matched) = s.resolve("first/a.rs", "a.rs") else {
            panic!("expected an inclusion");
        };
        assert_eq!(matched.pattern, "first/a.rs");
    }

    #[test]
    fn an_overlapping_classification_override_is_reported_as_contested() {
        let overrides = ClassificationOverrides::compile(
            &["data/*.bin".to_string()],
            &["*.bin".to_string()],
            false,
        )
        .unwrap();
        assert!(overrides.is_contested("data/a.bin", "a.bin"));
        assert_eq!(overrides.decide("data/a.bin", "a.bin"), Some(true));
        assert!(!overrides.is_contested("other/a.bin", "a.bin"));
    }

    #[test]
    fn resolution_is_stable_across_repeated_calls() {
        let s = set(&["src/*.rs", "*.rs"], &["src/main.rs"]);
        let first = s.resolve("src/main.rs", "main.rs");
        for _ in 0..64 {
            assert_eq!(first, s.resolve("src/main.rs", "main.rs"));
        }
    }
}
