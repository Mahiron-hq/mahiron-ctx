//! Throughput benchmark over a synthetic tree.
//!
//! Deliberately harness-free: the figure that matters is wall-clock time for a whole
//! run, which is what a user waits for, and comparing it across releases only requires
//! that the tree and the procedure stay the same.

use std::path::Path;
use std::time::{Duration, Instant};

use mahiron_ctx::config::{Destination, OutputFormat, Settings, SourceSpec};
use mahiron_ctx::engine::Engine;

const FILES: usize = 2_000;
const DIRECTORIES: usize = 40;
const LINES_PER_FILE: usize = 120;
const REPETITIONS: usize = 5;

fn main() {
    let workspace = tempfile::tempdir().expect("could not create the benchmark tree");
    let root = workspace.path().join("tree");
    let bytes = build_tree(&root);

    println!(
        "tree: {FILES} files across {DIRECTORIES} directories, {:.1} MiB",
        bytes as f64 / (1024.0 * 1024.0)
    );

    for format in [
        OutputFormat::Markdown,
        OutputFormat::Text,
        OutputFormat::Xml,
        OutputFormat::Json,
    ] {
        let destination = workspace
            .path()
            .join(format!("digest.{}", format.extension()));
        let mut settings = Settings {
            sources: vec![SourceSpec::Local(root.clone())],
            format,
            destination: Destination::File(destination),
            overwrite: true,
            ..Default::default()
        };
        settings.reporting.progress = false;

        let mut best = Duration::MAX;
        let mut produced = 0u64;
        for _ in 0..REPETITIONS {
            let started = Instant::now();
            let outcome = Engine::new(&settings).run().expect("the run failed");
            best = best.min(started.elapsed());
            produced = outcome.report.output.bytes;
        }

        println!(
            "{:>8}  {:>7.1} ms  {:>7.1} MiB/s  {} bytes produced",
            format.label(),
            best.as_secs_f64() * 1000.0,
            (bytes as f64 / (1024.0 * 1024.0)) / best.as_secs_f64(),
            produced
        );
    }
}

fn build_tree(root: &Path) -> u64 {
    let mut total = 0u64;
    for index in 0..FILES {
        let directory = root.join(format!("module{:02}", index % DIRECTORIES));
        std::fs::create_dir_all(&directory).expect("could not create a directory");

        let mut contents = String::with_capacity(LINES_PER_FILE * 48);
        for line in 0..LINES_PER_FILE {
            contents.push_str(&format!(
                "pub fn routine_{index}_{line}(value: u32) -> u32 {{ value + {line} }}\n"
            ));
        }
        total += contents.len() as u64;
        std::fs::write(directory.join(format!("file{index:04}.rs")), contents)
            .expect("could not write a file");
    }
    total
}
