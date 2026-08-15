use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

use crate::compress::Registry;
use crate::config::{LineEnding, Settings, TokenEncoding};
use crate::error::{Error, Result};
use crate::output::{
    writer_for, DigestWriter, DocumentContext, FileEntry, FileTree, MeteredSink, TreeEntry,
};
use crate::report::{
    ExclusionReason, FileRecord, OutputStats, Progress, WarningKind, WarningRecord,
};
use crate::tokens::TokenCounter;

use super::decode::{contains_disallowed_control, decode};
use super::discovery::Candidate;
use super::transform;

#[derive(Debug, Default)]
pub struct Composition {
    pub stats: OutputStats,
    /// Per-file token counts, present only when the invocation asked to rank files.
    pub file_tokens: Vec<(String, usize)>,
    pub warnings: Vec<WarningRecord>,
    /// Files that did make it into the document, in the order they appear there.
    pub written: Vec<String>,
    pub compressed: Vec<String>,
    /// Files dropped after discovery because their full content could not be used.
    pub dropped: Vec<FileRecord>,
    pub line_endings: BTreeMap<LineEnding, usize>,
    /// Files whose language this build cannot reduce, tallied rather than recorded one by
    /// one: on a mixed-language repository an unsupported language is the expected case,
    /// and a warning per file would bury everything else and say nothing the tally does not.
    pub uncompressed: BTreeMap<String, usize>,
}

/// Assemble the output document.
///
/// File sections are produced first into a spool and copied into the destination
/// afterwards, so the preface can state the number of files the document actually
/// contains rather than the number the pipeline set out to include.
#[allow(clippy::too_many_arguments)]
pub fn compose(
    candidates: &[Candidate],
    settings: &Settings,
    registry: &Registry,
    source_label: &str,
    discovered: usize,
    tree_entries: &[TreeEntry],
    destination: Box<dyn Write + '_>,
    progress: &dyn Progress,
) -> Result<Composition> {
    let mut writer = writer_for(settings.format);
    let mut composition = Composition::default();

    let mut spool = tempfile::tempfile()?;
    {
        let mut spool_sink = MeteredSink::new(
            Box::new(BufWriter::new(&mut spool)),
            None,
            settings.tokenization,
        );
        writer.begin_files(&mut spool_sink)?;
        for (index, candidate) in candidates.iter().enumerate() {
            write_candidate(
                candidate,
                settings,
                registry,
                writer.as_mut(),
                &mut spool_sink,
                &mut composition,
            )?;
            progress.packaged(index + 1, candidates.len());
        }
        writer.end_files(&mut spool_sink)?;
        spool_sink.finish()?;
    }

    if !composition.uncompressed.is_empty() {
        let files: usize = composition.uncompressed.values().sum();
        let mut languages: Vec<&str> = composition
            .uncompressed
            .keys()
            .map(String::as_str)
            .collect();
        languages.sort_unstable();
        let shown = languages
            .iter()
            .take(6)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let detail = match languages.len().saturating_sub(6) {
            0 => format!("{files} files in {shown}"),
            rest => format!("{files} files in {shown}, and {rest} other languages"),
        };
        composition
            .warnings
            .push(WarningRecord::global(WarningKind::CompressionUnsupported(
                detail,
            )));
    }
    progress.phase("writing document");
    spool.seek(SeekFrom::Start(0))?;

    let context = DocumentContext {
        source_label: source_label.to_string(),
        format: settings.format,
        discovered,
        included: composition.written.len(),
        transformations: settings.transforms.labels(),
        include_preface: settings.composition.include_preface,
        header_text: settings.composition.header_text.clone(),
        footer_text: settings.composition.footer_text.clone(),
        tree_style: settings.composition.tree_style,
    };

    let counter = TokenCounter::new(settings.tokenization)?;
    let mut sink = MeteredSink::new(destination, Some(counter), settings.tokenization);

    writer.begin(&mut sink, &context)?;
    if settings.composition.include_tree {
        let tree = FileTree::build(tree_root_label(source_label), tree_entries);
        writer.tree(&mut sink, &tree)?;
    }
    copy_with_progress(&mut spool, &mut sink, progress)?;
    writer.finish(&mut sink, &context)?;

    composition.stats = sink.finish()?;
    progress.finish();
    Ok(composition)
}

/// Copy the spooled file sections into the destination, reporting as it goes.
///
/// This is where a large document spends most of its time — every byte is measured and
/// tokenized on the way past — so it is the last place that should look like a stall.
fn copy_with_progress(
    spool: &mut std::fs::File,
    sink: &mut MeteredSink<'_>,
    progress: &dyn Progress,
) -> Result<()> {
    const CHUNK: usize = 256 * 1024;

    let total = spool.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buffer = vec![0_u8; CHUNK];
    let mut done = 0_u64;

    loop {
        let read = spool.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sink.write_all(&buffer[..read])?;
        done += read as u64;
        progress.written(done, total.max(done));
    }
    Ok(())
}

