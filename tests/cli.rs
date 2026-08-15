//! Behaviour visible from outside: filtering, precedence, symbolic links, exit status.

mod support;

use std::path::{Path, PathBuf};

#[cfg(unix)]
use mahiron_ctx::config::SymlinkPolicy;
use mahiron_ctx::config::{Destination, OutputFormat, Settings, SourceSpec};
use mahiron_ctx::engine::Engine;
use mahiron_ctx::report::{ExclusionReason, RunStatus};

use support::extract::{extract, extract_order};

struct Project {
    _guard: tempfile::TempDir,
    root: PathBuf,
    output: PathBuf,
}

impl Project {
    fn new() -> Self {
        let guard = tempfile::tempdir().expect("could not create the project directory");
        let root = guard.path().join("project");
        std::fs::create_dir_all(&root).expect("could not create the project root");
        let output = guard.path().join("mhrn-output.json");
        Self {
            _guard: guard,
            root,
            output,
        }
    }

    fn file(&self, relative: &str, contents: &str) -> &Self {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("could not create a directory");
        }
        std::fs::write(path, contents).expect("could not write a file");
        self
    }

    fn settings(&self) -> Settings {
        let mut settings = Settings {
            sources: vec![SourceSpec::Local(self.root.clone())],
            format: OutputFormat::Json,
            destination: Destination::File(self.output.clone()),
            overwrite: true,
            ..Default::default()
        };
        settings.reporting.progress = false;
        settings
    }

    fn run(&self, settings: &Settings) -> (Vec<String>, mahiron_ctx::report::RunReport) {
        let output = self.output.clone();
        self.run_at(settings, &output)
    }

    /// Run against a destination other than the default one, for the tests that care
    /// where the document lands relative to the tree being packaged.
    fn run_at(
        &self,
        settings: &Settings,
        output: &Path,
    ) -> (Vec<String>, mahiron_ctx::report::RunReport) {
        let outcome = Engine::new(settings).run().expect("the run failed");
        let document = std::fs::read_to_string(output).expect("no document was written");
        // Document order, not sorted order: several tests here are about sequence.
        let paths = extract_order(settings.format, &document);
        (paths, outcome.report)
    }
}

#[test]
fn a_bare_run_packages_everything_worth_packaging() {
    let project = Project::new();
    project
        .file("src/main.rs", "fn main() {}\n")
        .file("README.md", "# project\n")
        .file(".hidden", "hidden\n")
        .file("target/debug/artefact", "built\n")
        .file("node_modules/pkg/index.js", "module\n");

    let (paths, report) = project.run(&project.settings());
    assert!(paths.contains(&"src/main.rs".to_string()));
    assert!(paths.contains(&"README.md".to_string()));
    assert!(!paths.iter().any(|p| p.starts_with("target/")));
    assert!(!paths.iter().any(|p| p.starts_with("node_modules/")));
    assert!(!paths.contains(&".hidden".to_string()));
    assert_eq!(report.status(), RunStatus::Success);
}

#[test]
fn version_control_ignore_rules_are_honoured_and_can_be_set_aside() {
    let project = Project::new();
    project
        .file(".gitignore", "generated/\n*.log\n")
        .file("generated/output.txt", "generated\n")
        .file("run.log", "log\n")
        .file("kept.txt", "kept\n");

    let (paths, _) = project.run(&project.settings());
    assert_eq!(paths, vec!["kept.txt".to_string()]);

    let mut settings = project.settings();
    settings.filters.use_vcs_ignore = false;
    let (paths, _) = project.run(&settings);
    assert!(paths.contains(&"run.log".to_string()));
    assert!(paths.contains(&"generated/output.txt".to_string()));
}

