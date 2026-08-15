mod patterns;

pub use patterns::{ClassificationOverrides, PatternMatch, PatternSet, Resolution};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Filenames that carry tool-specific ignore rules, in resolution order.
pub const TOOL_IGNORE_FILENAMES: [&str; 2] = [".mahironignore", ".mhrnignore"];

/// Default output filename when the user names no destination.
/// Used only where the source has no usable name of its own.
pub const DEFAULT_OUTPUT_STEM: &str = "mhrn-output";

/// Appended to the source's name, so a document is recognisable as this tool's work and
/// two projects packaged into the same directory cannot overwrite one another.
pub const DEFAULT_OUTPUT_SUFFIX: &str = "-mhrn";

/// Directory names pruned before any other rule is consulted.
pub const DEFAULT_EXCLUDED_DIRS: [&str; 26] = [
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    ".jj",
    "node_modules",
    "bower_components",
    "vendor",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".gradle",
    ".terraform",
    ".cargo",
    ".turbo",
];

/// Files excluded by default: machine-generated, enormous, and of no use to a reader of
/// the codebase. `--include` names one back in; `--no-default-excludes` restores them all.
pub const DEFAULT_EXCLUDED_FILES: [&str; 13] = [
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "go.sum",
    "flake.lock",
    "gradle.lockfile",
];

/// Whole file names that hold credentials often enough that packaging one by accident is
/// the most damaging thing this tool can do.
///
/// The output is almost always destined for a model, so an exclusion here is cheap and a
/// mistake here is not recoverable. `--allow-secrets` turns the category off entirely.
pub const CREDENTIAL_FILE_NAMES: [&str; 18] = [
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".envrc",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".dockercfg",
    "credentials",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "secring.gpg",
    "terraform.tfvars",
    "kubeconfig",
    ".htpasswd",
];

/// Extensions that identify a private key or a credential store wherever they appear.
pub const CREDENTIAL_EXTENSIONS: [&str; 6] = ["pem", "key", "p12", "pfx", "jks", "keystore"];

/// Whether a file name identifies something that looks like a credential.
///
/// Name-based on purpose: content-based detection of secrets is a guessing game with
/// false negatives, and the point of the category is that it can be trusted.
pub fn looks_like_a_credential(file_name: &str) -> bool {
    let lowered = file_name.to_ascii_lowercase();
    if CREDENTIAL_FILE_NAMES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(file_name))
    {
        return true;
    }
    // `.env.staging`, `.env.ci` and the rest of an unbounded family.
    if lowered.starts_with(".env.")
        && !lowered.ends_with(".example")
        && !lowered.ends_with(".sample")
    {
        return true;
    }
    lowered.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty() && CREDENTIAL_EXTENSIONS.contains(&extension)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Markdown,
    Text,
    Xml,
    Json,
}

