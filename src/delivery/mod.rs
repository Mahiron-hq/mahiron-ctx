use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use tempfile::NamedTempFile;

use crate::config::{Destination, Settings};
use crate::error::{Error, Result};
use crate::report::{DeliveryReport, WarningRecord};

/// Document size above which clipboard delivery is confirmed before it is attempted.
///
/// Well below the sizes at which common clipboard implementations become unstable; the
/// operating system gives no advance warning of that failure, so the check happens here.
pub const CLIPBOARD_WARN_BYTES: u64 = 4 * 1024 * 1024;

/// Prefix every staging file carries.
///
/// Fixed rather than left to the temporary-file library's default, because discovery has
/// to recognise a staging file on sight: it sits in the destination's own directory,
/// which is usually inside the tree being packaged.
pub const STAGING_PREFIX: &str = ".mhrn-staging-";

/// Asks the user to approve an action that could lose data or destabilise the system.
pub trait Confirm {
    fn confirm(&self, prompt: &str) -> bool;
}

/// Declines everything, for callers that cannot ask: scripts, agents, pipelines.
pub struct NeverConfirm;

impl Confirm for NeverConfirm {
    fn confirm(&self, _prompt: &str) -> bool {
        false
    }
}

/// Approves everything, for a caller that already passed an explicit instruction.
pub struct AlwaysConfirm;

impl Confirm for AlwaysConfirm {
    fn confirm(&self, _prompt: &str) -> bool {
        true
    }
}

/// Prompts on the terminal, and declines when there is no terminal to prompt on.
pub struct TerminalConfirm;

impl Confirm for TerminalConfirm {
    fn confirm(&self, prompt: &str) -> bool {
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            return false;
        }
        eprint!("{prompt} [y/N] ");
        let _ = io::stderr().flush();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

enum Channel {
    /// Written beside the destination and moved into place once complete, so an
    /// interrupted run cannot leave a half-written document where a whole one was.
    File {
        staging: NamedTempFile,
        destination: PathBuf,
        /// Whether replacing an existing destination has already been permitted. When it
        /// has not, the move refuses to clobber a file that appeared in the meantime.
        may_replace: bool,
    },
    Stdout,
    Buffer {
        buffer: Vec<u8>,
        clipboard: bool,
    },
    Discard,
}

/// A destination that has been validated and opened, but not yet committed to.
pub struct Delivery {
    channel: Channel,
}

/// The directory a run's staging file will be created in, if there is one.
///
/// Discovery needs this before delivery opens, because the staging file must be
/// recognised and refused wherever the walk meets it.
pub fn staging_directory(settings: &Settings) -> Option<PathBuf> {
    if settings.dry_run {
        return None;
    }
    let Destination::File(path) = &settings.destination else {
        return None;
    };
    let directory = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(directory) => directory.to_path_buf(),
        None => PathBuf::from("."),
    };
    Some(std::fs::canonicalize(&directory).unwrap_or(directory))
}

/// Obtain permission to write the destination, without creating anything.
///
/// Separate from [`Delivery::open`] on purpose. The question has to be asked before any
/// work is done — refusing afterwards wastes all of it — but creating the staging file
/// that early put a file the tool is writing inside the tree the tool is about to walk.
/// Asking and creating are now two steps, and only the asking happens first.
pub fn guard_destination(settings: &Settings, confirm: &dyn Confirm) -> Result<bool> {
    if settings.dry_run {
        return Ok(true);
    }
    let Destination::File(path) = &settings.destination else {
        return Ok(true);
    };
    if settings.overwrite || !path.exists() {
        return Ok(settings.overwrite);
    }
    let prompt = format!("{} already exists. Overwrite it?", path.display());
    if confirm.confirm(&prompt) {
        return Ok(true);
    }
    Err(Error::RefuseOverwrite(path.to_path_buf()))
}

