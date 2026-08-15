use std::path::PathBuf;

use crate::config::{Settings, SourceSpec};
use crate::error::{Error, Result};
use crate::remote::{self, EphemeralCopy};
use crate::report::{RemoteOutcome, WarningKind, WarningRecord};

/// A source that is now a real directory or file on disk, whatever it started as.
#[derive(Debug)]
pub struct AcquiredSource {
    pub root: PathBuf,
    /// Whether the root is a single file rather than a directory. The traversal yields
    /// such a root as its only entry, at depth zero, which the walk would otherwise skip.
    pub is_file: bool,
    /// The designation exactly as the user gave it, used in reporting and the preface.
    pub label: String,
    /// Path segment distinguishing this source when several are packaged together.
    pub prefix: String,
    /// Whether ignore configuration found inside this source may govern filtering.
    pub honor_tool_ignore: bool,
    /// Retained so the copy outlives the run but no longer.
    _copy: Option<EphemeralCopy>,
}

#[derive(Debug, Default)]
pub struct Acquisition {
    pub sources: Vec<AcquiredSource>,
    pub warnings: Vec<WarningRecord>,
    pub remote: Option<RemoteOutcome>,
}

/// Turn every designated source into a local root, retrieving the one remote source if named.
pub fn acquire(settings: &Settings) -> Result<Acquisition> {
    let mut acquisition = Acquisition::default();

    for spec in &settings.sources {
        match spec {
            SourceSpec::Local(path) => {
                let canonical =
                    std::fs::canonicalize(path).map_err(|_| Error::SourceNotFound(path.clone()))?;
                let label = path.to_string_lossy().into_owned();
                acquisition.sources.push(AcquiredSource {
                    prefix: prefix_for(&canonical),
                    is_file: canonical.is_file(),
                    root: canonical,
                    label,
                    honor_tool_ignore: true,
                    _copy: None,
                });
            }
            SourceSpec::Remote(source) => {
                let trusted = settings.trusts_remote_config(&source.url);
                match remote::retrieve(source, settings.keep_remote_copy) {
                    Ok(retrieval) => {
                        let root = std::fs::canonicalize(retrieval.copy.path())
                            .unwrap_or_else(|_| retrieval.copy.path().to_path_buf());
                        acquisition.warnings.extend(retrieval.warnings);
                        if !trusted {
                            acquisition.warnings.push(WarningRecord::global(
                                WarningKind::UntrustedRemoteConfigIgnored,
                            ));
                        }
                        acquisition.remote = Some(RemoteOutcome::Succeeded {
                            designation: source.url.clone(),
                            reference: source.reference.clone(),
                            retained_at: retrieval
                                .copy
                                .retained_path()
                                .map(|p| p.to_string_lossy().into_owned()),
                        });
                        acquisition.sources.push(AcquiredSource {
                            prefix: prefix_for(&root),
                            is_file: false,
                            root,
                            label: source.url.clone(),
                            // A remote source's own configuration is not the user's until
                            // they say so for that specific source.
                            honor_tool_ignore: trusted,
                            _copy: Some(retrieval.copy),
                        });
                    }
                    Err(error) => {
                        acquisition.remote = Some(RemoteOutcome::Failed {
                            designation: source.url.clone(),
                            detail: error.to_string(),
                        });
                        return Err(error);
                    }
                }
            }
        }
    }

    if acquisition.sources.is_empty() {
        return Err(Error::config("no usable source was designated"));
    }
    disambiguate_prefixes(&mut acquisition.sources);
    Ok(acquisition)
}

/// Label shown at the root of the structural overview and used to disambiguate sources.
pub fn root_label(sources: &[AcquiredSource]) -> String {
    match sources {
        [single] => single.label.clone(),
        many => many
            .iter()
            .map(|source| source.label.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Give every source a prefix no other source shares.
///
/// The prefix is the leading path segment of every display path a source contributes, so
/// two sources reduced to the same prefix — `mhrn a/src b/src` — would produce two file
/// sections with identical paths, one overwriting the other in the structural overview
/// and the per-file records attributed to whichever sorted first. Colliding prefixes take
/// one more parent segment each until they differ.
fn disambiguate_prefixes(sources: &mut [AcquiredSource]) {
    let mut depth = 1;
    loop {
        let colliding: Vec<usize> = {
            let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            let mut colliding = Vec::new();
            for (index, source) in sources.iter().enumerate() {
                if let Some(first) = seen.insert(source.prefix.as_str(), index) {
                    colliding.push(first);
                    colliding.push(index);
                }
            }
            colliding
        };
        if colliding.is_empty() || depth > 8 {
            break;
        }
        depth += 1;
        for index in colliding {
            sources[index].prefix = trailing_segments(&sources[index].root, depth);
        }
    }

    // Deeper paths can still coincide (two roots reached through different links, say);
    // an ordinal suffix is the last resort and is at least unambiguous.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, source) in sources.iter_mut().enumerate() {
        if !seen.insert(source.prefix.clone()) {
            source.prefix = format!("{}~{}", source.prefix, index + 1);
            seen.insert(source.prefix.clone());
        }
    }
}

/// The last `count` path segments, joined with forward slashes.
fn trailing_segments(root: &std::path::Path, count: usize) -> String {
    let segments: Vec<String> = root
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let start = segments.len().saturating_sub(count);
    let joined = segments[start..].join("/");
    if joined.is_empty() {
        "source".to_string()
    } else {
        joined
    }
}

/// Name of the directory a source points at, used as its path prefix.
pub fn prefix_for(root: &std::path::Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn prefixes_come_from_the_directory_name() {
        assert_eq!(prefix_for(Path::new("/tmp/my-project")), "my-project");
    }

    #[test]
    fn sources_reduced_to_the_same_name_are_told_apart() {
        let mut sources = vec![
            AcquiredSource {
                root: PathBuf::from("/tmp/alpha/src"),
                is_file: false,
                label: "alpha/src".into(),
                prefix: "src".into(),
                honor_tool_ignore: true,
                _copy: None,
            },
            AcquiredSource {
                root: PathBuf::from("/tmp/beta/src"),
                is_file: false,
                label: "beta/src".into(),
                prefix: "src".into(),
                honor_tool_ignore: true,
                _copy: None,
            },
        ];
        disambiguate_prefixes(&mut sources);
        assert_eq!(sources[0].prefix, "alpha/src");
        assert_eq!(sources[1].prefix, "beta/src");
    }

    #[test]
    fn a_missing_local_source_is_reported_as_such() {
        let settings = Settings {
            sources: vec![SourceSpec::Local(PathBuf::from(
                "/definitely/not/here/mahiron",
            ))],
            ..Default::default()
        };
        assert!(matches!(acquire(&settings), Err(Error::SourceNotFound(_))));
    }
}
