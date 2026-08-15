use std::cmp::Ordering;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::config::{
    looks_like_a_credential, ClassificationOverrides, FilterSettings, PatternSet, Resolution,
    SymlinkPolicy, DEFAULT_EXCLUDED_DIRS, DEFAULT_EXCLUDED_FILES,
};
use crate::error::Result;
use crate::report::{ExclusionReason, FileRecord, Progress, WarningKind, WarningRecord};

use super::classify::{
    classify, contains_disallowed_control, detect_bom, guess_encoding, is_disallowed_control_char,
    Classification, PREFIX_BYTES,
};
use super::ignore_rules::{DirCache, IgnoreDecision, IgnoreResolver};
use super::source::AcquiredSource;

/// Largest file read into memory during composition when no explicit limit was given.
///
/// Composition reads each file whole — a markdown fence's width depends on the longest
/// backtick run anywhere in the content, so the file cannot be framed until all of it has
/// been seen. Without a ceiling, a single enormous text file exhausts memory in a
/// pipeline that is otherwise careful never to hold more than one file at a time.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;

/// Files seen per progress update during traversal.
///
/// The indicator's own ticker owns the redraw; this only decides how often the shared
/// counter is published to it, and one relaxed atomic store per batch costs nothing.
const PROGRESS_STRIDE: usize = 64;

/// A file that survived filtering and will contribute content to the document.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub absolute: PathBuf,
    pub display: String,
    pub size: u64,
    pub classification: Classification,
}

#[derive(Debug, Default)]
pub struct DiscoveryOutcome {
    pub candidates: Vec<Candidate>,
    pub records: Vec<FileRecord>,
    pub warnings: Vec<WarningRecord>,
    pub discovered: usize,
    /// Directories pruned at their boundary, counted apart from the files in `records` so
    /// that discovered, included and excluded reconcile with one another.
    pub pruned_directories: usize,
}

/// Paths this run is writing, which must never become part of what it packages.
#[derive(Debug, Default, Clone)]
pub struct ReservedPaths {
    /// The finished document, where it is written to a file.
    pub destination: Option<PathBuf>,
    /// The directory the staging file is created in, if there is one.
    pub staging_directory: Option<PathBuf>,
}

/// Compiled filtering state shared by every traversal thread.
struct FilterContext {
    patterns: PatternSet,
    overrides: ClassificationOverrides,
    settings: FilterSettings,
    /// The document this run is writing, if it happens to sit inside a source.
    reserved: Option<PathBuf>,
    /// Its final path segment, checked first so the expensive comparison runs at most
    /// once per identically-named file rather than once per file in the tree.
    reserved_name: Option<std::ffi::OsString>,
    /// Directory the staging file lives in. Anything in there carrying the staging prefix
    /// is a document being assembled right now — by this run or by a concurrent one.
    staging_directory: Option<PathBuf>,
}

impl FilterContext {
    /// Whether this path is a document being written rather than source to package.
    ///
    /// Both tests lead with a name comparison, which costs nothing and fails for every
    /// file in an ordinary tree, so the path comparisons behind them run essentially
    /// never — worth arranging, because they are the expensive part.
    fn is_a_document_in_flight(&self, path: &Path, file_name: &std::ffi::OsStr) -> bool {
        if self.reserved_name.as_deref() == Some(file_name) {
            let matches_destination = self.reserved.as_deref().is_some_and(|reserved| {
                crate::paths::same(path, reserved)
                    || std::fs::canonicalize(path)
                        .is_ok_and(|actual| crate::paths::same(&actual, reserved))
            });
            if matches_destination {
                return true;
            }
        }

        // The staging file exists on disk from the moment delivery opens until the moment
        // it is renamed into place. It is empty while the walk runs, and an empty file
        // classifies as text, so without this it becomes a candidate and contributes an
        // empty section under a random name. A concurrent run's staging file is caught by
        // the same test, and that one is not empty.
        if !file_name
            .to_string_lossy()
            .starts_with(crate::delivery::STAGING_PREFIX)
        {
            return false;
        }
        crate::paths::same_option(path.parent(), self.staging_directory.as_deref())
    }
}

