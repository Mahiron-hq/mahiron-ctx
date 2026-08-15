//! Round-trip tests over the adversarial corpus.
//!
//! Content that goes in must come back out unchanged, in every format, for every case
//! the corpus knows how to break.

mod support;

use std::path::{Path, PathBuf};

use mahiron_ctx::config::{Destination, OutputFormat, Settings, SourceSpec};
use mahiron_ctx::engine::Engine;
use mahiron_ctx::report::ExclusionReason;

use support::extract::{delimiters_require_final_newline, extract};

const FORMATS: [OutputFormat; 4] = [
    OutputFormat::Markdown,
    OutputFormat::Text,
    OutputFormat::Xml,
    OutputFormat::Json,
];

fn settings_for(root: &Path, format: OutputFormat, destination: &Path) -> Settings {
    let mut settings = Settings {
        sources: vec![SourceSpec::Local(root.to_path_buf())],
        format,
        destination: Destination::File(destination.to_path_buf()),
        overwrite: true,
        ..Default::default()
    };
    settings.filters.include_hidden = true;
    settings.filters.use_vcs_ignore = false;
    settings.reporting.progress = false;
    settings
}

fn package(root: &Path, format: OutputFormat) -> (String, mahiron_ctx::report::RunReport) {
    let output = tempfile::tempdir().expect("could not create the output directory");
    let path = output
        .path()
        .join(format!("mhrn-output.{}", format.extension()));
    let settings = settings_for(root, format, &path);
    let outcome = Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(&path).expect("the document was not written");
    (document, outcome.report)
}

/// A single source contributes paths relative to its own root, with no prefix.
fn key_for(path: &str) -> String {
    path.to_string()
}

#[test]
fn every_readable_case_survives_every_format() {
    let (_guard, root) = support::materialise();

    for format in FORMATS {
        let (document, _report) = package(&root, format);
        let extracted = extract(format, &document);

        for case in support::readable_cases() {
            let expected = case.expected_text.expect("filtered to readable cases");
            let key = key_for(case.path);
            let Some(actual) = extracted.get(&key) else {
                if case.path == "control-characters.txt" || case.path == "latin1.txt" {
                    continue;
                }
                panic!("{format:?}: {} is missing from the document", case.path);
            };

            if actual == expected {
                continue;
            }

            // Markdown and text place their closing delimiter on its own line, which a
            // file with no final newline cannot supply; nothing else may differ.
            let permitted = delimiters_require_final_newline(format)
                && !expected.is_empty()
                && !expected.ends_with('\n')
                && actual == &format!("{expected}\n");

            assert!(
                permitted,
                "{format:?}: {} came back changed\n  expected: {expected:?}\n  actual:   {actual:?}\n  ({})",
                case.path, case.note
            );
        }
    }
}

#[test]
fn machine_parsable_formats_are_exact_even_at_the_delimiters() {
    let (_guard, root) = support::materialise();

    for format in [OutputFormat::Xml, OutputFormat::Json] {
        let (document, _report) = package(&root, format);
        let extracted = extract(format, &document);

        for case in support::readable_cases() {
            let expected = case.expected_text.expect("filtered to readable cases");
            let Some(actual) = extracted.get(&key_for(case.path)) else {
                continue;
            };
            assert_eq!(
                actual, expected,
                "{format:?}: {} is not byte-exact ({})",
                case.path, case.note
            );
        }
    }
}

#[test]
fn an_undecodable_file_is_reported_rather_than_approximated() {
    let (_guard, root) = support::materialise();
    let (document, report) = package(&root, OutputFormat::Markdown);

    assert!(
        !document.contains('\u{FFFD}'),
        "a replacement character reached the document"
    );

    let binary = report
        .records
        .iter()
        .find(|record| record.path.ends_with("invalid-utf8.bin"))
        .expect("the undecodable file was never recorded");
    assert_eq!(binary.excluded, Some(ExclusionReason::BinaryContent));
}

#[test]
fn a_byte_order_mark_never_reaches_the_document() {
    let (_guard, root) = support::materialise_subset(&["utf8-bom.txt", "utf16le-bom.txt"]);

    for format in FORMATS {
        let (document, _report) = package(&root, format);
        assert!(
            !document.contains('\u{FEFF}'),
            "{format:?}: a byte order mark survived into the document"
        );
    }
}

#[test]
fn the_document_is_identical_across_repeated_runs() {
    let (_guard, root) = support::materialise();

    for format in FORMATS {
        let (first, _) = package(&root, format);
        let (second, _) = package(&root, format);
        assert_eq!(first, second, "{format:?}: two identical runs differed");
    }
}

#[test]
fn reported_counts_reconcile_with_the_document() {
    let (_guard, root) = support::materialise();

    for format in FORMATS {
        let (document, report) = package(&root, format);
        let extracted = extract(format, &document);
        assert_eq!(
            extracted.len(),
            report.included,
            "{format:?}: the summary and the document disagree on how many files there are"
        );
        assert_eq!(
            report.discovered,
            report.included + report.excluded,
            "{format:?}: discovered files are unaccounted for"
        );
        assert_eq!(
            report.output.bytes,
            document.len() as u64,
            "{format:?}: the reported size is not the size of the document"
        );
    }
}

#[test]
fn the_token_estimate_agrees_with_a_second_reading_of_the_document() {
    let (_guard, root) = support::materialise();
    let output = tempfile::tempdir().expect("could not create the output directory");

    for format in FORMATS {
        let path: PathBuf = output.path().join(format!("verify.{}", format.extension()));
        let mut settings = settings_for(&root, format, &path);
        settings.reporting.verify_token_count = true;
        let outcome = Engine::new(&settings).run().expect("the run failed");

        let mismatch = outcome
            .report
            .warnings
            .iter()
            .find(|warning| format!("{:?}", warning.kind).contains("TokenCountMismatch"));
        assert!(
            mismatch.is_none(),
            "{format:?}: the streamed and re-read token counts disagree"
        );
        assert!(outcome.report.output.tokens > 0);
    }
}

#[test]
fn transformations_are_absent_unless_they_are_asked_for() {
    let (_guard, root) = support::materialise_subset(&["trailing-whitespace.txt"]);
    let output = tempfile::tempdir().expect("could not create the output directory");
    let path = output.path().join("mhrn-output.json");

    let settings = settings_for(&root, OutputFormat::Json, &path);
    let _ = Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(&path).expect("the document was not written");
    let extracted = extract(OutputFormat::Json, &document);
    assert!(extracted[&key_for("trailing-whitespace.txt")].contains("spaces   \n"));

    let mut trimming = settings_for(&root, OutputFormat::Json, &path);
    trimming.transforms.trim_trailing_whitespace = true;
    let _ = Engine::new(&trimming).run().expect("the run failed");
    let document = std::fs::read_to_string(&path).expect("the document was not written");
    let extracted = extract(OutputFormat::Json, &document);
    assert!(extracted[&key_for("trailing-whitespace.txt")].contains("spaces\n"));
}
