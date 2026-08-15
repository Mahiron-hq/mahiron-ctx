mod console;
mod progress;

pub use console::render_console;
pub use progress::{NullProgress, Progress, TerminalProgress};

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{LineEnding, OutputFormat};

/// Number of files sharing one outcome above which the console summary aggregates it.
pub const AGGREGATION_THRESHOLD: usize = 10;

/// Why a discovered entry did not contribute content to the output document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionReason {
    IgnoreRules,
    ToolIgnoreRules,
    ExclusionPattern,
    NoInclusionPatternMatched,
    DefaultExclusion,
    Hidden,
    SizeThreshold,
    BinaryContent,
    UndeterminedEncoding,
    ExternalSymlink,
    SymlinkNotFollowed,
    SymlinkCycle,
    Unreadable,
    /// Content that no output format can carry, found only once the whole file was read.
    ControlCharacters,
    /// A name that identifies the file as holding credentials.
    CredentialLike,
    /// A symbolic link to a directory, which the contained policy cannot descend.
    SymlinkedDirectory,
    /// The document this run is itself writing, or the staging file it is written through.
    OutputDocument,
}

impl ExclusionReason {
    pub fn label(&self) -> &'static str {
        match self {
            Self::IgnoreRules => "version-control ignore rules",
            Self::ToolIgnoreRules => "tool ignore file",
            Self::ExclusionPattern => "exclusion pattern",
            Self::NoInclusionPatternMatched => "no inclusion pattern matched",
            Self::DefaultExclusion => "default exclusion",
            Self::Hidden => "hidden entry",
            Self::SizeThreshold => "size threshold",
            Self::BinaryContent => "binary content",
            Self::UndeterminedEncoding => "unreadable / encoding",
            Self::ExternalSymlink => "symbolic link outside every source",
            Self::SymlinkNotFollowed => "symbolic link not followed",
            Self::SymlinkCycle => "symbolic link cycle",
            Self::Unreadable => "unreadable / encoding",
            Self::ControlCharacters => "control characters no format can carry",
            Self::CredentialLike => "looks like a credential",
            Self::SymlinkedDirectory => "symbolic link to a directory",
            Self::OutputDocument => "this run's own output document",
        }
    }
}

impl fmt::Display for ExclusionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A condition worth surfacing that did not by itself stop the run.
///
/// Routine exclusions are not warnings: a binary file skipped by design or a directory
/// pruned by a rule the user asked for is reported in the exclusion breakdown, and
/// raising it here as well would make a run with warnings the normal case and cost the
/// exit status its meaning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum WarningKind {
    EncodingUndetermined,
    Unreadable(String),
    ExternalSymlink,
    SymlinkNotFollowed,
    SymlinkCycle,
    /// Raised once for the whole run, not once per file: on a mixed-language repository
    /// an unsupported language is the expected case, and one record per file would bury
    /// every other warning and say nothing the aggregate does not.
    CompressionUnsupported(String),
    CompressionFailed(String),
    MixedLineEndings,
    ControlCharacters,
    ContestedClassification,
    CredentialsExcluded(String),
    ClipboardUnavailable(String),
    UntrustedRemoteConfigIgnored,
    TokenCountMismatch(String),
}

/// Whether a condition is something to act on, or something merely worth knowing.
///
/// Only [`Severity::Warning`] affects the exit status. Without the distinction, a
/// repository that merely mixes line endings — the normal state of anything touched from
/// both Windows and Unix — makes a completely successful run look like a failure to CI,
/// and the exit code stops carrying information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Notice,
    Warning,
}

impl WarningKind {
    /// Stable text used both as the aggregation key and as the printed summary line.
    pub fn summary(&self) -> String {
        match self {
            Self::EncodingUndetermined => "encoding could not be determined, file skipped".into(),
            Self::Unreadable(_) => "file could not be read".into(),
            Self::ExternalSymlink => {
                "symbolic link resolves outside every designated source, not followed".into()
            }
            Self::SymlinkNotFollowed => "symbolic link not followed".into(),
            Self::SymlinkCycle => "symbolic link cycle avoided".into(),
            Self::CompressionUnsupported(_) => {
                "compression requested but not supported for file's language".into()
            }
            Self::CompressionFailed(_) => "compression failed, file included verbatim".into(),
            Self::MixedLineEndings => "project mixes line-ending conventions".into(),
            Self::ControlCharacters => {
                "content contains control characters no output format can carry".into()
            }
            Self::ContestedClassification => "named by both --force-text and --force-binary".into(),
            Self::CredentialsExcluded(_) => {
                "files that look like credentials were left out; pass --allow-secrets to \
                 package them"
                    .into()
            }
            Self::ClipboardUnavailable(m) => format!("clipboard unavailable: {m}"),
            Self::UntrustedRemoteConfigIgnored => {
                "remote source's own ignore file was packaged, not applied".into()
            }
            Self::TokenCountMismatch(m) => format!("token count mismatch: {m}"),
        }
    }

    /// Extra qualifier folded into an aggregated line, such as the languages involved.
    pub fn qualifier(&self) -> Option<&str> {
        match self {
            Self::Unreadable(detail)
            | Self::CompressionFailed(detail)
            | Self::CompressionUnsupported(detail)
            | Self::CredentialsExcluded(detail)
            | Self::TokenCountMismatch(detail) => Some(detail),
            _ => None,
        }
    }