#[test]
fn a_nested_ignore_file_overrides_a_shallower_one() {
    let project = Project::new();
    project
        .file(".gitignore", "*.txt\n")
        .file("keep/.gitignore", "!*.txt\n")
        .file("keep/wanted.txt", "wanted\n")
        .file("drop/unwanted.txt", "unwanted\n");

    let (paths, _) = project.run(&project.settings());
    assert!(paths.contains(&"keep/wanted.txt".to_string()));
    assert!(!paths.contains(&"drop/unwanted.txt".to_string()));
}

#[test]
fn invocation_patterns_outrank_every_ignore_file() {
    let project = Project::new();
    project
        .file(".gitignore", "secret.txt\n")
        .file(".mahironignore", "also-secret.txt\n")
        .file("secret.txt", "secret\n")
        .file("also-secret.txt", "secret\n");

    let mut settings = project.settings();
    settings.filters.include = vec!["secret.txt".into(), "also-secret.txt".into()];
    let (paths, _) = project.run(&settings);
    assert!(paths.contains(&"secret.txt".to_string()));
    assert!(paths.contains(&"also-secret.txt".to_string()));
}

#[test]
fn the_more_specific_pattern_decides_and_a_tie_excludes() {
    let project = Project::new();
    project
        .file("src/keep.rs", "keep\n")
        .file("src/generated/thing.rs", "generated\n")
        .file("other/tie.rs", "tie\n");

    let mut settings = project.settings();
    settings.filters.include = vec!["**/*.rs".into()];
    settings.filters.exclude = vec!["src/generated/*.rs".into()];
    let (paths, _) = project.run(&settings);
    assert!(paths.contains(&"src/keep.rs".to_string()));
    assert!(!paths.contains(&"src/generated/thing.rs".to_string()));

    let mut tied = project.settings();
    tied.filters.include = vec!["other/tie.rs".into()];
    tied.filters.exclude = vec!["other/tie.rs".into()];
    let (paths, _) = project.run(&tied);
    assert!(
        !paths.contains(&"other/tie.rs".to_string()),
        "an equally specific pair must resolve to exclusion"
    );
}

#[test]
fn a_size_threshold_excludes_by_size_and_says_so() {
    let project = Project::new();
    project
        .file("small.txt", "small\n")
        .file("large.txt", &"x".repeat(4096));

    let mut settings = project.settings();
    settings.filters.max_file_size = Some(1024);
    let (paths, report) = project.run(&settings);

    assert_eq!(paths, vec!["small.txt".to_string()]);
    assert_eq!(
        report.exclusions.get(&ExclusionReason::SizeThreshold),
        Some(&1)
    );
}

#[test]
fn a_binary_file_is_excluded_by_content_and_can_be_forced_back_in() {
    let project = Project::new();
    std::fs::write(project.root.join("data.bin"), [0u8, 1, 2, 3, 0xFF]).unwrap();
    project.file("text.txt", "text\n");

    let (paths, report) = project.run(&project.settings());
    assert_eq!(paths, vec!["text.txt".to_string()]);
    assert_eq!(
        report.exclusions.get(&ExclusionReason::BinaryContent),
        Some(&1)
    );

    let mut forced = project.settings();
    forced.filters.force_text = vec!["*.bin".into()];
    let (paths, _) = project.run(&forced);
    assert!(paths.contains(&"data.bin".to_string()));
}

#[test]
fn extensions_are_taken_as_given_and_never_judged() {
    let project = Project::new();
    project
        .file("component.vue", "<template/>\n")
        .file("script.zsh", "echo\n")
        .file("thing.wat", "(module)\n")
        .file("no-extension", "content\n")
        .file("archive.tar.gz.txt", "not really an archive\n");

    let (paths, _) = project.run(&project.settings());
    assert_eq!(paths.len(), 5, "an extension was second-guessed: {paths:?}");
}

