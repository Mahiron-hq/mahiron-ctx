use std::io::{self, Write};

use super::{DigestWriter, DocumentContext, FileEntry, FileTree, MeteredSink};
use crate::OUTPUT_SCHEMA_VERSION;

#[derive(Debug, Default)]
pub struct JsonWriter {
    wrote_first_file: bool,
}

/// Standard JSON string escaping, which needs no content-derived delimiter of its own.
///
/// Used for the short strings — paths, labels, framing text — where an allocation costs
/// nothing. File content goes through [`write_quoted`] instead.
fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Write an escaped JSON string straight to the sink.
///
/// `quote` would allocate a full copy of the content to build the `Value`, then a second
/// full copy to render it, per file. `to_writer` escapes into the sink as it goes, which
/// removes both copies and the memory spike they caused on a large file.
fn write_quoted(out: &mut MeteredSink<'_>, value: &str) -> io::Result<()> {
    serde_json::to_writer(out, value).map_err(io::Error::from)
}

impl DigestWriter for JsonWriter {
    fn begin(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        writeln!(out, "{{")?;
        writeln!(out, "  \"schemaVersion\": \"{OUTPUT_SCHEMA_VERSION}\",")?;
        writeln!(out, "  \"source\": {},", quote(&ctx.source_label))?;
        writeln!(out, "  \"format\": \"json\",")?;
        if let Some(header) = &ctx.header_text {
            writeln!(out, "  \"header\": {},", quote(header))?;
        }
        if ctx.include_preface {
            writeln!(out, "  \"preface\": {{")?;
            writeln!(out, "    \"discovered\": {},", ctx.discovered)?;
            writeln!(out, "    \"included\": {},", ctx.included)?;
            let transformations: Vec<String> =
                ctx.transformations.iter().map(|t| quote(t)).collect();
            writeln!(
                out,
                "    \"transformations\": [{}]",
                transformations.join(", ")
            )?;
            writeln!(out, "  }},")?;
        }
        Ok(())
    }

    fn tree(&mut self, out: &mut MeteredSink<'_>, tree: &FileTree) -> io::Result<()> {
        let rendered =
            serde_json::to_string_pretty(&tree.to_json()).unwrap_or_else(|_| "null".to_string());
        // Re-indented to sit inside the document rather than at the left margin.
        let indented = rendered.replace('\n', "\n  ");
        writeln!(out, "  \"tree\": {indented},")?;
        Ok(())
    }

    fn begin_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()> {
        write!(out, "  \"files\": [")?;
        Ok(())
    }

    fn end_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()> {
        if self.wrote_first_file {
            writeln!(out)?;
        }
        write!(out, "  ]")?;
        Ok(())
    }

    fn file(&mut self, out: &mut MeteredSink<'_>, entry: &FileEntry<'_>) -> io::Result<()> {
        if self.wrote_first_file {
            write!(out, ",")?;
        }
        writeln!(out)?;
        write!(out, "    {{")?;
        write!(out, "\"path\": {}", quote(entry.path))?;
        if entry.compressed {
            write!(out, ", \"compressed\": true")?;
        }
        write!(out, ", \"content\": ")?;
        write_quoted(out, entry.content)?;
        write!(out, "}}")?;
        self.wrote_first_file = true;
        Ok(())
    }

    fn finish(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        if let Some(footer) = &ctx.footer_text {
            write!(out, ",\n  \"footer\": {}", quote(footer))?;
        }
        writeln!(out)?;
        writeln!(out, "}}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OutputFormat, TokenEncoding, TreeStyle};

    fn context() -> DocumentContext {
        DocumentContext {
            source_label: "./my-project".into(),
            format: OutputFormat::Json,
            discovered: 2,
            included: 2,
            transformations: Vec::new(),
            include_preface: true,
            header_text: None,
            footer_text: None,
            tree_style: TreeStyle::Ascii,
        }
    }

    fn render(files: &[(&str, &str)]) -> String {
        let mut buffer: Vec<u8> = Vec::new();
        let ctx = context();
        {
            let mut sink = MeteredSink::new(Box::new(&mut buffer), None, TokenEncoding::Cl100kBase);
            let mut writer = JsonWriter::default();
            writer.begin(&mut sink, &ctx).unwrap();
            writer.begin_files(&mut sink).unwrap();
            for (path, content) in files {
                writer
                    .file(
                        &mut sink,
                        &FileEntry {
                            path,
                            content,
                            compressed: false,
                        },
                    )
                    .unwrap();
            }
            writer.end_files(&mut sink).unwrap();
            writer.finish(&mut sink, &ctx).unwrap();
            sink.finish().unwrap();
        }
        String::from_utf8(buffer).unwrap()
    }

    #[test]
    fn adversarial_content_stays_parsable() {
        let document = render(&[
            ("a.txt", "quotes \" backslash \\ newline \n tab \t"),
            ("b.txt", "```\n]]>\n\u{1f600}"),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert_eq!(
            parsed["files"][0]["content"],
            "quotes \" backslash \\ newline \n tab \t"
        );
        assert_eq!(parsed["files"][1]["content"], "```\n]]>\n\u{1f600}");
    }

    #[test]
    fn empty_file_list_is_still_valid() {
        let document = render(&[]);
        let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert!(parsed["files"].as_array().unwrap().is_empty());
    }
}