#[derive(Debug)]
enum Entry {
    Candidate(Box<Candidate>),
    Excluded(Box<FileRecord>),
    PrunedDirectory(Box<FileRecord>),
    Warning(Box<WarningRecord>),
}

/// Per-thread accumulator, merged into the shared collection when its thread finishes.
struct Batch {
    local: Vec<Entry>,
    shared: Arc<Mutex<Vec<Entry>>>,
}

impl Batch {
    fn exclude(
        &mut self,
        display: &str,
        reason: ExclusionReason,
        attribution: Option<String>,
        size: u64,
    ) {
        self.local.push(Entry::Excluded(Box::new(FileRecord {
            path: display.to_string(),
            size,
            excluded: Some(reason),
            attribution,
            encoding: None,
            compressed: false,
            tokens: None,
        })));
    }

    fn prune(&mut self, display: &str, reason: ExclusionReason, attribution: Option<String>) {
        self.local.push(Entry::PrunedDirectory(Box::new(FileRecord {
            path: format!("{display}/"),
            size: 0,
            excluded: Some(reason),
            attribution,
            encoding: None,
            compressed: false,
            tokens: None,
        })));
    }

    fn warn(&mut self, display: &str, kind: WarningKind) {
        self.local
            .push(Entry::Warning(Box::new(WarningRecord::about(
                display, kind,
            ))));
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        let mut shared = match self.shared.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        shared.append(&mut self.local);
    }
}