impl Delivery {
    /// Open the destination, having already obtained permission for it.
    pub fn open_permitted(settings: &Settings, may_replace: bool) -> Result<Self> {
        if settings.dry_run {
            return Ok(Self {
                channel: Channel::Discard,
            });
        }

        let channel = match &settings.destination {
            Destination::Stdout => Channel::Stdout,
            Destination::Clipboard => Channel::Buffer {
                buffer: Vec::new(),
                clipboard: true,
            },
            Destination::File(path) => {
                let directory = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    Some(directory) => {
                        std::fs::create_dir_all(directory).map_err(|e| Error::io(directory, e))?;
                        directory.to_path_buf()
                    }
                    None => PathBuf::from("."),
                };
                let staging = tempfile::Builder::new()
                    .prefix(STAGING_PREFIX)
                    .tempfile_in(&directory)
                    .map_err(|e| Error::io(path, e))?;
                // Registered so that a signal, which runs no destructors, does not leave
                // the staging file behind in the user's working directory.
                crate::cleanup::register_file(staging.path());
                Channel::File {
                    staging,
                    destination: path.clone(),
                    may_replace,
                }
            }
        };
        Ok(Self { channel })
    }

    /// Ask, then open. Retained for callers that do both at one point in time.
    pub fn open(settings: &Settings, confirm: &dyn Confirm) -> Result<Self> {
        let may_replace = guard_destination(settings, confirm)?;
        Self::open_permitted(settings, may_replace)
    }

    /// Delivery that keeps the document so a caller can hand it on itself.
    pub fn in_memory() -> Self {
        Self {
            channel: Channel::Buffer {
                buffer: Vec::new(),
                clipboard: false,
            },
        }
    }

    pub fn writer(&mut self) -> Box<dyn Write + '_> {
        match &mut self.channel {
            Channel::File { staging, .. } => Box::new(io::BufWriter::new(staging.as_file_mut())),
            Channel::Stdout => Box::new(io::BufWriter::new(io::stdout().lock())),
            Channel::Buffer { buffer, .. } => Box::new(buffer),
            Channel::Discard => Box::new(io::sink()),
        }
    }

    /// Finalise delivery, returning what happened and the document where it was retained.
    pub fn commit(self, bytes: u64, confirm: &dyn Confirm) -> Result<Committed> {
        match self.channel {
            Channel::Discard => Ok(Committed {
                report: DeliveryReport::DryRun,
                document: None,
                warnings: Vec::new(),
            }),
            Channel::Stdout => Ok(Committed {
                report: DeliveryReport::Stdout,
                document: None,
                warnings: Vec::new(),
            }),
            Channel::File {
                staging,
                destination,
                may_replace,
            } => {
                let staged_path = staging.path().to_path_buf();
                // A rename can reach the destination before the data it points at does.
                // The guarantee in this type's documentation — that an interrupted run
                // never leaves a half-written document where a whole one was — holds
                // against process death without this, but not against power loss.
                staging
                    .as_file()
                    .sync_all()
                    .map_err(|e| Error::io(&staged_path, e))?;

                // Whether the destination existed was decided before the run started; a
                // file that appeared in between was never approved for replacement, so
                // the move refuses rather than silently discarding it.
                let outcome = if may_replace {
                    staging.persist(&destination).map_err(|e| e.error)
                } else {
                    staging.persist_noclobber(&destination).map_err(|e| e.error)
                };
                match outcome {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        crate::cleanup::deregister(&staged_path);
                        return Err(Error::RefuseOverwrite(destination));
                    }
                    Err(error) => {
                        crate::cleanup::deregister(&staged_path);
                        return Err(Error::io(&destination, error));
                    }
                }
                crate::cleanup::deregister(&staged_path);
                Ok(Committed {
                    report: DeliveryReport::File {
                        path: destination.to_string_lossy().into_owned(),
                    },
                    document: None,
                    warnings: Vec::new(),
                })
            }
            Channel::Buffer { buffer, clipboard } => {
                if clipboard {
                    return deliver_to_clipboard(buffer, bytes, confirm);
                }
                // The document exists and is being handed back; calling that a dry run
                // told every agent using the inline MCP response that nothing had been
                // produced, in the same message that carried it.
                Ok(Committed {
                    report: DeliveryReport::Retained,
                    document: Some(buffer),
                    warnings: Vec::new(),
                })
            }
        }
    }
}

pub struct Committed {
    pub report: DeliveryReport,
    /// The document itself, where the destination was memory rather than the filesystem.
    pub document: Option<Vec<u8>>,
    pub warnings: Vec<WarningRecord>,
}

