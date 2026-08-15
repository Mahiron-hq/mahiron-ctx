use std::path::PathBuf;

use clap::Parser;

use crate::config::{
    CompositionSettings, CompressionRequest, Destination, FailurePolicy, FilterSettings,
    LineEnding, OutputFormat, ReportFormat, ReportingSettings, Settings, SourceSpec, SymlinkPolicy,
    TokenEncoding, TransformSettings, TreeStyle, Verbosity, DEFAULT_OUTPUT_STEM,
    DEFAULT_OUTPUT_SUFFIX,
};
use crate::error::{Error, Result};
use crate::remote;

const ABOUT: &str = "Consolidate a codebase into a single, faithful document.";

const AFTER_HELP: &str = "\
RULE PRECEDENCE
  Rules are resolved in this order, highest first:
    1. --include / --exclude given at invocation
    2. the tool's own ignore file (.mahironignore, .mhrnignore)
    3. version-control ignore rules (.gitignore, .ignore, core.excludesFile)
    4. built-in defaults (hidden entries, generated directories, lock files,
     and files whose names identify them as credentials)
  Within step 1, the more specific pattern wins; where two patterns are equally
  specific, the exclusion wins. Deeper ignore files override shallower ones.
  Step 1 governs directories as well as files: --include '.github/**' descends
  into a directory the defaults would otherwise prune, and --exclude 'x/**'
  prunes at the boundary rather than once per file inside.

EXIT STATUS
  0  the run completed with no warnings
  1  the run completed and reported one or more warnings
  2  the run failed
  Notices - mixed line endings, a language this build cannot reduce, credentials
  left out - are reported but do not affect the status: they are not failures and
  there is nothing to act on.

NETWORK ACTIVITY
  None, unless a remote source is designated or --mcp-server --transport sse is
  selected. No telemetry, crash reporting or update checks exist in any build.";

/// Command-line surface. Changes to any flag, default or exit status here are governed
/// by the crate's own semantic version.
#[derive(Debug, Parser)]
#[command(
    name = "mahiron-ctx",
    // No `bin_name`: it is taken from argv[0], so `mahiron-ctx --help` no longer prints
    // usage under the name of the other binary.
    version,
    about = ABOUT,
    after_help = AFTER_HELP,
    max_term_width = 96
)]
pub struct Cli {
    /// Files or directories to package; a single remote repository URL is also accepted
    #[arg(value_name = "SOURCE")]
    pub sources: Vec<String>,

    /// Write the document to this path
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Write the document to standard output instead of a file
    #[arg(long, conflicts_with_all = ["output", "clipboard"])]
    pub stdout: bool,

    /// Copy the document to the system clipboard instead of writing a file
    #[arg(long, conflicts_with = "output")]
    pub clipboard: bool,

    /// Output format: markdown, text, xml or json
    #[arg(short, long, value_name = "FORMAT")]
    pub format: Option<String>,

    /// Add an inclusion pattern; may be repeated
    #[arg(long, value_name = "PATTERN")]
    pub include: Vec<String>,

    /// Add an exclusion pattern; may be repeated
    #[arg(long, value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Ignore version-control ignore rules for this run
    #[arg(long)]
    pub no_gitignore: bool,

    /// Ignore the tool's own ignore file for this run
    #[arg(long)]
    pub no_tool_ignore: bool,

    /// Do not apply the built-in exclusions for well-known generated directories
    #[arg(long)]
    pub no_default_excludes: bool,

    /// Include hidden files and directories
    #[arg(long)]
    pub hidden: bool,

    /// Package files whose names identify them as credentials, which are left out by default
    #[arg(long)]
    pub allow_secrets: bool,

    /// Exclude files larger than this size, e.g. 500K or 2MB
    #[arg(long, value_name = "SIZE")]
    pub max_size: Option<String>,

    /// Match patterns without regard to case
    #[arg(long, conflicts_with = "case_sensitive")]
    pub case_insensitive: bool,

    /// Match patterns with regard to case
    #[arg(long)]
    pub case_sensitive: bool,

    /// Symbolic-link policy: never, within-sources (files only) or always
    #[arg(long, value_name = "POLICY")]
    pub symlinks: Option<String>,

    /// Treat files matching this pattern as text regardless of their content
    #[arg(long, value_name = "PATTERN")]
    pub force_text: Vec<String>,