#[test]
fn a_file_may_be_kept_out_of_the_content_while_staying_in_the_overview() {
    let project = Project::new();
    project
        .file("kept.txt", "kept\n")
        .file("shown.txt", "shown\n");

    let mut settings = project.settings();
    settings.format = OutputFormat::Markdown;
    settings.destination = Destination::File(project.output.with_extension("md"));
    settings.filters.exclude = vec!["shown.txt".into()];
    settings.filters.show_excluded_in_tree = true;

    let _ = Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(project.output.with_extension("md")).unwrap();

    assert!(
        document.contains("shown.txt"),
        "the overview omits the file"
    );
    assert!(
        !document.contains("### shown.txt"),
        "the content of an excluded file reached the document"
    );
}

#[test]
fn every_discovered_file_carries_a_reason_when_it_is_not_included() {
    let project = Project::new();
    project
        .file(".gitignore", "ignored.txt\n")
        .file("ignored.txt", "ignored\n")
        .file("large.txt", &"x".repeat(4096))
        .file("kept.txt", "kept\n");
    std::fs::write(project.root.join("data.bin"), [0u8, 159, 146, 150]).unwrap();

    let mut settings = project.settings();
    settings.filters.max_file_size = Some(1024);
    let (_, report) = project.run(&settings);

    for record in &report.records {
        if record.path.ends_with('/') {
            continue;
        }
        // `.gitignore` is itself hidden, and so is excluded like any other hidden file.
        let included = record.path.ends_with("kept.txt");
        assert_eq!(
            record.excluded.is_none(),
            included,
            "{} has no attributable outcome",
            record.path
        );
    }
}

#[test]
fn a_dry_run_reports_without_writing() {
    let project = Project::new();
    project.file("a.txt", "a\n");

    let mut settings = project.settings();
    settings.dry_run = true;
    let outcome = Engine::new(&settings).run().expect("the run failed");

    assert_eq!(outcome.report.included, 1);
    assert!(outcome.report.output.bytes > 0);
    assert!(!project.output.exists(), "a dry run wrote a document");
}