    /// Whether this condition should affect the exit status.
    pub fn severity(&self) -> Severity {
        match self {
            // Informational: nothing was lost, nothing needs doing, and the run is clean.
            Self::MixedLineEndings
            | Self::CompressionUnsupported(_)
            | Self::CredentialsExcluded(_)
            | Self::UntrustedRemoteConfigIgnored => Severity::Notice,
            _ => Severity::Warning,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WarningRecord {
    /// Relative display path, or `None` for a warning about the run as a whole.
    pub path: Option<String>,
    pub kind: WarningKind,
    /// Language or extension label, when the warning is attributable to one.
    pub language: Option<String>,
}

impl WarningRecord {
    pub fn severity(&self) -> Severity {
        self.kind.severity()
    }

    pub fn about(path: impl Into<String>, kind: WarningKind) -> Self {
        Self {
            path: Some(path.into()),
            kind,
            language: None,
        }
    }

    pub fn global(kind: WarningKind) -> Self {
        Self {
            path: None,
            kind,
            language: None,
        }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// Per-file outcome, retained in full so that any aggregated count can be expanded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded: Option<ExclusionReason>,
    /// Pattern, rule or setting that produced the outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub compressed: bool,
    /// Counted only when the invocation asked for a per-file breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputStats {
    pub bytes: u64,
    pub lines: u64,
    /// Exact count for the reference encoding named alongside it.
    pub tokens: usize,
    pub token_encoding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status")]
pub enum RemoteOutcome {
    Succeeded {
        designation: String,
        reference: Option<String>,
        retained_at: Option<String>,
    },
    Failed {
        designation: String,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "target")]
pub enum DeliveryReport {
    File {
        path: String,
    },
    Stdout,
    Clipboard,
    /// The document was handed back to the caller rather than written anywhere. Distinct
    /// from [`Self::DryRun`]: the document exists, it simply travelled in the response.
    Retained,
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Success,
    SuccessWithWarnings,
    Failure,
}

impl RunStatus {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::SuccessWithWarnings => 1,
            Self::Failure => 2,
        }
    }
}

/// Everything a run produced, other than the output document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub source_label: String,
    pub format: OutputFormat,
    pub discovered: usize,
    pub included: usize,
    pub excluded: usize,
    /// Directories pruned at their boundary. Counted apart from `excluded`, which counts
    /// files, so that the three file counts reconcile with one another.
    pub directories_pruned: usize,
    pub exclusions: BTreeMap<ExclusionReason, usize>,
    pub records: Vec<FileRecord>,
    pub warnings: Vec<WarningRecord>,
    pub output: OutputStats,
    pub delivery: DeliveryReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteOutcome>,
    pub line_endings: BTreeMap<LineEnding, usize>,
    pub duration: Duration,
    /// Whether the invocation asked for the elapsed time to be shown.
    pub show_duration: bool,
    /// How many of the heaviest files the invocation asked to see.
    pub rank_files: usize,
    pub transformations: Vec<String>,
    pub dry_run: bool,
}

impl RunReport {
    /// Included files ordered by what they cost, largest first.
    pub fn heaviest_files(&self, count: usize) -> Vec<(&str, usize)> {
        let mut ranked: Vec<(&str, usize)> = self
            .records
            .iter()
            .filter(|record| record.excluded.is_none())
            .filter_map(|record| Some((record.path.as_str(), record.tokens?)))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        ranked.truncate(count);
        ranked
    }

    pub fn status(&self) -> RunStatus {
        if matches!(self.remote, Some(RemoteOutcome::Failed { .. })) {
            return RunStatus::Failure;
        }
        if self.actionable_warnings().next().is_some() {
            RunStatus::SuccessWithWarnings
        } else {
            RunStatus::Success
        }
    }

    /// The warnings that affect the exit status, as distinct from informational notices.
    pub fn actionable_warnings(&self) -> impl Iterator<Item = &WarningRecord> {
        self.warnings
            .iter()
            .filter(|warning| warning.severity() == Severity::Warning)
    }

    pub fn notices(&self) -> impl Iterator<Item = &WarningRecord> {
        self.warnings
            .iter()
            .filter(|warning| warning.severity() == Severity::Notice)
    }

    /// Warning counts keyed by their printable summary, in a stable order.
    pub fn aggregated_warnings(&self, severity: Severity) -> Vec<AggregatedWarning> {
        let mut buckets: BTreeMap<String, AggregatedWarning> = BTreeMap::new();
        for warning in self.warnings.iter().filter(|w| w.severity() == severity) {
            let key = warning.kind.summary();
            let entry = buckets
                .entry(key.clone())
                .or_insert_with(|| AggregatedWarning {
                    summary: key,
                    count: 0,
                    languages: Vec::new(),
                });
            entry.count += 1;
            if let Some(language) = &warning.language {
                if !entry.languages.iter().any(|l| l == language) {
                    entry.languages.push(language.clone());
                }
            }
        }
        let mut aggregated: Vec<_> = buckets.into_values().collect();
        aggregated.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.summary.cmp(&b.summary))
        });
        aggregated
    }

    /// Per-file detail behind one aggregated warning line.
    pub fn warnings_matching(&self, summary: &str) -> Vec<&WarningRecord> {
        self.warnings
            .iter()
            .filter(|w| w.kind.summary() == summary)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedWarning {
    pub summary: String,
    pub count: usize,
    pub languages: Vec<String>,
}

/// Byte count rendered in units a reader can hold in their head.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Thousands-separated integer, matching the register of the rest of the summary.
pub fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_inserts_separators() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_847), "1,847");
        assert_eq!(grouped(28_450), "28,450");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
    }

    #[test]
    fn token_counts_are_shown_in_full() {
        assert_eq!(grouped(341_492), "341,492");
        assert_eq!(grouped(842), "842");
    }
}