/// Discover every candidate file across all acquired sources.
///
/// Traversal is parallel and collects only paths and lightweight metadata; no file's full
/// content is read here, and the nondeterministic order in which threads finish is
/// resolved into one stable ordering before anything is composed.
pub fn discover(
    sources: &[AcquiredSource],
    filters: &FilterSettings,
    reserved: &ReservedPaths,
    progress: &dyn Progress,
) -> Result<DiscoveryOutcome> {
    let context = Arc::new(FilterContext {
        patterns: PatternSet::compile(
            &filters.include,
            &filters.exclude,
            filters.case_insensitive,
        )?,
        overrides: ClassificationOverrides::compile(
            &filters.force_text,
            &filters.force_binary,
            filters.case_insensitive,
        )?,
        settings: filters.clone(),
        reserved: reserved.destination.clone(),
        reserved_name: reserved
            .destination
            .as_deref()
            .and_then(Path::file_name)
            .map(std::ffi::OsStr::to_os_string),
        staging_directory: reserved.staging_directory.clone(),
    });

    let source_roots: Arc<Vec<PathBuf>> =
        Arc::new(sources.iter().map(|s| s.root.clone()).collect());
    let multiple = sources.len() > 1;
    let collected: Arc<Mutex<Vec<Entry>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(AtomicUsize::new(0));

    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);

    for source in sources {
        // A file designated directly has no directory of its own; the ignore rules that
        // govern it are the ones that govern the directory it sits in.
        let resolver_root = if source.is_file {
            source.root.parent().unwrap_or(&source.root).to_path_buf()
        } else {
            source.root.clone()
        };
        let resolver = Arc::new(IgnoreResolver::new(
            &resolver_root,
            filters.use_vcs_ignore,
            filters.use_tool_ignore && source.honor_tool_ignore,
            filters.case_insensitive,
        ));
        let prefix: Option<String> = multiple.then(|| source.prefix.clone());

        let mut builder = WalkBuilder::new(&source.root);
        builder
            .standard_filters(false)
            .follow_links(filters.symlinks == SymlinkPolicy::Always)
            .threads(threads);

        builder.build_parallel().run(|| {
            let context = Arc::clone(&context);
            let resolver = Arc::clone(&resolver);
            let roots = Arc::clone(&source_roots);
            let prefix = prefix.clone();
            let root = source.root.clone();
            let seen = Arc::clone(&seen);
            let mut batch = Batch {
                local: Vec::new(),
                shared: Arc::clone(&collected),
            };
            let mut scratch = Scratch::default();
            let mut since_report = 0_usize;

            Box::new(move |result| {
                let entry = match result {
                    Ok(entry) => entry,
                    Err(error) => {
                        batch
                            .local
                            .push(Entry::Warning(Box::new(warning_for(&error))));
                        return WalkState::Continue;
                    }
                };

                let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());

                // A directory root contributes nothing itself. A *file* root is the only
                // entry the walk will ever yield, so discarding every depth-zero entry is
                // what made `mhrn src/main.rs` package nothing at all.
                if entry.depth() == 0 && is_directory {
                    return WalkState::Continue;
                }

                let relative = relative_path(&root, entry.path());
                let display = prefixed_path(prefix.as_deref(), &relative);

                if is_directory {
                    return visit_directory(
                        &context,
                        &resolver,
                        &entry,
                        &relative,
                        &display,
                        &mut batch,
                        &mut scratch,
                    );
                }

                let count = seen.fetch_add(1, AtomicOrdering::Relaxed) + 1;
                since_report += 1;
                if since_report >= PROGRESS_STRIDE {
                    since_report = 0;
                    // Published as the walk goes rather than once it has finished, which
                    // left the indicator showing nothing during the phase a large tree
                    // spends most of its time in.
                    progress.discovered(count);
                }

                visit_file(
                    &context,
                    &resolver,
                    &roots,
                    &entry,
                    &relative,
                    display,
                    &mut batch,
                    &mut scratch,
                );
                WalkState::Continue
            })
        });

        progress.discovered(seen.load(AtomicOrdering::Relaxed));
    }

    let entries = {
        let mut guard = match collected.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *guard)
    };

    let mut outcome = DiscoveryOutcome {
        discovered: seen.load(AtomicOrdering::Relaxed),
        ..Default::default()
    };
    for entry in entries {
        match entry {
            Entry::Candidate(candidate) => outcome.candidates.push(*candidate),
            Entry::Excluded(record) => outcome.records.push(*record),
            Entry::PrunedDirectory(record) => {
                outcome.pruned_directories += 1;
                outcome.records.push(*record);
            }
            Entry::Warning(warning) => outcome.warnings.push(*warning),
        }
    }

    let credentials: Vec<&str> = outcome
        .records
        .iter()
        .filter(|record| record.excluded.as_ref() == Some(&ExclusionReason::CredentialLike))
        .map(|record| record.path.as_str())
        .collect();
    if !credentials.is_empty() {
        // Named rather than counted. A count is exactly what a reader skims past, and the
        // whole value of the category is that the user notices what it caught.
        let detail = summarise_paths(&credentials);
        outcome
            .warnings
            .push(WarningRecord::global(WarningKind::CredentialsExcluded(
                detail,
            )));
    }

    for candidate in &outcome.candidates {
        outcome.records.push(FileRecord {
            path: candidate.display.clone(),
            size: candidate.size,
            excluded: None,
            attribution: None,
            encoding: match &candidate.classification {
                Classification::Text { encoding, .. } => Some(encoding.name().to_string()),
                _ => None,
            },
            compressed: false,
            tokens: None,
        });
    }

    outcome
        .candidates
        .sort_by(|a, b| compare_display_paths(&a.display, &b.display));
    outcome
        .records
        .sort_by(|a, b| compare_display_paths(&a.path, &b.path));
    outcome.warnings.sort();

    Ok(outcome)
}

fn summarise_paths(paths: &[&str]) -> String {
    const SHOWN: usize = 6;
    let shown = paths
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match paths.len().saturating_sub(SHOWN) {
        0 => shown,
        rest => format!("{shown}, and {rest} more"),
    }
}

