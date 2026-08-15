//! Structural-signature compression.
//!
//! Every language plugs in through [`LanguageCompressor`] and registers itself from its
//! own module; nothing in the engine, and nothing shared between languages, names any
//! individual language, so support for another one is an added file rather than an edit
//! to logic every existing language already depends on.

#[cfg(feature = "compress-rust")]
mod rust_lang;

use std::path::Path;

use crate::config::CompressionRequest;

#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    #[error("could not parse as {language}: {detail}")]
    Parse {
        language: &'static str,
        detail: String,
    },
}

/// Structural reduction of one language's source text to its declarations.
///
/// Implementations must reduce content only in ways whose incompleteness is visible on
/// the face of the result; anything that could pass for a complete file is a defect.
pub trait LanguageCompressor: Send + Sync {
    /// Stable identifier used on the command line and in reporting.
    fn language(&self) -> &'static str;

    /// Extensions this implementation claims, lowercase and without a leading dot.
    fn extensions(&self) -> &'static [&'static str];

    fn compress(&self, source: &str) -> Result<String, CompressionError>;
}

/// Registration submitted by each language module and collected at link time.
pub struct Registration {
    pub factory: fn() -> Box<dyn LanguageCompressor>,
}

inventory::collect!(Registration);

/// The set of compressors linked into this build.
pub struct Registry {
    compressors: Vec<Box<dyn LanguageCompressor>>,
}

impl Registry {
    pub fn load() -> Self {
        let mut compressors: Vec<Box<dyn LanguageCompressor>> = inventory::iter::<Registration>
            .into_iter()
            .map(|registration| (registration.factory)())
            .collect();
        compressors.sort_by_key(|c| c.language());
        Self { compressors }
    }

    pub fn languages(&self) -> Vec<&'static str> {
        self.compressors.iter().map(|c| c.language()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.compressors.is_empty()
    }

    fn by_extension(&self, extension: &str) -> Option<&dyn LanguageCompressor> {
        self.compressors
            .iter()
            .find(|c| c.extensions().contains(&extension))
            .map(|c| c.as_ref())
    }

    /// The compressor that should handle `path`, honouring the languages the user named.
    pub fn resolve(
        &self,
        path: &Path,
        request: &CompressionRequest,
    ) -> Option<&dyn LanguageCompressor> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        let compressor = self.by_extension(&extension)?;
        match request {
            CompressionRequest::Disabled => None,
            CompressionRequest::AllSupported => Some(compressor),
            CompressionRequest::Languages(languages) => languages
                .iter()
                .any(|l| l.eq_ignore_ascii_case(compressor.language()))
                .then_some(compressor),
        }
    }

    /// Validate that every language the user named is actually linked in.
    pub fn unknown_languages(&self, request: &CompressionRequest) -> Vec<String> {
        match request {
            CompressionRequest::Languages(languages) => languages
                .iter()
                .filter(|requested| {
                    !self
                        .compressors
                        .iter()
                        .any(|c| c.language().eq_ignore_ascii_case(requested))
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrequested_languages_are_not_resolved() {
        let registry = Registry::load();
        let path = Path::new("src/main.rs");
        assert!(registry
            .resolve(path, &CompressionRequest::Disabled)
            .is_none());
        assert!(registry
            .resolve(path, &CompressionRequest::Languages(vec!["python".into()]))
            .is_none());
    }

    #[test]
    fn unknown_language_names_are_reported() {
        let registry = Registry::load();
        let unknown =
            registry.unknown_languages(&CompressionRequest::Languages(vec!["klingon".into()]));
        assert_eq!(unknown, vec!["klingon".to_string()]);
    }
}