#[cfg(feature = "clipboard")]
fn deliver_to_clipboard(buffer: Vec<u8>, bytes: u64, confirm: &dyn Confirm) -> Result<Committed> {
    if bytes > CLIPBOARD_WARN_BYTES {
        let prompt = format!(
            "The document is {bytes} bytes, above the {CLIPBOARD_WARN_BYTES} byte threshold at \
             which clipboard delivery can destabilise the system clipboard. Continue?"
        );
        if !confirm.confirm(&prompt) {
            return Err(Error::Clipboard(format!(
                "document of {bytes} bytes exceeds the {CLIPBOARD_WARN_BYTES} byte clipboard \
                 threshold; write it to a file with --output, or re-run and confirm the prompt"
            )));
        }
    }

    let text = String::from_utf8(buffer)
        .map_err(|_| Error::Clipboard("document is not valid UTF-8".into()))?;

    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
        Ok(()) => Ok(Committed {
            report: DeliveryReport::Clipboard,
            document: None,
            warnings: Vec::new(),
        }),
        Err(error) => Err(Error::Clipboard(error.to_string())),
    }
}

#[cfg(not(feature = "clipboard"))]
fn deliver_to_clipboard(
    _buffer: Vec<u8>,
    _bytes: u64,
    _confirm: &dyn Confirm,
) -> Result<Committed> {
    Err(Error::Clipboard(
        "this build has no clipboard integration; use --output or --stdout instead".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Destination;

    #[test]
    fn an_existing_destination_is_protected_without_an_explicit_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mhrn-output.md");
        std::fs::write(&path, "existing").unwrap();

        let settings = Settings {
            destination: Destination::File(path.clone()),
            ..Default::default()
        };
        assert!(matches!(
            Delivery::open(&settings, &NeverConfirm),
            Err(Error::RefuseOverwrite(_))
        ));
        assert!(Delivery::open(&settings, &AlwaysConfirm).is_ok());
    }

    #[test]
    fn a_dry_run_never_touches_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mhrn-output.md");
        let settings = Settings {
            destination: Destination::File(path.clone()),
            dry_run: true,
            ..Default::default()
        };
        let mut delivery = Delivery::open(&settings, &NeverConfirm).unwrap();
        delivery.writer().write_all(b"content").unwrap();
        let committed = delivery.commit(7, &NeverConfirm).unwrap();
        assert!(matches!(committed.report, DeliveryReport::DryRun));
        assert!(!path.exists());
    }

    #[test]
    fn a_staging_file_is_recognisable_and_lives_beside_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mhrn-output.md");
        let settings = Settings {
            destination: Destination::File(path.clone()),
            ..Default::default()
        };
        let delivery = Delivery::open(&settings, &NeverConfirm).unwrap();
        let Channel::File { staging, .. } = &delivery.channel else {
            panic!("expected a file channel");
        };
        let name = staging
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(STAGING_PREFIX),
            "a staging file discovery cannot recognise: {name}"
        );
        assert!(crate::paths::same_option(
            staging.path().parent(),
            path.parent()
        ));
        // Compared through the helper: `staging_directory` canonicalises, and on Windows
        // that yields a verbatim path that is textually unequal to the one it came from.
        assert!(crate::paths::same_option(
            staging_directory(&settings).as_deref(),
            Some(dir.path())
        ));
    }

    #[test]
    fn a_destination_that_appears_after_the_check_is_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mhrn-output.md");
        let settings = Settings {
            destination: Destination::File(path.clone()),
            ..Default::default()
        };
        // Permission was granted for a destination that did not exist.
        let mut delivery = Delivery::open(&settings, &NeverConfirm).unwrap();
        delivery.writer().write_all(b"new").unwrap();
        // Something else created it in the meantime.
        std::fs::write(&path, "someone else was here").unwrap();

        assert!(matches!(
            delivery.commit(3, &NeverConfirm),
            Err(Error::RefuseOverwrite(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "someone else was here"
        );
    }

    #[test]
    fn a_retained_document_is_not_reported_as_a_dry_run() {
        let mut delivery = Delivery::in_memory();
        delivery.writer().write_all(b"document").unwrap();
        let committed = delivery.commit(8, &NeverConfirm).unwrap();
        assert!(matches!(committed.report, DeliveryReport::Retained));
        assert_eq!(committed.document.as_deref(), Some(&b"document"[..]));
    }

    #[test]
    fn file_delivery_replaces_the_destination_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mhrn-output.md");
        let settings = Settings {
            destination: Destination::File(path.clone()),
            overwrite: true,
            ..Default::default()
        };
        let mut delivery = Delivery::open(&settings, &AlwaysConfirm).unwrap();
        delivery.writer().write_all(b"document").unwrap();
        delivery.commit(8, &AlwaysConfirm).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "document");
    }
}
