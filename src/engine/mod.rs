//! The packaging engine.
//!
//! Every interface — the command line, MCP server mode, any future graphical shell —
//! calls [`Engine::run`] and receives the same result for the same settings. No filtering,
//! classification or composition decision lives anywhere else.

pub mod classify;
pub mod compose;
pub mod decode;
pub mod discovery;
pub mod ignore_rules;
pub mod source;
pub mod transform;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::compress::Registry;
use crate::config::Settings;
use crate::delivery::{Confirm, Delivery, NeverConfirm};
use crate::error::{Error, Result};
use crate::output::TreeEntry;
use crate::report::{
    DeliveryReport, ExclusionReason, FileRecord, NullProgress, Progress, RunReport, WarningKind,
    WarningRecord,
};

/// What a completed run produced.
#[derive(Debug)]
pub struct RunOutcome {
    pub report: RunReport,
    /// The document itself, when the destination retained it rather than writing it out.
    pub document: Option<Vec<u8>>,
}

/// Packaging engine bound to one set of settings.
pub struct Engine<'a> {
    settings: &'a Settings,
    registry: Registry,
    progress: &'a dyn Progress,
    confirm: &'a dyn Confirm,
    retain_document: bool,
}

impl<'a> Engine<'a> {
    pub fn new(settings: &'a Settings) -> Self {
        Self {
            settings,
            registry: Registry::load(),
            progress: &NullProgress,
            confirm: &NeverConfirm,
            retain_document: false,
        }
    }

    pub fn with_progress(mut self, progress: &'a dyn Progress) -> Self {
        self.progress = progress;
        self
    }

    pub fn with_confirmation(mut self, confirm: &'a dyn Confirm) -> Self {
        self.confirm = confirm;
        self
    }

    /// Keep the document in memory and hand it back instead of delivering it, for callers
    /// that must forward it themselves rather than write it somewhere.
    pub fn retaining_document(mut self) -> Self {
        self.retain_document = true;
        self
    }