fn visit_directory(
    context: &FilterContext,
    resolver: &IgnoreResolver,
    entry: &DirEntry,
    relative: &str,
    display: &str,
    batch: &mut Batch,
    scratch: &mut Scratch,
) -> WalkState {
    let name = entry.file_name().to_string_lossy();

    // Invocation-time patterns sit above every other rule source for directories exactly
    // as they do for files. Consulting them only in `visit_file` is what made `--include
    // '.github/**'` return nothing: the directory was pruned before any file inside it
    // was ever offered to the pattern set.
    match context.patterns.resolve_directory(relative, &name) {
        Resolution::Excluded(matched) => {
            // Pruning here is also what stops `--exclude 'node_modules/**'` from walking
            // the whole subtree and emitting one record per file inside it.
            batch.prune(
                display,
                ExclusionReason::ExclusionPattern,
                Some(matched.pattern),
            );
            return WalkState::Skip;
        }
        Resolution::Included(_) => return WalkState::Continue,
        Resolution::Neutral | Resolution::NotIncluded => {}
    }

    let verdict = if context.settings.use_default_exclusions
        && DEFAULT_EXCLUDED_DIRS.contains(&name.as_ref())
    {
        // Checked before the hidden rule so that `.git` and its kind are attributed to
        // the list they are actually on, instead of changing reason under `--hidden`.
        Some((ExclusionReason::DefaultExclusion, name.into_owned()))
    } else if !context.settings.include_hidden && is_hidden_name(&name) {
        Some((ExclusionReason::Hidden, name.into_owned()))
    } else {
        match resolver.decide_cached(&mut scratch.ignores, entry.path(), true) {
            IgnoreDecision::Ignored { reason, rule } => Some((reason, rule)),
            IgnoreDecision::Allowed => None,
        }
    };

    let Some((reason, attribution)) = verdict else {
        return WalkState::Continue;
    };

    if context.settings.enumerate_pruned {
        // Descending anyway costs one directory read per level but lets every file below
        // carry its own attribution instead of being summarised at the boundary.
        return WalkState::Continue;
    }

    // An inclusion pattern reaching into this directory outranks the rule that would
    // otherwise prune it. Descending is the only way to find out whether anything inside
    // actually matches, and every file is still filtered on the way past.
    if context.patterns.may_include_below(relative) {
        return WalkState::Continue;
    }

    // The exclusion record carries the reason and the rule that produced it; a directory
    // the user asked to be skipped is not something to warn about.
    batch.prune(display, reason, Some(attribution));
    WalkState::Skip
}

