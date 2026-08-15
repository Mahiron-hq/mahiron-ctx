use std::io::{self, Write};

use crate::config::Verbosity;

use super::{
    grouped, human_bytes, DeliveryReport, RemoteOutcome, RunReport, Severity, AGGREGATION_THRESHOLD,
};

/// Indentation of a labelled line, and of anything subordinate to one.
const INDENT: &str = "  ";
const SUB_INDENT: &str = "      ";

/// Column the values start at, measured from the start of the label.
const LABEL_WIDTH: usize = 30;
const SUB_LABEL_WIDTH: usize = LABEL_WIDTH + INDENT.len() - SUB_INDENT.len();

/// How many individual entries an exclusion reason lists before it is summarised.
const EXCLUSION_SAMPLE: usize = 6;

/// Everything here is ASCII on purpose: a console running a legacy code page turns
/// anything else into replacement characters, and a summary is no use unreadable.
pub fn render_console(
    out: &mut dyn Write,
    report: &RunReport,
    verbosity: Verbosity,
) -> io::Result<()> {
    if verbosity == Verbosity::Quiet {
        return render_quiet(out, report);
    }

    writeln!(out)?;

    if let Some(remote) = &report.remote {
        render_remote(out, remote)?;
    } else {
        field(out, "Source", &report.source_label)?;
    }

    field(out, "Files discovered", &grouped(report.discovered as u64))?;
    field(out, "Files included", &grouped(report.included as u64))?;
    field(out, "Files excluded", &grouped(report.excluded as u64))?;
    if report.directories_pruned > 0 {
        // Counted apart from the files: a pruned directory is not one of the files that
        // were discovered, and folding the two together made the three counts above
        // fail to reconcile on any project with a build directory in it.
        field(
            out,
            "Directories pruned",
            &grouped(report.directories_pruned as u64),
        )?;
    }
    render_exclusions(out, report, verbosity)?;

    writeln!(out)?;
    field(out, "Output format", report.format.label())?;
    field(out, "Output size", &human_bytes(report.output.bytes))?;
    field(out, "Output lines", &grouped(report.output.lines))?;
    field(
        out,
        "Tokens",
        &format!(
            "{} ({})",
            grouped(report.output.tokens as u64),
            report.output.token_encoding
        ),
    )?;

    match &report.delivery {
        DeliveryReport::File { path } => field(out, "Written to", path)?,
        DeliveryReport::Stdout => field(out, "Written to", "standard output")?,
        DeliveryReport::Clipboard => field(out, "Copied to", "system clipboard")?,
        DeliveryReport::Retained => field(out, "Returned to", "the caller, in memory")?,
        DeliveryReport::DryRun => field(out, "Preview only", "nothing written")?,
    }

    if report.line_endings.len() > 1 {
        let mix = report
            .line_endings
            .iter()
            .map(|(ending, count)| format!("{} {}", grouped(*count as u64), ending.label()))
            .collect::<Vec<_>>()
            .join(", ");
        field(out, "Line endings", &mix)?;
    }

    if !report.transformations.is_empty() {
        field(out, "Transformations", &report.transformations.join(", "))?;
    }

    render_heaviest(out, report)?;
    render_warnings(out, report, verbosity)?;
    render_notices(out, report, verbosity)?;

    if report.reports_duration() {
        writeln!(out)?;
        writeln!(
            out,
            "{INDENT}Completed in {:.1}s",
            report.duration.as_secs_f64()
        )?;
    }

    Ok(())
}

fn render_quiet(out: &mut dyn Write, report: &RunReport) -> io::Result<()> {
    // Quiet mode still has to answer the two questions a script cannot infer from the
    // exit status alone: where the document went, and whether anything was flagged.
    if let DeliveryReport::File { path } = &report.delivery {
        writeln!(out, "{path}")?;
    }
    let actionable = report.actionable_warnings().count();
    if actionable > 0 {
        writeln!(out, "{actionable} warning(s)")?;
    }
    Ok(())
}