    pub fn compression_languages(&self) -> Vec<&'static str> {
        self.registry.languages()
    }

    /// Package the designated sources and deliver the result.
    pub fn run(&self) -> Result<RunOutcome> {
        let started = Instant::now();
        self.settings.validate()?;

        let unknown = self
            .registry
            .unknown_languages(&self.settings.transforms.compression);
        if !unknown.is_empty() {
            return Err(Error::config(format!(
                "no structural-signature support for {}; this build supports {}",
                unknown.join(", "),
                if self.registry.is_empty() {
                    "no languages".to_string()
                } else {
                    self.registry.languages().join(", ")
                }
            )));
        }

        // Everything this run creates outside its own destination is registered for
        // removal, so a signal — which runs no destructors — leaves nothing behind.
        crate::cleanup::install_handler();

        // Permission is obtained first, and deliberately: it is the only step that can
        // ask the user a question, a question has to be asked before the progress
        // indicator starts drawing over the same line, and refusing after the work has
        // been done wastes all of it.
        //
        // Creating the staging file is a separate step and happens *after* discovery.
        // Doing both here put a file this run was writing inside the tree this run was
        // about to walk, where the walker duly found it, classified the empty file as
        // text, and packaged it.
        let may_replace = if self.retain_document {
            false
        } else {
            crate::delivery::guard_destination(self.settings, self.confirm)?
        };

        self.progress.phase("resolving sources");
        let acquisition = source::acquire(self.settings)?;
        let source_label = source::root_label(&acquisition.sources);

        self.progress.phase("scanning");
        let reserved = discovery::ReservedPaths {
            destination: self.settings.destination_path(),
            staging_directory: (!self.retain_document)
                .then(|| crate::delivery::staging_directory(self.settings))
                .flatten(),
        };
        let discovery = discovery::discover(
            &acquisition.sources,
            &self.settings.filters,
            &reserved,
            self.progress,
        )?;
        self.progress.phase_complete(&format!(
            "scan complete - {} files found, {} to package",
            discovery.discovered,
            discovery.candidates.len()
        ));

        if self.settings.failure_policy == crate::config::FailurePolicy::Strict {
            if let Some(warning) = discovery
                .warnings
                .iter()
                .find(|warning| warning.severity() == crate::report::Severity::Warning)
            {
                return Err(Error::Strict(format!(
                    "{}: {}",
                    warning.path.as_deref().unwrap_or("run"),
                    warning.kind.summary()
                )));
            }
        }

        let tree_entries = self.tree_entries(&discovery.candidates, &discovery.records);

        // Opened only now that the walk has finished, so the staging file cannot be
        // discovered by it.
        let mut delivery = if self.retain_document {
            Delivery::in_memory()
        } else {
            Delivery::open_permitted(self.settings, may_replace)?
        };

        self.progress.phase("packaging");
        let composition = {
            let writer = delivery.writer();
            compose::compose(
                &discovery.candidates,
                self.settings,
                &self.registry,
                &source_label,
                discovery.discovered,
                &tree_entries,
                writer,
                self.progress,
            )?
        };

        let committed = delivery.commit(composition.stats.bytes, self.confirm)?;
        let records = self.merge_records(discovery.records, &composition);

        let mut warnings = acquisition.warnings;
        warnings.extend(discovery.warnings);
        warnings.extend(composition.warnings);
        warnings.extend(committed.warnings);

        if composition.line_endings.len() > 1 {
            warnings.push(WarningRecord::global(WarningKind::MixedLineEndings));
        }

        if self.settings.reporting.verify_token_count {
            let mismatch = match (&committed.report, &committed.document) {
                (DeliveryReport::File { path }, _) => compose::verify_token_count(
                    std::path::Path::new(path),
                    self.settings.tokenization,
                    composition.stats.tokens,
                )?,
                (_, Some(document)) => compose::verify_token_count_in_memory(
                    document,
                    self.settings.tokenization,
                    composition.stats.tokens,
                )?,
                _ => None,
            };
            warnings.extend(mismatch);
        }

        // Pruned directories are counted apart from files. Folding them together made
        // `excluded` exceed `discovered`, which counts files only, on any repository with
        // a build directory in it.
        let mut exclusions: BTreeMap<ExclusionReason, usize> = BTreeMap::new();
        let mut excluded = 0_usize;
        for record in &records {
            if let Some(reason) = &record.excluded {
                *exclusions.entry(reason.clone()).or_default() += 1;
                if !record.path.ends_with('/') {
                    excluded += 1;
                }
            }
        }

        warnings.sort();

        let report = RunReport {
            source_label,
            format: self.settings.format,
            discovered: discovery.discovered,
            included: composition.written.len(),
            excluded,
            directories_pruned: discovery.pruned_directories,
            exclusions,
            records,
            warnings,
            output: composition.stats,
            delivery: committed.report,
            remote: acquisition.remote,
            line_endings: composition.line_endings,
            duration: started.elapsed(),
            show_duration: self.settings.reporting.show_duration,
            rank_files: self.settings.reporting.rank_files,
            transformations: self.settings.transforms.labels(),
            dry_run: self.settings.dry_run,
        };

        Ok(RunOutcome {
            report,
            document: committed.document,
        })
    }

    /// Paths shown in the structural overview, which may name files whose content is absent.
    fn tree_entries(
        &self,
        candidates: &[discovery::Candidate],
        records: &[FileRecord],
    ) -> Vec<TreeEntry> {
        let mut entries: Vec<TreeEntry> = candidates
            .iter()
            .map(|candidate| TreeEntry {
                path: candidate.display.clone(),
                included: true,
            })
            .collect();

        if self.settings.filters.show_excluded_in_tree {
            entries.extend(
                records
                    .iter()
                    .filter(|record| record.excluded.is_some() && !record.path.ends_with('/'))
                    .map(|record| TreeEntry {
                        path: record.path.clone(),
                        included: false,
                    }),
            );
        }
        entries
    }

    /// Fold composition-time outcomes back into the per-file record set.
    ///
    /// Indexed once rather than searched three times: the previous linear scan per
    /// element made this quadratic, and with `--top-files` on a hundred thousand files
    /// that is on the order of ten billion string comparisons.
    fn merge_records(
        &self,
        mut records: Vec<FileRecord>,
        composition: &compose::Composition,
    ) -> Vec<FileRecord> {
        let index: HashMap<String, usize> = records
            .iter()
            .enumerate()
            .map(|(position, record)| (record.path.clone(), position))
            .collect();

        for dropped in &composition.dropped {
            if let Some(existing) = index.get(&dropped.path).map(|p| &mut records[*p]) {
                existing.excluded = dropped.excluded.clone();
                existing.attribution = dropped.attribution.clone();
            }
        }
        for path in &composition.compressed {
            if let Some(existing) = index.get(path).map(|p| &mut records[*p]) {
                existing.compressed = true;
            }
        }
        for (path, tokens) in &composition.file_tokens {
            if let Some(existing) = index.get(path).map(|p| &mut records[*p]) {
                existing.tokens = Some(*tokens);
            }
        }
        records.sort_by(|a, b| discovery::compare_display_paths(&a.path, &b.path));
        records
    }
}