#[allow(clippy::too_many_arguments)]
fn visit_file(
    context: &FilterContext,
    resolver: &IgnoreResolver,
    roots: &[PathBuf],
    entry: &DirEntry,
    relative: &str,
    display: String,
    batch: &mut Batch,
    scratch: &mut Scratch,
) {
    let path = entry.path();
    let file_name = entry.file_name().to_string_lossy();

    // Packaging the previous run's output would double the document and swamp every
    // per-file statistic in it, so it is refused before any rule is consulted.
    if context.is_a_document_in_flight(path, entry.file_name()) {
        batch.exclude(&display, ExclusionReason::OutputDocument, None, 0);
        return;
    }

    // A path no output format can carry is not a candidate in any format, and settling
    // that here keeps every writer free of the question.
    if contains_disallowed_control(&display) {
        batch.exclude(
            &display.replace(is_disallowed_control_char, "?"),
            ExclusionReason::ControlCharacters,
            Some("path contains control characters".to_string()),
            0,
        );
        return;
    }

    // Patterns are resolved before the link is, so a link the user explicitly excluded
    // does not still produce a diagnostic — and, with warnings governing the exit status,
    // does not make an otherwise clean run report a problem.
    match context.patterns.resolve(relative, &file_name) {
        Resolution::Excluded(matched) => {
            batch.exclude(
                &display,
                ExclusionReason::ExclusionPattern,
                Some(matched.pattern),
                0,
            );
            return;
        }
        Resolution::NotIncluded => {
            batch.exclude(
                &display,
                ExclusionReason::NoInclusionPatternMatched,
                None,
                0,
            );
            return;
        }
        // An explicit inclusion is the user's most deliberate statement about this file
        // and therefore outranks every rule source that could otherwise drop it.
        Resolution::Included(_) => {}
        Resolution::Neutral => {
            if let IgnoreDecision::Ignored { reason, rule } =
                resolver.decide_cached(&mut scratch.ignores, path, false)
            {
                batch.exclude(&display, reason, Some(rule), 0);
                return;
            }
            if !context.settings.include_hidden && has_hidden_component(relative, &file_name) {
                batch.exclude(
                    &display,
                    ExclusionReason::Hidden,
                    Some(file_name.into_owned()),
                    0,
                );
                return;
            }
            if context.settings.use_default_exclusions && is_default_excluded(relative) {
                batch.exclude(
                    &display,
                    ExclusionReason::DefaultExclusion,
                    Some("built-in exclusion".to_string()),
                    0,
                );
                return;
            }
            if !context.settings.allow_secrets && looks_like_a_credential(&file_name) {
                batch.exclude(
                    &display,
                    ExclusionReason::CredentialLike,
                    Some(file_name.into_owned()),
                    0,
                );
                return;
            }
        }
    }

    let Some(target) = resolve_symlink(context, roots, entry, &display, batch) else {
        return;
    };

    // The walker already has this entry's metadata from the directory read on most
    // platforms; asking it avoids a second stat for every file in the tree.
    let metadata = if target == path {
        entry
            .metadata()
            .map(|m| m.len())
            .map_err(std::io::Error::other)
    } else {
        std::fs::metadata(&target).map(|m| m.len())
    };

    let size = match metadata {
        Ok(length) => length,
        Err(error) => {
            batch.exclude(
                &display,
                ExclusionReason::Unreadable,
                Some(error.to_string()),
                0,
            );
            batch.warn(&display, WarningKind::Unreadable(error.to_string()));
            return;
        }
    };

    match context.settings.max_file_size {
        Some(limit) if size > limit => {
            batch.exclude(
                &display,
                ExclusionReason::SizeThreshold,
                Some(format!("{size} bytes exceeds the {limit} byte limit")),
                size,
            );
            return;
        }
        // Composition holds one file at a time in memory by design; with no limit at all
        // that becomes the whole of memory for one pathological file.
        None if size > DEFAULT_MAX_FILE_SIZE => {
            batch.exclude(
                &display,
                ExclusionReason::SizeThreshold,
                Some(format!(
                    "{size} bytes exceeds the {DEFAULT_MAX_FILE_SIZE} byte default limit; \
                     raise it with --max-size"
                )),
                size,
            );
            return;
        }
        _ => {}
    }

    if context.overrides.is_contested(relative, &file_name) {
        // Resolved rather than refused, but not silently: the user named the same file
        // twice with opposite instructions and should hear about it.
        batch.warn(&display, WarningKind::ContestedClassification);
    }
    let forced = context.overrides.decide(relative, &file_name);
    if forced == Some(false) {
        batch.exclude(
            &display,
            ExclusionReason::BinaryContent,
            Some("classification override".into()),
            size,
        );
        return;
    }

    let classification = match read_prefix(&target, size, &mut scratch.prefix) {
        Ok((prefix, complete)) => {
            let detected = classify(prefix, complete);
            match (forced, detected) {
                (
                    Some(true),
                    Classification::Text {
                        encoding,
                        bom_length,
                    },
                ) => Classification::Text {
                    encoding,
                    bom_length,
                },
                // An override is an instruction, not a hint: decode with whatever
                // encoding can represent these bytes rather than failing on the first
                // one that is not UTF-8.
                (Some(true), _) => Classification::Text {
                    encoding: guess_encoding(prefix, complete),
                    bom_length: detect_bom(prefix).map_or(0, |(_, length)| length),
                },
                (_, detected) => detected,
            }
        }
        Err(error) => {
            batch.exclude(
                &display,
                ExclusionReason::Unreadable,
                Some(error.to_string()),
                size,
            );
            batch.warn(&display, WarningKind::Unreadable(error.to_string()));
            return;
        }
    };

    match classification {
        Classification::Binary => {
            batch.exclude(&display, ExclusionReason::BinaryContent, None, size);
        }
        Classification::Undetermined => {
            batch.exclude(&display, ExclusionReason::UndeterminedEncoding, None, size);
            batch.warn(&display, WarningKind::EncodingUndetermined);
        }
        classification => {
            batch.local.push(Entry::Candidate(Box::new(Candidate {
                absolute: target,
                display,
                size,
                classification,
            })));
        }
    }
}

