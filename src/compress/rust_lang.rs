use syn::visit_mut::{self, VisitMut};
use syn::{Block, ImplItemFn, ItemFn, TraitItemFn};

use super::{CompressionError, LanguageCompressor, Registration};

const LANGUAGE: &str = "rust";
const EXTENSIONS: &[&str] = &["rs"];

/// Line prefixed to every reduced file so its incompleteness is stated, not inferred.
const MARKER: &str = "// structural signatures only: implementation bodies elided\n\n";

struct RustCompressor;

impl LanguageCompressor for RustCompressor {
    fn language(&self) -> &'static str {
        LANGUAGE
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn compress(&self, source: &str) -> Result<String, CompressionError> {
        let mut file = syn::parse_file(source).map_err(|e| CompressionError::Parse {
            language: LANGUAGE,
            detail: e.to_string(),
        })?;
        BodyStripper.visit_file_mut(&mut file);
        Ok(format!("{MARKER}{}", prettyplease::unparse(&file)))
    }
}

struct BodyStripper;

fn empty_block() -> Block {
    Block {
        brace_token: Default::default(),
        stmts: Vec::new(),
    }
}

impl VisitMut for BodyStripper {
    fn visit_item_fn_mut(&mut self, item: &mut ItemFn) {
        visit_mut::visit_item_fn_mut(self, item);
        *item.block = empty_block();
    }

    fn visit_impl_item_fn_mut(&mut self, item: &mut ImplItemFn) {
        visit_mut::visit_impl_item_fn_mut(self, item);
        item.block = empty_block();
    }

    fn visit_trait_item_fn_mut(&mut self, item: &mut TraitItemFn) {
        visit_mut::visit_trait_item_fn_mut(self, item);
        // A trait method keeps its signature; dropping the default body leaves the
        // required trailing semicolon, which `default` being `None` implies.
        if item.default.take().is_some() {
            item.semi_token = Some(Default::default());
        }
    }
}

inventory::submit! {
    Registration {
        factory: || Box::new(RustCompressor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_removed_but_signatures_survive() {
        let source = r#"
            //! module docs
            pub struct Config { pub name: String }

            impl Config {
                pub fn new(name: String) -> Self {
                    let trimmed = name.trim().to_string();
                    Self { name: trimmed }
                }
            }

            pub fn run(config: &Config) -> Result<(), std::io::Error> {
                println!("{}", config.name);
                Ok(())
            }
        "#;
        let compressed = RustCompressor.compress(source).unwrap();
        assert!(compressed.starts_with(MARKER));
        assert!(compressed.contains("pub fn new(name: String) -> Self"));
        assert!(compressed.contains("pub fn run(config: &Config)"));
        assert!(!compressed.contains("trimmed"));
        assert!(!compressed.contains("println"));
    }

    #[test]
    fn unparsable_source_is_reported_rather_than_mangled() {
        let error = RustCompressor.compress("fn broken( {").unwrap_err();
        assert!(matches!(error, CompressionError::Parse { .. }));
    }

    #[test]
    fn compressed_output_is_itself_parsable() {
        let source = "pub trait T { fn a(&self) -> u8 { 1 } fn b(&self); }";
        let compressed = RustCompressor.compress(source).unwrap();
        syn::parse_file(&compressed).expect("reduced output should stay well-formed");
    }
}