impl OutputFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "text" | "txt" | "plain" => Ok(Self::Text),
            "xml" => Ok(Self::Xml),
            "json" => Ok(Self::Json),
            other => Err(Error::config(format!(
                "unknown output format `{other}`; expected one of markdown, text, xml, json"
            ))),
        }
    }

    /// Format implied by an output path's extension, where one is recognised.
    pub fn from_destination(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "txt" | "text" => Some(Self::Text),
            "xml" => Some(Self::Xml),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Text => "txt",
            Self::Xml => "xml",
            Self::Json => "json",
        }
    }

    /// Whether this format can carry a control character without losing it.
    ///
    /// Only JSON can: a string escapes them as `\uXXXX` and reads back byte-exact. XML
    /// 1.0 admits no representation at all, not even inside a CDATA section, and markdown
    /// and plain text would carry the raw byte straight into the document. A file holding
    /// one is therefore dropped for those three and reproduced faithfully for JSON, which
    /// is what lets `--force-text` on a genuinely binary file still mean something.
    pub fn carries_control_characters(self) -> bool {
        matches!(self, Self::Json)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Xml => "xml",
            Self::Json => "json",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TreeStyle {
    #[default]
    Ascii,
    Compact,
}

impl TreeStyle {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ascii" | "tree" => Ok(Self::Ascii),
            "compact" | "tags" => Ok(Self::Compact),
            other => Err(Error::config(format!(
                "unknown tree style `{other}`; expected ascii or compact"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    /// Symbolic links are never traversed; they are reported as excluded references.
    Never,
    /// Links to files are followed, but only while their target stays inside a designated
    /// source. Links to directories are never followed under this policy: the traversal
    /// cannot descend selectively, so a contained directory link is reported rather than
    /// walked. Use `always` to follow directory links.
    #[default]
    WithinSources,
    /// Links are followed wherever they point, with cycle protection still in force.
    Always,
}

impl SymlinkPolicy {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "never" | "skip" | "opaque" => Ok(Self::Never),
            "within-sources" | "within" | "contained" => Ok(Self::WithinSources),
            "always" | "follow" => Ok(Self::Always),
            other => Err(Error::config(format!(
                "unknown symlink policy `{other}`; expected never, within-sources or always"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lf" | "unix" | "\n" => Ok(Self::Lf),
            "crlf" | "windows" | "dos" => Ok(Self::Crlf),
            "cr" | "classic-mac" => Ok(Self::Cr),
            other => Err(Error::config(format!(
                "unknown line ending `{other}`; expected lf, crlf or cr"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Cr => "\r",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
            Self::Cr => "CR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TokenEncoding {
    #[default]
    Cl100kBase,
    O200kBase,
}

impl TokenEncoding {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "cl100k_base" | "cl100k" => Ok(Self::Cl100kBase),
            "o200k_base" | "o200k" => Ok(Self::O200kBase),
            other => Err(Error::config(format!(
                "unknown token encoding `{other}`; expected cl100k_base or o200k_base"
            ))),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cl100kBase => "cl100k_base",
            Self::O200kBase => "o200k_base",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

impl Verbosity {
    pub fn at_least(self, other: Verbosity) -> bool {
        self as u8 >= other as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReportFormat {
    #[default]
    Console,
    Json,
}

/// What the engine does when a single file cannot be processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FailurePolicy {
    /// Record the problem and keep going.
    #[default]
    Continue,
    /// Abort the whole run on the first problem.
    Strict,
}

/// Which languages, if any, the structural-signature transformation should apply to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CompressionRequest {
    #[default]
    Disabled,
    AllSupported,
    Languages(Vec<String>),
}

impl CompressionRequest {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// A source the user designated, before any acquisition step has run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceSpec {
    Local(PathBuf),
    Remote(RemoteSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSource {
    pub url: String,
    /// Branch, tag or commit identifying the single snapshot to retrieve.
    pub reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    File(PathBuf),
    Stdout,
    Clipboard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterSettings {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub case_insensitive: bool,
    pub use_vcs_ignore: bool,
    pub use_tool_ignore: bool,
    pub use_default_exclusions: bool,
    pub include_hidden: bool,
    pub max_file_size: Option<u64>,
    pub symlinks: SymlinkPolicy,
    pub force_text: Vec<String>,
    pub force_binary: Vec<String>,
    /// Whether files kept out of the content body still appear in the structural overview.
    pub show_excluded_in_tree: bool,
    /// Enumerate the contents of pruned directories so every file is individually attributable.
    pub enumerate_pruned: bool,
    /// Package files whose names identify them as credentials, which are excluded by default.
    pub allow_secrets: bool,
    /// Remote sources, named exactly as designated, whose own ignore file the user trusts.
    pub trusted_remote_config: Vec<String>,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            case_insensitive: cfg!(any(windows, target_os = "macos")),
            use_vcs_ignore: true,
            use_tool_ignore: true,
            use_default_exclusions: true,
            include_hidden: false,
            max_file_size: None,
            symlinks: SymlinkPolicy::default(),
            force_text: Vec::new(),
            force_binary: Vec::new(),
            show_excluded_in_tree: false,
            enumerate_pruned: false,
            allow_secrets: false,
            trusted_remote_config: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformSettings {
    pub remove_blank_lines: bool,
    pub trim_trailing_whitespace: bool,
    pub normalize_line_endings: Option<LineEnding>,
    pub compression: CompressionRequest,
}

impl TransformSettings {
    pub fn any_active(&self) -> bool {
        self.remove_blank_lines
            || self.trim_trailing_whitespace
            || self.normalize_line_endings.is_some()
            || self.compression.is_enabled()
    }

    /// Human-readable labels for the transformations in force, for the document preface.
    pub fn labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        if self.remove_blank_lines {
            labels.push("blank lines removed".to_string());
        }
        if self.trim_trailing_whitespace {
            labels.push("trailing whitespace trimmed".to_string());
        }
        if let Some(ending) = self.normalize_line_endings {
            labels.push(format!("line endings normalised to {}", ending.label()));
        }
        match &self.compression {
            CompressionRequest::Disabled => {}
            CompressionRequest::AllSupported => {
                labels.push("structural signatures only (all supported languages)".to_string())
            }
            CompressionRequest::Languages(langs) => {
                labels.push(format!("structural signatures only ({})", langs.join(", ")))
            }
        }
        labels
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionSettings {
    pub include_preface: bool,
    pub include_tree: bool,
    pub tree_style: TreeStyle,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
}

impl Default for CompositionSettings {
    fn default() -> Self {
        Self {
            include_preface: true,
            include_tree: true,
            tree_style: TreeStyle::default(),
            header_text: None,
            footer_text: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportingSettings {
    pub verbosity: Verbosity,
    pub format: ReportFormat,
    pub show_duration: bool,
    /// Interactive progress indication; suppressed automatically without a terminal.
    pub progress: bool,
    /// How many of the heaviest files to list, or none at all.
    pub rank_files: usize,
    /// Recount tokens from the written document and compare the two counting paths.
    pub verify_token_count: bool,
    /// Whether a count was asked for at all, so that a request for zero is a mistake
    /// rather than a silent no-op.
    pub rank_files_requested: bool,
    /// Whether the report is written somewhere other than the document's stream.
    /// Always true today; kept explicit so that changing where reports go cannot
    /// silently reintroduce a report interleaved with a document on standard output.
    pub report_stream_is_separate: bool,
}

impl Default for ReportingSettings {
    fn default() -> Self {
        Self {
            verbosity: Verbosity::default(),
            format: ReportFormat::default(),
            show_duration: true,
            progress: true,
            rank_files: 0,
            verify_token_count: false,
            rank_files_requested: false,
            report_stream_is_separate: true,
        }
    }
}

/// The complete set of settings governing one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub sources: Vec<SourceSpec>,
    pub format: OutputFormat,
    pub destination: Destination,
    pub dry_run: bool,
    pub overwrite: bool,
    pub keep_remote_copy: bool,
    pub filters: FilterSettings,
    pub transforms: TransformSettings,
    pub composition: CompositionSettings,
    pub reporting: ReportingSettings,
    pub tokenization: TokenEncoding,
    pub failure_policy: FailurePolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sources: vec![SourceSpec::Local(PathBuf::from("."))],
            format: OutputFormat::default(),
            destination: Destination::File(PathBuf::from("./mhrn-output.md")),
            dry_run: false,
            overwrite: false,
            keep_remote_copy: false,
            filters: FilterSettings::default(),
            transforms: TransformSettings::default(),
            composition: CompositionSettings::default(),
            reporting: ReportingSettings::default(),
            tokenization: TokenEncoding::default(),
            failure_policy: FailurePolicy::default(),
        }
    }
}

impl Settings {
    /// Absolute path the document will be written to, where it is written to a file.
    ///
    /// Resolved because the document has to be recognised during traversal: a run that
    /// packages its own previous output doubles the document and dominates every
    /// per-file statistic in it.
    pub fn destination_path(&self) -> Option<std::path::PathBuf> {
        let Destination::File(path) = &self.destination else {
            return None;
        };
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        Some(std::fs::canonicalize(&absolute).unwrap_or(absolute))
    }

    pub fn validate(&self) -> Result<()> {
        if self.sources.is_empty() {
            return Err(Error::config("at least one source is required"));
        }
        // A document delivered to standard output must be the only thing on that stream.
        // The console summary goes to stderr and so is always safe; the JSON report is
        // now sent to stderr as well, so no combination is refused here — but the check
        // below keeps the invariant enforced rather than merely intended.
        if matches!(self.destination, Destination::Stdout)
            && self.reporting.format == ReportFormat::Json
            && !self.reporting.report_stream_is_separate
        {
            return Err(Error::config(
                "--report json cannot share standard output with the document; \
                 write the document to a file with --output, or use --report console",
            ));
        }

        if self.reporting.verify_token_count && !matches!(self.destination, Destination::File(_)) {
            return Err(Error::config(
                "--verify-tokens recounts the delivered document and so needs one to read \
                 back; write the document to a file with --output",
            ));
        }

        if self.reporting.rank_files == 0 && self.reporting.rank_files_requested {
            return Err(Error::config(
                "--top-files needs a positive count; omit it entirely to rank nothing",
            ));
        }

        for (label, text) in [
            ("--header", self.composition.header_text.as_deref()),
            ("--footer", self.composition.footer_text.as_deref()),
        ] {
            if let Some(text) = text {
                if crate::engine::classify::contains_disallowed_control(text) {
                    return Err(Error::config(format!(
                        "{label} contains control characters that cannot be represented in \
                         every output format; remove them"
                    )));
                }
            }
        }

        Ok(())
    }

    pub fn has_remote_source(&self) -> bool {
        self.sources
            .iter()
            .any(|s| matches!(s, SourceSpec::Remote(_)))
    }

    pub fn trusts_remote_config(&self, designation: &str) -> bool {
        self.filters
            .trusted_remote_config
            .iter()
            .any(|d| d == designation)
    }
}
