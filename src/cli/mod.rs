//! The command-line consumer of the engine.
//!
//! Argument parsing and console reporting live here; every decision about what a run
//! actually does belongs to [`crate::engine`].

mod args;

pub use args::{parse_size, Cli};

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use crate::config::{ReportFormat, Settings};
use crate::delivery::{AlwaysConfirm, Confirm, TerminalConfirm};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::report::{
    render_console, NullProgress, Progress, RunReport, RunStatus, TerminalProgress,
};

/// Entry point shared by both binaries, so the full name and the short alias cannot drift.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(status) => ExitCode::from(status.exit_code()),
        Err(error) => {
            eprintln!("mahiron-ctx: {error}");
            ExitCode::from(RunStatus::Failure.exit_code())
        }
    }
}

fn dispatch(cli: &Cli) -> Result<RunStatus> {
    if cli.list_compression_languages {
        return list_languages();
    }

    let settings = cli.to_settings()?;

    if cli.mcp_server {
        return crate::mcp::serve(settings, cli.transport.as_deref(), cli.bind.as_deref());
    }

    if cli.watch {
        return watch_loop(&settings);
    }

    single_run(&settings)
}

fn list_languages() -> Result<RunStatus> {
    let settings = Settings::default();
    let engine = Engine::new(&settings);
    let languages = engine.compression_languages();
    if languages.is_empty() {
        println!("this build reduces no languages to structural signatures");
    } else {
        for language in languages {
            println!("{language}");
        }
    }
    Ok(RunStatus::Success)
}

/// Repeat a run whenever the sources change, where this build can watch them.
#[cfg(feature = "watch")]
pub fn watch_loop(settings: &Settings) -> Result<RunStatus> {
    crate::watch::run(settings, single_run)
}

/// Repeat a run whenever the sources change, where this build can watch them.
#[cfg(not(feature = "watch"))]
pub fn watch_loop(_settings: &Settings) -> Result<RunStatus> {
    Err(Error::config(
        "this build has no watch support; re-run without --watch",
    ))
}

/// One complete packaging run, reported the way the invocation asked for.
pub fn single_run(settings: &Settings) -> Result<RunStatus> {
    let indicator = if settings.reporting.progress {
        TerminalProgress::if_interactive()
    } else {
        None
    };
    let progress: &dyn Progress = match &indicator {
        Some(indicator) => indicator,
        None => &NullProgress,
    };

    // Not tied to the progress indicator. `TerminalConfirm` already declines when either
    // stream is not a terminal, so binding the two meant `--quiet` and `--no-progress`
    // silently stopped asking about an existing destination and failed outright instead.
    let confirm: &dyn Confirm = if settings.overwrite {
        &AlwaysConfirm
    } else {
        &TerminalConfirm
    };

    let outcome = Engine::new(settings)
        .with_progress(progress)
        .with_confirmation(confirm)
        .run()?;

    if let Some(document) = &outcome.document {
        std::io::stdout().write_all(document)?;
    }

    emit_report(settings, &outcome.report)?;
    Ok(outcome.report.status())
}

fn emit_report(settings: &Settings, report: &RunReport) -> Result<()> {
    match settings.reporting.format {
        // Both branches go to the error stream, for the same reason: a document delivered
        // to standard output must stay free of anything that is not the document. The
        // JSON branch used to print to standard output, so `--stdout --report json`
        // interleaved the report with the document it was reporting on.
        ReportFormat::Json => {
            let rendered =
                serde_json::to_string_pretty(report).map_err(|e| Error::config(e.to_string()))?;
            writeln!(std::io::stderr(), "{rendered}")?;
        }
        ReportFormat::Console => {
            render_console(&mut std::io::stderr(), report, settings.reporting.verbosity)?
        }
    }
    Ok(())
}