/// Break each exclusion reason down into what it actually excluded.
///
/// A count on its own leaves the user guessing which rule cost them which file; naming
/// the entries is what makes an unexpected exclusion diagnosable without a second run.
fn render_exclusions(
    out: &mut dyn Write,
    report: &RunReport,
    verbosity: Verbosity,
) -> io::Result<()> {
    for (reason, count) in &report.exclusions {
        if *count == 0 {
            continue;
        }
        subfield(out, reason.label(), &grouped(*count as u64))?;

        let entries: Vec<&super::FileRecord> = report
            .records
            .iter()
            .filter(|record| record.excluded.as_ref() == Some(reason))
            .collect();

        let limit = if verbosity.at_least(Verbosity::Verbose) {
            entries.len()
        } else {
            EXCLUSION_SAMPLE
        };

        for record in entries.iter().take(limit) {
            let kind = if record.path.ends_with('/') {
                "dir "
            } else {
                "file"
            };
            match &record.attribution {
                // A rule that merely repeats the name it matched tells the reader
                // nothing they cannot already see in the path.
                Some(rule) if worth_showing(rule, &record.path) => {
                    writeln!(out, "{SUB_INDENT}  {kind} {}  [{rule}]", record.path)?
                }
                _ => writeln!(out, "{SUB_INDENT}  {kind} {}", record.path)?,
            }
        }

        let remainder = entries.len().saturating_sub(limit);
        if remainder > 0 {
            writeln!(
                out,
                "{SUB_INDENT}  ... and {} more, run with --verbose to list them",
                grouped(remainder as u64)
            )?;
        }
    }
    Ok(())
}

/// Whether an attribution adds anything to the path it is printed beside.
fn worth_showing(rule: &str, path: &str) -> bool {
    if rule == "built-in exclusion" {
        return false;
    }
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);
    rule != name
}

fn render_heaviest(out: &mut dyn Write, report: &RunReport) -> io::Result<()> {
    let heaviest = report.heaviest_files(report.rank_files);
    if heaviest.is_empty() {
        return Ok(());
    }

    writeln!(out)?;
    writeln!(out, "{INDENT}Heaviest files")?;

    let widest = heaviest
        .iter()
        .map(|(_, tokens)| grouped(*tokens as u64).len())
        .max()
        .unwrap_or(0);
    let total = report.output.tokens.max(1);

    for (path, tokens) in heaviest {
        let share = tokens * 100 / total;
        writeln!(
            out,
            "{SUB_INDENT}{:>width$} tokens  {share:>2}%  {path}",
            grouped(tokens as u64),
            width = widest
        )?;
    }
    Ok(())
}

fn render_remote(out: &mut dyn Write, remote: &RemoteOutcome) -> io::Result<()> {
    match remote {
        RemoteOutcome::Succeeded {
            designation,
            reference,
            retained_at,
        } => {
            let reference = reference.as_deref().unwrap_or("default branch");
            field(out, "Remote source retrieved", designation)?;
            subfield(out, "reference", reference)?;
            match retained_at {
                Some(path) => subfield(out, "local copy retained at", path)?,
                None => subfield(out, "local copy", "ephemeral, discarded after run")?,
            }
        }
        RemoteOutcome::Failed {
            designation,
            detail,
        } => {
            field(out, "Remote retrieval failed", designation)?;
            subfield(out, "detail", detail)?;
        }
    }
    Ok(())
}

/// Conditions that did not affect the exit status, kept apart from those that did.
///
/// A reader who sees a "Warnings" heading and an exit code of 1 should be able to act on
/// what is under it. Mixed line endings and an unsupported language are neither
/// actionable nor failures, and listing them together devalued both.
fn render_notices(out: &mut dyn Write, report: &RunReport, verbosity: Verbosity) -> io::Result<()> {
    let notices: Vec<&super::WarningRecord> = report.notices().collect();
    if notices.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(out, "{INDENT}Notices ({})", grouped(notices.len() as u64))?;
    for group in report.aggregated_warnings(Severity::Notice) {
        let _ = verbosity;
        writeln!(out, "{SUB_INDENT}{}", group.summary)?;
        if let Some(warning) = notices
            .iter()
            .find(|warning| warning.kind.summary() == group.summary)
        {
            if let Some(detail) = warning.kind.qualifier() {
                writeln!(out, "{SUB_INDENT}  {detail}")?;
            }
        }
    }
    Ok(())
}