#[test]
fn an_existing_destination_is_not_overwritten_without_permission() {
    let project = Project::new();
    project.file("a.txt", "a\n");
    std::fs::write(&project.output, "previous").unwrap();

    let mut settings = project.settings();
    settings.overwrite = false;
    assert!(matches!(
        Engine::new(&settings).run(),
        Err(mahiron_ctx::Error::RefuseOverwrite(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&project.output).unwrap(),
        "previous"
    );
}

#[test]
fn the_output_order_does_not_depend_on_the_order_of_traversal() {
    let project = Project::new();
    for index in 0..64 {
        project.file(&format!("dir{}/file{index}.txt", index % 8), "content\n");
    }

    let (first, _) = project.run(&project.settings());
    let (second, _) = project.run(&project.settings());
    assert_eq!(first, second);

    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(
        first, sorted,
        "the document is not in a deterministic order"
    );
}

#[cfg(unix)]
#[test]
fn a_link_leaving_every_source_is_not_followed_unless_it_is_allowed_to_be() {
    let project = Project::new();
    project.file("inside.txt", "inside\n");

    let outside = project._guard.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, project.root.join("link")).unwrap();

    let (paths, _) = project.run(&project.settings());
    assert!(!paths.iter().any(|path| path.contains("secret.txt")));

    let mut permissive = project.settings();
    permissive.filters.symlinks = SymlinkPolicy::Always;
    let (paths, _) = project.run(&permissive);
    assert!(paths.iter().any(|path| path.contains("secret.txt")));
}

#[cfg(unix)]
#[test]
fn a_circular_link_does_not_stop_the_run() {
    let project = Project::new();
    project.file("a/file.txt", "content\n");
    std::os::unix::fs::symlink(project.root.join("a"), project.root.join("a/loop")).unwrap();

    let mut settings = project.settings();
    settings.filters.symlinks = SymlinkPolicy::Always;
    let (paths, _) = project.run(&settings);
    assert!(paths.contains(&"a/file.txt".to_string()));
}

#[test]
fn several_sources_are_packaged_into_one_document_without_collision() {
    let guard = tempfile::tempdir().unwrap();
    let first = guard.path().join("alpha");
    let second = guard.path().join("beta");
    std::fs::create_dir_all(first.join("src")).unwrap();
    std::fs::create_dir_all(second.join("src")).unwrap();
    std::fs::write(first.join("src/main.rs"), "alpha\n").unwrap();
    std::fs::write(second.join("src/main.rs"), "beta\n").unwrap();

    let output = guard.path().join("mhrn-output.json");
    let mut settings = Settings {
        sources: vec![
            SourceSpec::Local(first.clone()),
            SourceSpec::Local(second.clone()),
        ],
        format: OutputFormat::Json,
        destination: Destination::File(output.clone()),
        overwrite: true,
        ..Default::default()
    };
    settings.reporting.progress = false;

    let _ = Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(&output).unwrap();
    let extracted = extract(OutputFormat::Json, &document);

    assert_eq!(extracted["alpha/src/main.rs"], "alpha\n");
    assert_eq!(extracted["beta/src/main.rs"], "beta\n");
}

#[test]
fn a_missing_source_fails_with_a_message_naming_it() {
    let mut settings = Settings {
        sources: vec![SourceSpec::Local(PathBuf::from("/no/such/place/here"))],
        ..Default::default()
    };
    settings.reporting.progress = false;

    let error = Engine::new(&settings).run().unwrap_err();
    assert!(error.to_string().contains("/no/such/place/here"));
}

#[test]
fn structural_signatures_are_opt_in_and_report_what_they_could_not_reduce() {
    let project = Project::new();
    project
        .file("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n")
        .file("script.py", "def answer():\n    return 42\n");

    let (paths, report) = project.run(&project.settings());
    assert!(paths.contains(&"src/lib.rs".to_string()));
    assert!(report.records.iter().all(|record| !record.compressed));

    let mut settings = project.settings();
    settings.transforms.compression = mahiron_ctx::config::CompressionRequest::AllSupported;
    let (paths, report) = project.run(&settings);

    assert!(paths.contains(&"script.py".to_string()));
    let unsupported = report
        .warnings
        .iter()
        .any(|warning| format!("{:?}", warning.kind).contains("CompressionUnsupported"));
    assert!(unsupported, "an unsupported language went unreported");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("could not read the document")
}

#[test]
fn a_header_and_footer_appear_where_they_were_asked_to() {
    let project = Project::new();
    project.file("a.txt", "a\n");

    let output = project.output.with_extension("md");
    let mut settings = project.settings();
    settings.format = OutputFormat::Markdown;
    settings.destination = Destination::File(output.clone());
    settings.composition.header_text = Some("READ THIS FIRST".into());
    settings.composition.footer_text = Some("READ THIS LAST".into());

    let _ = Engine::new(&settings).run().expect("the run failed");
    let document = read(&output);
    let header = document
        .find("READ THIS FIRST")
        .expect("the header is missing");
    let footer = document
        .find("READ THIS LAST")
        .expect("the footer is missing");
    assert!(header < footer);
}

#[test]
fn lock_files_are_left_out_by_default_and_can_be_asked_for() {
    let project = Project::new();
    project
        .file("Cargo.lock", "generated\n")
        .file("package-lock.json", "{}\n")
        .file("src/main.rs", "fn main() {}\n");

    let (paths, _) = project.run(&project.settings());
    assert_eq!(paths, vec!["src/main.rs".to_string()]);

    let mut wanted = project.settings();
    wanted.filters.include = vec!["Cargo.lock".into()];
    let (paths, _) = project.run(&wanted);
    assert!(paths.contains(&"Cargo.lock".to_string()));

    let mut everything = project.settings();
    everything.filters.use_default_exclusions = false;
    let (paths, _) = project.run(&everything);
    assert!(paths.contains(&"package-lock.json".to_string()));
}

#[test]
fn fenced_blocks_carry_the_identifier_a_highlighter_expects() {
    let project = Project::new();
    project
        .file("src/main.rs", "fn main() {}\n")
        .file("config.yml", "key: value\n")
        .file("Makefile", "all:\n");

    let output = project.output.with_extension("md");
    let mut settings = project.settings();
    settings.format = OutputFormat::Markdown;
    settings.destination = Destination::File(output.clone());

    let _ = Engine::new(&settings).run().expect("the run failed");
    let document = read(&output);

    assert!(document.contains("```rust"), "extension used verbatim");
    assert!(document.contains("```yaml"));
    assert!(document.contains("```makefile"));
}

#[test]
fn the_document_follows_the_order_of_the_structural_overview() {
    let project = Project::new();
    project
        .file("config.rs", "config\n")
        .file("src/analysis.rs", "analysis\n")
        .file("src/zebra.rs", "zebra\n")
        .file("README.md", "readme\n");

    let (paths, _) = project.run(&project.settings());

    // Directories first at each level, exactly as the overview draws them.
    assert_eq!(
        paths,
        vec![
            "src/analysis.rs".to_string(),
            "src/zebra.rs".to_string(),
            "README.md".to_string(),
            "config.rs".to_string(),
        ]
    );
}

#[test]
fn json_output_carries_the_structural_overview() {
    let project = Project::new();
    project
        .file("src/main.rs", "fn main() {}\n")
        .file("README.md", "readme\n");

    let _ = Engine::new(&project.settings())
        .run()
        .expect("the run failed");
    let document = read(&project.output);
    let parsed: serde_json::Value = serde_json::from_str(&document).expect("invalid JSON");

    let children = parsed["tree"]["children"]
        .as_array()
        .expect("the document has no tree");
    assert_eq!(children[0]["name"], "src");
    assert_eq!(children[0]["type"], "directory");
    assert_eq!(children[0]["children"][0]["name"], "main.rs");
    assert_eq!(children.last().unwrap()["name"], "README.md");
}

#[test]
fn the_heaviest_files_are_counted_only_when_they_are_asked_for() {
    let project = Project::new();
    project
        .file("big.txt", &"word ".repeat(500))
        .file("small.txt", "word\n");

    let (_, quiet) = project.run(&project.settings());
    assert!(quiet.records.iter().all(|record| record.tokens.is_none()));

    let mut ranked = project.settings();
    ranked.reporting.rank_files = 5;
    let (_, report) = project.run(&ranked);

    let heaviest = report.heaviest_files(5);
    assert_eq!(heaviest.first().map(|(path, _)| *path), Some("big.txt"));
    assert!(heaviest[0].1 > heaviest[1].1);
}

#[test]
fn a_previous_run_s_document_is_not_packaged_by_the_next_one() {
    let project = Project::new();
    project.file("src/main.rs", "fn main() {}\n");

    // The destination sits inside the source, which is what happens by default.
    let inside = project.root.join("project-mhrn.json");
    let mut settings = project.settings();
    settings.destination = Destination::File(inside.clone());

    let _ = Engine::new(&settings).run().expect("the first run failed");
    let outcome = Engine::new(&settings).run().expect("the second run failed");

    let document = read(&inside);
    let paths = extract_order(OutputFormat::Json, &document);
    assert_eq!(paths, vec!["src/main.rs".to_string()]);

    let own = outcome
        .report
        .records
        .iter()
        .find(|record| record.path.ends_with("project-mhrn.json"))
        .expect("the document was not recorded at all");
    assert_eq!(own.excluded, Some(ExclusionReason::OutputDocument));
}

#[test]
fn a_run_never_packages_the_staging_file_it_is_writing_through() {
    // Delivery opens a staging file beside the destination and renames it into place.
    // It used to be created *before* discovery, so the walk found it, classified the
    // empty file as text, and packaged it as an empty section. Only `--hidden` exposed
    // it, because the old staging name began with a dot.
    let project = Project::new();
    project.file("src/main.rs", "fn main() {}\n");

    let inside = project.root.join("project-mhrn.json");
    let mut settings = project.settings();
    settings.destination = Destination::File(inside.clone());
    settings.filters.include_hidden = true;

    let (paths, _) = project.run_at(&settings, &inside);
    assert_eq!(
        paths,
        vec!["src/main.rs".to_string()],
        "the run packaged something it was itself writing"
    );
}

#[test]
fn two_runs_in_one_directory_do_not_package_each_other() {
    let project = Project::new();
    project.file("src/main.rs", "fn main() {}\n");

    let first = project.root.join("first-mhrn.json");
    let mut settings = project.settings();
    settings.destination = Destination::File(first.clone());
    settings.filters.include_hidden = true;

    // A staging file left in place by another run in flight, which the walk will meet.
    let staging = project
        .root
        .join(format!("{}ZZZZZZ", mahiron_ctx::delivery::STAGING_PREFIX));
    std::fs::write(&staging, "a partially written document from elsewhere").unwrap();

    let (paths, _) = project.run_at(&settings, &first);
    assert_eq!(paths, vec!["src/main.rs".to_string()], "{paths:?}");
}

#[test]
fn an_inclusion_reaches_into_a_directory_the_defaults_would_prune() {
    // The precedence rules put --include above the built-in defaults, but directories
    // were pruned before any pattern was consulted, so this returned nothing at all.
    let project = Project::new();
    project
        .file(".github/workflows/ci.yml", "name: ci\n")
        .file("target/debug/generated.rs", "fn generated() {}\n")
        .file("src/main.rs", "fn main() {}\n");

    let mut settings = project.settings();
    settings.filters.include = vec![".github/**".into()];
    let (paths, _) = project.run(&settings);
    assert_eq!(paths, vec![".github/workflows/ci.yml".to_string()]);

    let mut settings = project.settings();
    settings.filters.include = vec!["target/**".into()];
    let (paths, _) = project.run(&settings);
    assert_eq!(paths, vec!["target/debug/generated.rs".to_string()]);
}

#[test]
fn an_exclusion_prunes_a_directory_rather_than_every_file_in_it() {
    let project = Project::new();
    project
        .file("node_modules/a/index.js", "a\n")
        .file("node_modules/b/index.js", "b\n")
        .file("src/main.rs", "fn main() {}\n");

    let mut settings = project.settings();
    settings.filters.use_default_exclusions = false;
    settings.filters.exclude = vec!["node_modules/**".into()];
    let (paths, report) = project.run(&settings);

    assert_eq!(paths, vec!["src/main.rs".to_string()]);
    assert_eq!(
        report.directories_pruned, 1,
        "the subtree was walked instead of pruned at its boundary"
    );
    assert!(
        !report.records.iter().any(|r| r.path.ends_with("index.js")),
        "a record was produced for every file inside a pruned directory"
    );
}

#[test]
fn a_single_file_may_be_designated_as_a_source() {
    // `WalkBuilder` yields a file root as one entry at depth zero, which the walk
    // discarded along with every directory root — so this packaged nothing.
    let project = Project::new();
    project
        .file("src/main.rs", "fn main() {}\n")
        .file("src/other.rs", "fn other() {}\n");

    let mut settings = project.settings();
    settings.sources = vec![SourceSpec::Local(project.root.join("src/main.rs"))];
    let (paths, _) = project.run(&settings);
    assert_eq!(paths, vec!["main.rs".to_string()]);
}

#[test]
fn a_source_named_after_a_pruned_directory_is_still_packaged() {
    // Rules were matched against the display path, which carries a source prefix only
    // when more than one source is named. A source directory called `target` therefore
    // made every file under it look like a build artefact.
    let guard = tempfile::tempdir().unwrap();
    let target = guard.path().join("target");
    let other = guard.path().join("other");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(target.join("a.rs"), "a\n").unwrap();
    std::fs::write(other.join("b.rs"), "b\n").unwrap();

    let output = guard.path().join("mhrn-output.json");
    let mut settings = Settings {
        sources: vec![
            SourceSpec::Local(target.clone()),
            SourceSpec::Local(other.clone()),
        ],
        format: OutputFormat::Json,
        destination: Destination::File(output.clone()),
        overwrite: true,
        ..Default::default()
    };
    settings.reporting.progress = false;

    Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(&output).unwrap();
    let extracted = extract(OutputFormat::Json, &document);
    assert_eq!(extracted["target/a.rs"], "a\n");
    assert_eq!(extracted["other/b.rs"], "b\n");
}

#[test]
fn sources_with_the_same_directory_name_do_not_collide() {
    let guard = tempfile::tempdir().unwrap();
    let first = guard.path().join("alpha").join("src");
    let second = guard.path().join("beta").join("src");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(first.join("main.rs"), "alpha\n").unwrap();
    std::fs::write(second.join("main.rs"), "beta\n").unwrap();

    let output = guard.path().join("mhrn-output.json");
    let mut settings = Settings {
        sources: vec![SourceSpec::Local(first), SourceSpec::Local(second)],
        format: OutputFormat::Json,
        destination: Destination::File(output.clone()),
        overwrite: true,
        ..Default::default()
    };
    settings.reporting.progress = false;

    Engine::new(&settings).run().expect("the run failed");
    let document = std::fs::read_to_string(&output).unwrap();
    let extracted = extract(OutputFormat::Json, &document);
    assert_eq!(
        extracted.len(),
        2,
        "one source overwrote the other: {extracted:?}"
    );
    assert!(extracted.values().any(|v| v == "alpha\n"));
    assert!(extracted.values().any(|v| v == "beta\n"));
}

#[test]
fn a_control_character_beyond_the_classification_prefix_is_caught() {
    // Classification reads only the first few kilobytes, so a control character further
    // in passed as text and was carried into markdown and plain text unaltered.
    let project = Project::new();
    let mut contents = "harmless line\n".repeat(1500);
    contents.push('\u{0}');
    contents.push('\n');
    project.file("late.txt", &contents);
    project.file("clean.txt", "clean\n");

    let mut settings = project.settings();
    settings.format = OutputFormat::Markdown;
    let output = project.root.parent().unwrap().join("mhrn-output.md");
    settings.destination = Destination::File(output.clone());

    let (paths, report) = project.run_at(&settings, &output);
    assert_eq!(paths, vec!["clean.txt".to_string()], "{paths:?}");
    assert_eq!(
        report.exclusions.get(&ExclusionReason::ControlCharacters),
        Some(&1),
        "dropped under a reason that does not describe what happened"
    );
    assert!(!read(&output).contains('\u{0}'));
}

#[test]
fn credential_like_files_are_left_out_until_they_are_asked_for() {
    let project = Project::new();
    project
        .file(".env", "SECRET=hunter2\n")
        .file("deploy/id_rsa", "-----BEGIN PRIVATE KEY-----\n")
        .file("src/main.rs", "fn main() {}\n");

    let mut settings = project.settings();
    settings.filters.include_hidden = true;
    let (paths, report) = project.run(&settings);
    assert_eq!(paths, vec!["src/main.rs".to_string()], "{paths:?}");
    assert_eq!(
        report.exclusions.get(&ExclusionReason::CredentialLike),
        Some(&2)
    );
    // Reported, but not as a failure: nothing went wrong.
    assert_eq!(report.status(), RunStatus::Success);

    let mut allowed = settings.clone();
    allowed.filters.allow_secrets = true;
    let (paths, _) = project.run(&allowed);
    assert!(paths.contains(&".env".to_string()), "{paths:?}");
}

#[test]
fn a_repository_that_merely_mixes_line_endings_still_exits_cleanly() {
    // Mixed line endings are the normal state of anything touched from both Windows and
    // Unix. Treating that as a warning made a clean run look like a failure to CI.
    let project = Project::new();
    project
        .file("unix.txt", "one\ntwo\n")
        .file("windows.txt", "one\r\ntwo\r\n");

    let (_, report) = project.run(&project.settings());
    assert!(report.line_endings.len() > 1, "the corpus did not mix");
    assert_eq!(report.status(), RunStatus::Success);
    assert_eq!(report.status().exit_code(), 0);
}

#[test]
fn an_unsupported_language_is_reported_once_and_is_not_a_failure() {
    let project = Project::new();
    for index in 0..12 {
        project.file(
            &format!("script{index}.py"),
            "def answer():\n    return 42\n",
        );
    }
    project.file("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");

    let mut settings = project.settings();
    settings.transforms.compression = mahiron_ctx::config::CompressionRequest::AllSupported;
    let (_, report) = project.run(&settings);

    let unsupported = report
        .warnings
        .iter()
        .filter(|w| format!("{:?}", w.kind).contains("CompressionUnsupported"))
        .count();
    assert_eq!(
        unsupported, 1,
        "one record per file buries every other warning"
    );
    assert_eq!(report.status(), RunStatus::Success);
}

#[test]
fn the_three_file_counts_reconcile_with_one_another() {
    let project = Project::new();
    project
        .file("src/main.rs", "fn main() {}\n")
        .file("target/debug/x.rs", "generated\n")
        .file("Cargo.lock", "lock\n");

    let (_, report) = project.run(&project.settings());
    assert_eq!(
        report.discovered,
        report.included + report.excluded,
        "counts do not reconcile: {} discovered, {} included, {} excluded",
        report.discovered,
        report.included,
        report.excluded
    );
    assert!(report.directories_pruned > 0);
}

/// A build without the optional features has to say what it cannot do.
///
/// `cargo test --no-default-features` used to prove only that such a build compiles. Each
/// disabled feature has a hand-written arm whose whole job is to report rather than do
/// nothing quietly, and no test ever ran one of them.
mod degraded_builds {
    // Every test here is gated off in a build that has all the features, so in that
    // configuration the module is empty and the import unused.
    #[allow(unused_imports)]
    use super::*;

    #[test]
    #[cfg(not(feature = "clipboard"))]
    fn a_build_without_clipboard_support_says_so() {
        let project = Project::new();
        project.file("src/main.rs", "fn main() {}\n");
        let mut settings = project.settings();
        settings.destination = mahiron_ctx::config::Destination::Clipboard;

        let error = Engine::new(&settings)
            .run()
            .expect_err("expected a refusal");
        let message = error.to_string();
        assert!(
            message.contains("clipboard") && message.contains("--output"),
            "the refusal did not name the feature or an alternative: {message}"
        );
    }

    #[test]
    #[cfg(not(feature = "watch"))]
    fn a_build_without_watch_support_says_so() {
        let project = Project::new();
        project.file("src/main.rs", "fn main() {}\n");
        let settings = project.settings();

        let error = mahiron_ctx::cli::watch_loop(&settings).expect_err("expected a refusal");
        assert!(
            error.to_string().contains("watch"),
            "the refusal did not name the feature: {error}"
        );
    }

    #[test]
    #[cfg(not(feature = "mcp-sse"))]
    fn a_build_without_the_sse_transport_says_so() {
        let error = mahiron_ctx::mcp::serve(Settings::default(), Some("sse"), None)
            .expect_err("expected a refusal");
        let message = error.to_string();
        assert!(
            message.contains("SSE") && message.contains("stdio"),
            "the refusal did not name the feature or an alternative: {message}"
        );
    }
}