/// Resolve a symbolic link under the active policy, or record why it was not followed.
fn resolve_symlink(
    context: &FilterContext,
    roots: &[PathBuf],
    entry: &DirEntry,
    display: &str,
    batch: &mut Batch,
) -> Option<PathBuf> {
    let path = entry.path().to_path_buf();
    if !entry.path_is_symlink() || context.settings.symlinks == SymlinkPolicy::Always {
        return Some(path);
    }

    if context.settings.symlinks == SymlinkPolicy::Never {
        batch.exclude(display, ExclusionReason::SymlinkNotFollowed, None, 0);
        batch.warn(display, WarningKind::SymlinkNotFollowed);
        return None;
    }

    match std::fs::canonicalize(&path) {
        Ok(resolved) if !roots.iter().any(|root| resolved.starts_with(root)) => {
            batch.exclude(
                display,
                ExclusionReason::ExternalSymlink,
                Some(resolved.to_string_lossy().into_owned()),
                0,
            );
            batch.warn(display, WarningKind::ExternalSymlink);
            None
        }
        // Contained, but a directory. The traversal cannot be told to follow one link and
        // not another, so a directory link is reported under a reason that says so rather
        // than under one that describes an uncontained target. `--symlinks always`
        // follows it, and the policy documentation now states this rather than implying
        // the opposite. Not a warning: it is the stated behaviour of the chosen policy.
        Ok(resolved) if resolved.is_dir() => {
            batch.exclude(
                display,
                ExclusionReason::SymlinkedDirectory,
                Some(resolved.to_string_lossy().into_owned()),
                0,
            );
            None
        }
        Ok(resolved) => Some(resolved),
        Err(error) => {
            batch.exclude(
                display,
                ExclusionReason::Unreadable,
                Some(error.to_string()),
                0,
            );
            batch.warn(display, WarningKind::Unreadable(error.to_string()));
            None
        }
    }
}

/// Per-thread working memory, so the per-file path allocates nothing it can reuse.
#[derive(Default)]
struct Scratch {
    ignores: DirCache,
    prefix: Vec<u8>,
}

/// Read the leading bytes used for classification into the caller's buffer.
///
/// The buffer is reused across files: a tree of a hundred thousand files would otherwise
/// allocate and zero the same eight kilobytes a hundred thousand times.
fn read_prefix<'a>(
    path: &Path,
    size: u64,
    buffer: &'a mut Vec<u8>,
) -> std::io::Result<(&'a [u8], bool)> {
    let capacity = PREFIX_BYTES.min(size.max(1) as usize);
    buffer.clear();
    buffer.resize(capacity, 0);

    let mut file = File::open(path)?;
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    buffer.truncate(filled);
    let complete = (filled as u64) >= size;
    Ok((&buffer[..], complete))
}

fn warning_for(error: &ignore::Error) -> WarningRecord {
    match error {
        ignore::Error::Loop { child, .. } => WarningRecord::about(
            child.to_string_lossy().into_owned(),
            WarningKind::SymlinkCycle,
        ),
        ignore::Error::WithPath { path, err } => WarningRecord::about(
            path.to_string_lossy().into_owned(),
            WarningKind::Unreadable(err.to_string()),
        ),
        other => WarningRecord::global(WarningKind::Unreadable(other.to_string())),
    }
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

fn has_hidden_component(relative: &str, file_name: &str) -> bool {
    is_hidden_name(file_name) || relative.split('/').any(is_hidden_name)
}

fn is_default_excluded(relative: &str) -> bool {
    let mut segments = relative.split('/').peekable();
    while let Some(segment) = segments.next() {
        if DEFAULT_EXCLUDED_DIRS.contains(&segment) {
            return true;
        }
        // The file list applies to the final segment only, so a directory that happens to
        // share a lock file's name does not take its contents with it.
        if segments.peek().is_none() && DEFAULT_EXCLUDED_FILES.contains(&segment) {
            return true;
        }
    }
    false
}

/// Path relative to a source's own root, in forward-slash form.
///
/// Every filtering decision is made against this and never against the display path. The
/// display path carries a source prefix when more than one source is designated, and
/// matching rules against it made the outcome depend on how many sources were named:
/// `mhrn target other/` dropped everything under `target` because `target` had become a
/// segment of every path, and `mhrn .config other/` dropped everything as hidden.
fn relative_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut out = String::with_capacity(relative.as_os_str().len());
    for component in relative.components() {
        if let Component::Normal(part) = component {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&part.to_string_lossy());
        }
    }
    if out.is_empty() {
        // A file designated directly as a source strips to nothing against itself; it is
        // known by its own name rather than by an empty path.
        if let Some(name) = path.file_name() {
            out.push_str(&name.to_string_lossy());
        }
    }
    out
}