    /// Treat files matching this pattern as binary regardless of their content
    #[arg(long, value_name = "PATTERN")]
    pub force_binary: Vec<String>,

    /// Omit the structural overview
    #[arg(long)]
    pub no_tree: bool,

    /// Structural overview rendering: ascii or compact
    #[arg(long, value_name = "STYLE")]
    pub tree_style: Option<String>,

    /// List excluded files in the structural overview without including their content
    #[arg(long)]
    pub show_excluded_in_tree: bool,

    /// Enumerate the contents of pruned directories so every file is attributable
    #[arg(long)]
    pub enumerate_pruned: bool,

    /// Omit the generated preface
    #[arg(long)]
    pub no_preface: bool,

    /// Text placed before the packaged content
    #[arg(long, value_name = "TEXT", conflicts_with = "header_file")]
    pub header: Option<String>,

    /// File whose contents are placed before the packaged content
    #[arg(long, value_name = "PATH")]
    pub header_file: Option<PathBuf>,

    /// Text placed after the packaged content
    #[arg(long, value_name = "TEXT", conflicts_with = "footer_file")]
    pub footer: Option<String>,

    /// File whose contents are placed after the packaged content
    #[arg(long, value_name = "PATH")]
    pub footer_file: Option<PathBuf>,

    /// Remove blank lines from packaged content
    #[arg(long)]
    pub remove_blank_lines: bool,

    /// Trim trailing whitespace from packaged content
    #[arg(long)]
    pub trim_trailing_whitespace: bool,

    /// Normalise line endings in packaged content: lf, crlf or cr
    #[arg(long, value_name = "ENDING")]
    pub line_endings: Option<String>,

    /// Reduce supported languages to structural signatures; names languages, or all when bare
    ///
    /// Written as `--compress` or `--compress=rust,python`. The value is attached rather
    /// than separate so that `mhrn --compress .` packages the current directory instead of
    /// reading `.` as a language name.
    #[arg(
        long,
        value_name = "LANGS",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        value_delimiter = ','
    )]
    pub compress: Option<Vec<String>>,

    /// List the languages this build can reduce to structural signatures
    #[arg(long)]
    pub list_compression_languages: bool,

    /// Reference encoding for the token count: cl100k_base or o200k_base
    #[arg(long, value_name = "ENCODING")]
    pub token_encoding: Option<String>,

    /// List the heaviest files by token count; five unless a number is given
    ///
    /// Written as `--top-files` or `--top-files=10`, so that a following positional
    /// argument cannot be swallowed as the count.
    #[arg(
        long,
        value_name = "N",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "5"
    )]
    pub top_files: Option<usize>,

    /// Recount tokens from the delivered document and compare against the streamed count
    #[arg(long)]
    pub verify_tokens: bool,

    /// Report what would be produced without writing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Overwrite the destination, and confirm any other prompt, without asking
    #[arg(long)]
    pub force: bool,

    /// Abort the run on the first problem instead of recording it and continuing
    #[arg(long)]
    pub strict: bool,

    /// Increase diagnostic detail, including per-file outcomes behind aggregated counts
    #[arg(short, long, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress everything but the essentials
    #[arg(short, long)]
    pub quiet: bool,

    /// Summary format: console or json
    #[arg(long, value_name = "FORMAT")]
    pub report: Option<String>,

    /// Never show progress indication, even on a terminal
    #[arg(long)]
    pub no_progress: bool,

    /// Branch, tag or commit identifying which snapshot of a remote source to retrieve
    #[arg(long, value_name = "REF")]
    pub remote_ref: Option<String>,

    /// Keep the retrieved copy of a remote source instead of discarding it
    #[arg(long)]
    pub keep_remote_copy: bool,

    /// Apply the named remote source's own ignore file to this run
    #[arg(long, value_name = "SOURCE")]
    pub trust_remote_config: Vec<String>,

    /// Re-run automatically whenever a source changes, until stopped
    #[arg(long, conflicts_with = "mcp_server")]
    pub watch: bool,

    /// Serve the engine over the Model Context Protocol instead of performing one run
    #[arg(long)]
    pub mcp_server: bool,

    /// MCP transport: stdio or sse
    #[arg(long, value_name = "TRANSPORT", requires = "mcp_server")]
    pub transport: Option<String>,

    /// Address the SSE transport listens on; loopback unless told otherwise
    #[arg(long, value_name = "ADDR", requires = "mcp_server")]
    pub bind: Option<String>,
}