fn render_warnings(
    out: &mut dyn Write,
    report: &RunReport,
    verbosity: Verbosity,
) -> io::Result<()> {
    let actionable = report.actionable_warnings().count();
    if actionable == 0 {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(out, "{INDENT}Warnings ({})", grouped(actionable as u64))?;

    for group in report.aggregated_warnings(Severity::Warning) {
        let expand = verbosity.at_least(Verbosity::Verbose) || group.count <= AGGREGATION_THRESHOLD;
        if expand {
            for warning in report.warnings_matching(&group.summary) {
                match &warning.path {
                    Some(path) => {
                        writeln!(out, "{SUB_INDENT}{path}")?;
                        writeln!(out, "{SUB_INDENT}  {}", detail_line(warning))?;
                    }
                    None => writeln!(out, "{SUB_INDENT}{}", detail_line(warning))?,
                }
            }
            continue;
        }

        writeln!(
            out,
            "{SUB_INDENT}{:<width$}{:>8}",
            group.summary,
            grouped(group.count as u64),
            width = LABEL_WIDTH + 8
        )?;
        let languages = if group.languages.is_empty() {
            String::new()
        } else {
            let shown: Vec<_> = group.languages.iter().take(4).cloned().collect();
            let remainder = group.languages.len().saturating_sub(shown.len());
            if remainder > 0 {
                format!("({}, and {remainder} other languages; ", shown.join(", "))
            } else {
                format!("({}; ", shown.join(", "))
            }
        };
        writeln!(
            out,
            "{SUB_INDENT}  {languages}run with --verbose for the full per-file list)"
        )?;
    }
    Ok(())
}

fn detail_line(warning: &super::WarningRecord) -> String {
    match warning.kind.qualifier() {
        Some(detail) => format!("{}: {detail}", warning.kind.summary()),
        None => warning.kind.summary(),
    }
}

/// A labelled line. Values sit in one column so counts read as a column of numbers.
///
/// A label wider than the column keeps its own separation rather than running into the
/// value, which is what turned one reason into `version-control ignore rules2`.
fn field(out: &mut dyn Write, label: &str, value: &str) -> io::Result<()> {
    writeln!(out, "{INDENT}{}{value}", pad(label, LABEL_WIDTH))
}

fn subfield(out: &mut dyn Write, label: &str, value: &str) -> io::Result<()> {
    writeln!(out, "{SUB_INDENT}{}{value}", pad(label, SUB_LABEL_WIDTH))
}

fn pad(label: &str, width: usize) -> String {
    let gap = width.saturating_sub(label.chars().count()).max(2);
    format!("{label}{}", " ".repeat(gap))
}

impl RunReport {
    fn reports_duration(&self) -> bool {
        self.show_duration && !self.duration.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputFormat;
    use crate::report::{ExclusionReason, FileRecord, OutputStats, RunStatus};
    use std::collections::BTreeMap;

    fn record(path: &str, reason: Option<ExclusionReason>, tokens: Option<usize>) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            size: 10,
            excluded: reason,
            attribution: None,
            encoding: None,
            compressed: false,
            tokens,
        }
    }

    fn report() -> RunReport {
        let mut exclusions = BTreeMap::new();
        exclusions.insert(ExclusionReason::DefaultExclusion, 2);

        RunReport {
            source_label: "./project".to_string(),
            format: OutputFormat::Markdown,
            discovered: 5,
            included: 3,
            excluded: 2,
            exclusions,
            directories_pruned: 1,
            records: vec![
                record("src/main.rs", None, Some(1200)),
                record("README.md", None, Some(300)),
                record("target/", Some(ExclusionReason::DefaultExclusion), None),
                record(
                    "node_modules/pkg/a.js",
                    Some(ExclusionReason::DefaultExclusion),
                    None,
                ),
            ],
            warnings: Vec::new(),
            output: OutputStats {
                bytes: 2048,
                lines: 90,
                tokens: 1500,
                token_encoding: "cl100k_base".to_string(),
            },
            delivery: DeliveryReport::File {
                path: "out.md".to_string(),
            },
            remote: None,
            line_endings: BTreeMap::new(),
            duration: std::time::Duration::ZERO,
            show_duration: false,
            rank_files: 5,
            transformations: Vec::new(),
            dry_run: false,
        }
    }

    fn rendered(verbosity: Verbosity) -> String {
        let mut buffer: Vec<u8> = Vec::new();
        render_console(&mut buffer, &report(), verbosity).unwrap();
        String::from_utf8(buffer).unwrap()
    }

    #[test]
    fn the_summary_stays_within_ascii() {
        assert!(rendered(Verbosity::Normal).is_ascii());
    }

    #[test]
    fn an_overlong_label_still_separates_from_its_value() {
        let mut long = report();
        long.exclusions.clear();
        long.exclusions.insert(ExclusionReason::IgnoreRules, 2);
        let mut buffer: Vec<u8> = Vec::new();
        render_console(&mut buffer, &long, Verbosity::Normal).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        let line = output
            .lines()
            .find(|line| line.contains(ExclusionReason::IgnoreRules.label()))
            .expect("the reason was not rendered");
        assert!(
            line.ends_with("  2"),
            "label and value ran together: {line:?}"
        );
    }

    #[test]
    fn an_attribution_that_repeats_the_name_is_left_out() {
        let mut report = report();
        report.records[2].attribution = Some("target".to_string());
        let mut buffer: Vec<u8> = Vec::new();
        render_console(&mut buffer, &report, Verbosity::Normal).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("dir  target/"), "{output}");
        assert!(!output.contains("[target]"), "{output}");
    }

    #[test]
    fn exclusions_name_what_was_excluded_and_say_which_kind() {
        let output = rendered(Verbosity::Normal);
        assert!(output.contains("dir  target/"), "{output}");
        assert!(output.contains("file node_modules/pkg/a.js"), "{output}");
    }

    #[test]
    fn values_line_up_in_one_column() {
        let output = rendered(Verbosity::Normal);
        let columns: Vec<usize> = output
            .lines()
            .filter(|line| line.starts_with("  Files "))
            .map(|line| line.find(char::is_numeric).unwrap_or(0))
            .collect();
        assert_eq!(columns.len(), 3);
        assert!(
            columns.windows(2).all(|pair| pair[0] == pair[1]),
            "counts are not aligned: {output}"
        );
    }

    #[test]
    fn the_heaviest_files_are_listed_when_they_were_counted() {
        let output = rendered(Verbosity::Normal);
        assert!(output.contains("Heaviest files"));
        let main = output.find("src/main.rs").unwrap();
        let readme = output.find("README.md").unwrap();
        assert!(main < readme, "not ordered by cost: {output}");
        assert!(output.contains("80%"), "no share shown: {output}");
    }

    #[test]
    fn a_run_that_did_not_rank_files_says_nothing_about_them() {
        let mut without = report();
        for record in &mut without.records {
            record.tokens = None;
        }
        let mut buffer: Vec<u8> = Vec::new();
        render_console(&mut buffer, &without, Verbosity::Normal).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert!(!output.contains("Heaviest files"));
    }

    #[test]
    fn quiet_mode_reports_only_the_destination() {
        let output = {
            let mut buffer: Vec<u8> = Vec::new();
            render_console(&mut buffer, &report(), Verbosity::Quiet).unwrap();
            String::from_utf8(buffer).unwrap()
        };
        assert_eq!(output, "out.md\n");
        assert_eq!(report().status(), RunStatus::Success);
    }
}