/// The path as it appears in the document and in reporting.
fn prefixed_path(prefix: Option<&str>, relative: &str) -> String {
    match prefix {
        Some(prefix) if !relative.is_empty() => format!("{prefix}/{relative}"),
        Some(prefix) => prefix.to_string(),
        None => relative.to_string(),
    }
}

/// Ordering that walks the hierarchy the way the structural overview draws it.
///
/// Segment by segment, with directories before files at each level, so the sequence of
/// file sections in the document is the sequence a reader has just seen in the overview.
/// Comparing whole paths would not do: `/` sorts after `.` and `-`, which interleaves a
/// directory's contents with its siblings.
pub fn compare_display_paths(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split('/').peekable();
    let mut right_parts = right.split('/').peekable();
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                if a == b {
                    continue;
                }
                let left_is_directory = left_parts.peek().is_some();
                let right_is_directory = right_parts.peek().is_some();
                return match (left_is_directory, right_is_directory) {
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    _ => a.as_bytes().cmp(b.as_bytes()),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_compares_segment_by_segment() {
        let mut paths = vec![
            "src/b.rs".to_string(),
            "src.old/a.rs".to_string(),
            "src/a.rs".to_string(),
            "README.md".to_string(),
        ];
        paths.sort_by(|a, b| compare_display_paths(a, b));

        // Directories first, exactly as the structural overview draws them.
        assert_eq!(
            paths,
            vec!["src/a.rs", "src/b.rs", "src.old/a.rs", "README.md"]
        );
    }

    #[test]
    fn a_directorys_files_stay_together() {
        let mut paths = vec![
            "a-b/second.rs".to_string(),
            "a/first.rs".to_string(),
            "a/nested/deep.rs".to_string(),
            "a!/third.rs".to_string(),
        ];
        paths.sort_by(|a, b| compare_display_paths(a, b));
        assert_eq!(
            paths,
            vec![
                "a/nested/deep.rs",
                "a/first.rs",
                "a!/third.rs",
                "a-b/second.rs",
            ]
        );
    }

    #[test]
    fn ordering_is_antisymmetric() {
        let paths = ["a", "a/b", "a/b/c", "b", "A"];
        for left in paths {
            for right in paths {
                assert_eq!(
                    compare_display_paths(left, right).reverse(),
                    compare_display_paths(right, left)
                );
            }
        }
    }

    #[test]
    fn display_paths_use_forward_slashes_and_honour_prefixes() {
        let root = Path::new("/tmp/project");
        let path = Path::new("/tmp/project/src/main.rs");
        assert_eq!(relative_path(root, path), "src/main.rs");
        assert_eq!(
            prefixed_path(Some("proj"), &relative_path(root, path)),
            "proj/src/main.rs"
        );
    }

    #[test]
    fn a_file_designated_as_a_source_is_known_by_its_own_name() {
        let root = Path::new("/tmp/project/src/main.rs");
        assert_eq!(relative_path(root, root), "main.rs");
    }

    #[test]
    fn rules_are_evaluated_against_the_unprefixed_path() {
        // A source named `target` must not make every file under it look like a build
        // directory, and `.config` must not make every file under it look hidden.
        assert!(is_default_excluded("target/debug/x.rs"));
        assert!(!is_default_excluded("x.rs"));
        assert!(has_hidden_component(".config/x", "x"));
        assert!(!has_hidden_component("x", "x"));
    }
}