impl Cli {
    /// Translate arguments into the settings the engine actually acts on.
    pub fn to_settings(&self) -> Result<Settings> {
        let sources = self.resolve_sources()?;
        let format = self.resolve_format()?;
        let destination = self.resolve_destination(format, &sources);

        let filters = FilterSettings {
            include: self.include.clone(),
            exclude: self.exclude.clone(),
            case_insensitive: self.resolve_case_sensitivity(),
            use_vcs_ignore: !self.no_gitignore,
            use_tool_ignore: !self.no_tool_ignore,
            use_default_exclusions: !self.no_default_excludes,
            include_hidden: self.hidden,
            max_file_size: self.max_size.as_deref().map(parse_size).transpose()?,
            symlinks: match &self.symlinks {
                Some(policy) => SymlinkPolicy::parse(policy)?,
                None => SymlinkPolicy::default(),
            },
            force_text: self.force_text.clone(),
            force_binary: self.force_binary.clone(),
            show_excluded_in_tree: self.show_excluded_in_tree,
            enumerate_pruned: self.enumerate_pruned,
            allow_secrets: self.allow_secrets,
            trusted_remote_config: self.trust_remote_config.clone(),
        };

        let transforms = TransformSettings {
            remove_blank_lines: self.remove_blank_lines,
            trim_trailing_whitespace: self.trim_trailing_whitespace,
            normalize_line_endings: self
                .line_endings
                .as_deref()
                .map(LineEnding::parse)
                .transpose()?,
            compression: match &self.compress {
                None => CompressionRequest::Disabled,
                Some(languages) => {
                    // A bare `--compress` arrives as one empty value; naming no language
                    // means every language this build supports.
                    let named: Vec<String> = languages
                        .iter()
                        .map(|language| language.trim().to_string())
                        .filter(|language| !language.is_empty())
                        .collect();
                    if named.is_empty() {
                        CompressionRequest::AllSupported
                    } else {
                        CompressionRequest::Languages(named)
                    }
                }
            },
        };

        let composition = CompositionSettings {
            include_preface: !self.no_preface,
            include_tree: !self.no_tree,
            tree_style: match &self.tree_style {
                Some(style) => TreeStyle::parse(style)?,
                None => TreeStyle::default(),
            },
            header_text: self.read_framing(self.header.clone(), self.header_file.as_deref())?,
            footer_text: self.read_framing(self.footer.clone(), self.footer_file.as_deref())?,
        };

        let verbosity = if self.quiet {
            Verbosity::Quiet
        } else if self.verbose {
            Verbosity::Verbose
        } else {
            Verbosity::Normal
        };

        let reporting = ReportingSettings {
            verbosity,
            format: match self.report.as_deref() {
                Some("json") => ReportFormat::Json,
                Some("console") | None => ReportFormat::Console,
                Some(other) => {
                    return Err(Error::config(format!(
                        "unknown report format `{other}`; expected console or json"
                    )))
                }
            },
            show_duration: true,
            progress: !self.no_progress && !self.quiet,
            rank_files: self.top_files.unwrap_or(0),
            verify_token_count: self.verify_tokens,
            // `--top-files 0` is a mistake rather than a request for nothing, and only
            // recording the number would make it indistinguishable from not asking.
            rank_files_requested: self.top_files.is_some(),
            report_stream_is_separate: true,
        };

        Ok(Settings {
            sources,
            format,
            destination,
            dry_run: self.dry_run,
            overwrite: self.force,
            keep_remote_copy: self.keep_remote_copy,
            filters,
            transforms,
            composition,
            reporting,
            tokenization: match &self.token_encoding {
                Some(encoding) => TokenEncoding::parse(encoding)?,
                None => TokenEncoding::default(),
            },
            failure_policy: if self.strict {
                FailurePolicy::Strict
            } else {
                FailurePolicy::Continue
            },
        })
    }

    fn resolve_sources(&self) -> Result<Vec<SourceSpec>> {
        if self.sources.is_empty() {
            return Ok(vec![SourceSpec::Local(PathBuf::from("."))]);
        }

        let mut remote_count = 0;
        let mut sources = Vec::with_capacity(self.sources.len());
        for argument in &self.sources {
            match remote::recognise(argument, self.remote_ref.as_deref()) {
                Some(source) => {
                    remote_count += 1;
                    sources.push(SourceSpec::Remote(source));
                }
                None => sources.push(SourceSpec::Local(PathBuf::from(argument))),
            }
        }

        if remote_count > 1 {
            return Err(Error::config(
                "only one remote source may be designated per run",
            ));
        }
        Ok(sources)
    }