fn write_candidate(
    candidate: &Candidate,
    settings: &Settings,
    registry: &Registry,
    writer: &mut dyn DigestWriter,
    spool: &mut MeteredSink<'_>,
    composition: &mut Composition,
) -> Result<()> {
    let bytes = match std::fs::read(&candidate.absolute) {
        Ok(bytes) => bytes,
        Err(error) => {
            return drop_candidate(
                candidate,
                settings,
                composition,
                ExclusionReason::Unreadable,
                WarningKind::Unreadable(error.to_string()),
            )
        }
    };

    let decoded = match decode(&bytes, &candidate.classification) {
        Ok(decoded) => decoded,
        Err(error) => {
            return drop_candidate(
                candidate,
                settings,
                composition,
                ExclusionReason::UndeterminedEncoding,
                WarningKind::Unreadable(error.to_string()),
            )
        }
    };

    for (ending, count) in &decoded.line_endings {
        *composition.line_endings.entry(*ending).or_default() += count;
    }

    // Checked for every format that cannot carry the result, not only XML, and under a
    // reason that describes what happened. Classification reads only the first few
    // kilobytes of a file, so a control character further in arrives here having passed
    // as text, and markdown and plain text would previously have carried the raw byte
    // straight into the document.
    if !settings.format.carries_control_characters() && contains_disallowed_control(&decoded.text) {
        return drop_candidate(
            candidate,
            settings,
            composition,
            ExclusionReason::ControlCharacters,
            WarningKind::ControlCharacters,
        );
    }

    let mut compressed = false;
    // Borrowed unless something actually rewrites the text, so a run with no compression
    // and no transformations never copies a file's content at all.
    let content: Cow<'_, str> = if settings.transforms.compression.is_enabled() {
        match registry.resolve(&candidate.absolute, &settings.transforms.compression) {
            Some(compressor) => match compressor.compress(&decoded.text) {
                Ok(reduced) => {
                    compressed = true;
                    composition.compressed.push(candidate.display.clone());
                    Cow::Owned(reduced)
                }
                Err(error) => {
                    composition.warnings.push(
                        WarningRecord::about(
                            candidate.display.clone(),
                            WarningKind::CompressionFailed(error.to_string()),
                        )
                        .with_language(compressor.language().to_string()),
                    );
                    Cow::Borrowed(decoded.text.as_ref())
                }
            },
            None => {
                *composition
                    .uncompressed
                    .entry(language_label(&candidate.display))
                    .or_default() += 1;
                Cow::Borrowed(decoded.text.as_ref())
            }
        }
    } else {
        Cow::Borrowed(decoded.text.as_ref())
    };

    let content = transform::apply(&content, &settings.transforms);

    if settings.reporting.rank_files > 0 {
        // Counted from the content that was actually packaged, transformations and
        // compression included, so the ranking reflects what the document costs.
        //
        // This is a second tokenisation of the corpus, on top of the incremental count
        // the sink performs over the assembled document, and roughly doubles the
        // tokenising cost of a run that asks for it. Metering per-file spans in the spool
        // sink instead would avoid that, but it would count each file's framing — fences,
        // headings, CDATA delimiters — into that file's total, which is not what the
        // ranking is meant to answer. The extra pass is the price of the honest number,
        // and it is only paid when `--top-files` is given.
        let tokens = crate::tokens::count_str(settings.tokenization, content.as_ref())?;
        composition
            .file_tokens
            .push((candidate.display.clone(), tokens));
    }

    writer.file(
        spool,
        &FileEntry {
            path: &candidate.display,
            content: content.as_ref(),
            compressed,
        },
    )?;
    composition.written.push(candidate.display.clone());
    Ok(())
}

fn drop_candidate(
    candidate: &Candidate,
    settings: &Settings,
    composition: &mut Composition,
    reason: ExclusionReason,
    warning: WarningKind,
) -> Result<()> {
    if settings.failure_policy == crate::config::FailurePolicy::Strict {
        return Err(Error::Strict(format!(
            "{}: {}",
            candidate.display,
            warning.summary()
        )));
    }
    composition.dropped.push(FileRecord {
        path: candidate.display.clone(),
        size: candidate.size,
        excluded: Some(reason),
        attribution: None,
        encoding: None,
        compressed: false,
        tokens: None,
    });
    composition
        .warnings
        .push(WarningRecord::about(candidate.display.clone(), warning));
    Ok(())
}

/// Extension used as the language label in aggregated reporting, without any lookup table.
fn language_label(display: &str) -> String {
    display
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
        .map(|(_, ext)| format!(".{ext}"))
        .unwrap_or_else(|| "(no extension)".to_string())
}

fn tree_root_label(source_label: &str) -> String {
    source_label
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .unwrap_or("source")
        .to_string()
}

/// Recount from the delivered document, as an independent check on the count produced
/// while the document was streaming past.
pub fn verify_token_count(
    path: &std::path::Path,
    encoding: TokenEncoding,
    expected: usize,
) -> Result<Option<WarningRecord>> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let observed = crate::tokens::count_reader(encoding, file)?;
    if observed == expected {
        return Ok(None);
    }
    Ok(Some(WarningRecord::global(
        WarningKind::TokenCountMismatch(format!("streamed {expected}, re-read {observed}")),
    )))
}

/// Token count for a document held in memory, used where nothing was written to disk.
pub fn verify_token_count_in_memory(
    document: &[u8],
    encoding: TokenEncoding,
    expected: usize,
) -> Result<Option<WarningRecord>> {
    let observed = crate::tokens::count_reader(encoding, document)?;
    if observed == expected {
        return Ok(None);
    }
    Ok(Some(WarningRecord::global(
        WarningKind::TokenCountMismatch(format!("streamed {expected}, re-read {observed}")),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_labels_come_from_the_extension_alone() {
        assert_eq!(language_label("src/main.rs"), ".rs");
        assert_eq!(language_label("Makefile"), "(no extension)");
        assert_eq!(language_label("a/.gitignore"), "(no extension)");
    }

    #[test]
    fn tree_root_uses_the_final_path_segment() {
        assert_eq!(tree_root_label("./my-project"), "my-project");
        assert_eq!(tree_root_label("/tmp/repo/"), "repo");
        assert_eq!(tree_root_label("."), "source");
    }
}