    fn resolve_format(&self) -> Result<OutputFormat> {
        let inferred = self
            .output
            .as_deref()
            .and_then(OutputFormat::from_destination);

        let Some(requested) = &self.format else {
            return Ok(inferred.unwrap_or_default());
        };
        let requested = OutputFormat::parse(requested)?;

        // A file named `.xml` holding markdown is a trap for whatever reads it next, and
        // the tool cannot tell which of the two the user meant.
        if let Some(inferred) = inferred {
            if inferred != requested {
                return Err(Error::config(format!(
                    "--format {} conflicts with the `.{}` extension of the output path, which \
                     means {}; rename the output, or drop --format and let the extension decide",
                    requested.label(),
                    self.output
                        .as_deref()
                        .and_then(|p| p.extension())
                        .map(|e| e.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    inferred.label(),
                )));
            }
        }
        Ok(requested)
    }

    fn resolve_destination(&self, format: OutputFormat, sources: &[SourceSpec]) -> Destination {
        if self.stdout {
            return Destination::Stdout;
        }
        if self.clipboard {
            return Destination::Clipboard;
        }
        match &self.output {
            Some(path) => Destination::File(path.clone()),
            None => Destination::File(PathBuf::from(format!(
                "./{}{DEFAULT_OUTPUT_SUFFIX}.{}",
                default_stem(sources),
                format.extension()
            ))),
        }
    }

    fn resolve_case_sensitivity(&self) -> bool {
        if self.case_insensitive {
            return true;
        }
        if self.case_sensitive {
            return false;
        }
        FilterSettings::default().case_insensitive
    }

    fn read_framing(
        &self,
        inline: Option<String>,
        path: Option<&std::path::Path>,
    ) -> Result<Option<String>> {
        match (inline, path) {
            (Some(text), _) => Ok(Some(text)),
            (None, Some(path)) => std::fs::read_to_string(path)
                .map(Some)
                .map_err(|e| Error::io(path, e)),
            (None, None) => Ok(None),
        }
    }
}

/// Name the document after what it holds: the directory or the repository packaged.
///
/// A directory full of documents all called the same thing helps nobody, and the name is
/// the first thing a reader sees once the document is passed on somewhere else.
fn default_stem(sources: &[SourceSpec]) -> String {
    let name = match sources.first() {
        Some(SourceSpec::Remote(source)) => repository_name(&source.url),
        Some(SourceSpec::Local(path)) => local_name(path),
        None => None,
    };
    name.unwrap_or_else(|| DEFAULT_OUTPUT_STEM.to_string())
}

/// `.` and `..` are path syntax rather than names, so the directory has to be resolved
/// before asking what it is called.
fn local_name(path: &std::path::Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let name = resolved.file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

fn repository_name(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let name = trimmed.rsplit(['/', ':']).next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Parse a size written the way people write sizes.
pub fn parse_size(value: &str) -> Result<u64> {
    let trimmed = value.trim();
    let digits: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return Err(Error::config(format!("could not read `{value}` as a size")));
    }
    let magnitude: f64 = digits
        .parse()
        .map_err(|_| Error::config(format!("could not read `{value}` as a size")))?;

    let unit = trimmed[digits.len()..].trim().to_ascii_lowercase();
    let multiplier: u64 = match unit.as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        other => {
            return Err(Error::config(format!(
                "unknown size unit `{other}` in `{value}`"
            )))
        }
    };
    Ok((magnitude * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn argument_definitions_are_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn sizes_accept_the_usual_spellings() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("2K").unwrap(), 2048);
        assert_eq!(parse_size("1.5MB").unwrap(), 1_572_864);
        assert!(parse_size("many").is_err());
        assert!(parse_size("10QB").is_err());
    }

    fn parse(arguments: &[&str]) -> Settings {
        let mut full = vec!["mhrn"];
        full.extend_from_slice(arguments);
        Cli::parse_from(full).to_settings().unwrap()
    }

    #[test]
    fn a_bare_invocation_packages_the_current_directory_into_a_named_file() {
        let settings = parse(&[]);
        assert_eq!(
            settings.sources,
            vec![SourceSpec::Local(PathBuf::from("."))]
        );
        assert_eq!(settings.format, OutputFormat::Markdown);

        let expected = format!(
            "./{}-mhrn.md",
            std::env::current_dir()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        );
        assert_eq!(
            settings.destination,
            Destination::File(PathBuf::from(expected))
        );
    }

    #[test]
    fn a_remote_source_names_the_document_after_the_repository() {
        assert_eq!(
            repository_name("https://github.com/owner/spring-framework.git"),
            Some("spring-framework".to_string())
        );
        assert_eq!(
            repository_name("git@github.com:owner/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            repository_name("https://example.com/owner/repo/"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn format_is_inferred_from_the_destination_extension() {
        assert_eq!(parse(&["-o", "out.xml"]).format, OutputFormat::Xml);
        assert_eq!(parse(&["-o", "out.json"]).format, OutputFormat::Json);
        assert_eq!(parse(&["-o", "out.unknown"]).format, OutputFormat::Markdown);
        assert_eq!(
            parse(&["-o", "out.json", "-f", "json"]).format,
            OutputFormat::Json
        );
    }

    #[test]
    fn a_format_contradicting_the_extension_is_refused() {
        let cli = Cli::parse_from(["mhrn", "-o", "out.xml", "-f", "markdown"]);
        let message = cli.to_settings().unwrap_err().to_string();
        assert!(
            message.contains("conflicts"),
            "unhelpful message: {message}"
        );

        // An extension the tool has no opinion about contradicts nothing.
        assert_eq!(
            parse(&["-o", "out.context", "-f", "xml"]).format,
            OutputFormat::Xml
        );
    }

    #[test]
    fn compression_is_off_unless_asked_for() {
        assert_eq!(
            parse(&[]).transforms.compression,
            CompressionRequest::Disabled
        );
        assert_eq!(
            parse(&["--compress"]).transforms.compression,
            CompressionRequest::AllSupported
        );
        assert_eq!(
            parse(&["--compress=rust"]).transforms.compression,
            CompressionRequest::Languages(vec!["rust".into()])
        );
        assert_eq!(
            parse(&["--compress=rust,python"]).transforms.compression,
            CompressionRequest::Languages(vec!["rust".into(), "python".into()])
        );
    }

    #[test]
    fn a_bare_compress_flag_does_not_swallow_the_source() {
        // `--compress .` used to read `.` as a language name and fail with
        // "no structural-signature support for `.`" rather than packaging anything.
        let settings = parse(&["--compress", "."]);
        assert_eq!(
            settings.transforms.compression,
            CompressionRequest::AllSupported
        );
        assert_eq!(
            settings.sources,
            vec![SourceSpec::Local(PathBuf::from("."))]
        );
    }

    #[test]
    fn a_bare_top_files_flag_does_not_swallow_the_source() {
        let settings = parse(&["--top-files", "src"]);
        assert_eq!(settings.reporting.rank_files, 5);
        assert_eq!(
            settings.sources,
            vec![SourceSpec::Local(PathBuf::from("src"))]
        );
    }

    #[test]
    fn asking_for_zero_of_the_heaviest_files_is_refused() {
        let cli = Cli::parse_from(["mhrn", "--top-files=0"]);
        let settings = cli.to_settings().unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn an_address_without_a_server_is_refused_rather_than_ignored() {
        // `--bind 0.0.0.0:80` on an ordinary run was accepted and silently did nothing.
        assert!(Cli::try_parse_from(["mhrn", "--bind", "0.0.0.0:80"]).is_err());
        assert!(Cli::try_parse_from(["mhrn", "--mcp-server", "--bind", "0.0.0.0:80"]).is_ok());
    }

    #[test]
    fn transformations_stay_off_by_default() {
        let settings = parse(&[]);
        assert!(!settings.transforms.any_active());
        assert!(settings.transforms.labels().is_empty());
    }

    #[test]
    fn an_argument_resembling_a_url_without_a_scheme_stays_local() {
        let settings = parse(&["example.com/owner/repo"]);
        assert_eq!(
            settings.sources,
            vec![SourceSpec::Local(PathBuf::from("example.com/owner/repo"))]
        );
    }

    #[test]
    fn only_one_remote_source_is_accepted() {
        let cli = Cli::parse_from([
            "mhrn",
            "https://example.com/a.git",
            "https://example.com/b.git",
        ]);
        assert!(cli.to_settings().is_err());
    }
}
