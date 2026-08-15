use std::io::{self, Write};

use super::{DigestWriter, DocumentContext, FileEntry, FileTree, MeteredSink};
use crate::OUTPUT_SCHEMA_VERSION;

#[derive(Debug, Default)]
pub struct XmlWriter {
    files_section_opened: bool,
}

/// Escape the characters that would otherwise terminate or reinterpret an attribute value.
pub(crate) fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Split every `]]>` across two adjacent CDATA sections.
///
/// The terminator is emitted as `]]` in the first section and `>` in the second, so the
/// intact sequence never appears inside a section while the reassembled text is unchanged.
pub fn cdata_sections(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + 16);
    out.push_str("<![CDATA[");
    let mut rest = content;
    while let Some(index) = rest.find("]]>") {
        out.push_str(&rest[..index]);
        out.push_str("]]]]><![CDATA[>");
        rest = &rest[index + 3..];
    }
    out.push_str(rest);
    out.push_str("]]>");
    out
}

impl DigestWriter for XmlWriter {
    fn begin(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
        writeln!(
            out,
            "<digest source=\"{}\" format=\"xml\" schemaVersion=\"{}\">",
            escape_attribute(&ctx.source_label),
            OUTPUT_SCHEMA_VERSION
        )?;
        if let Some(header) = &ctx.header_text {
            writeln!(out, "  <header>{}</header>", cdata_sections(header))?;
        }
        if ctx.include_preface {
            writeln!(out, "  <preface>")?;
            writeln!(out, "    <discovered>{}</discovered>", ctx.discovered)?;
            writeln!(out, "    <included>{}</included>", ctx.included)?;
            for transformation in &ctx.transformations {
                writeln!(
                    out,
                    "    <transformation>{}</transformation>",
                    escape_attribute(transformation)
                )?;
            }
            writeln!(out, "  </preface>")?;
        }
        Ok(())
    }

    fn tree(&mut self, out: &mut MeteredSink<'_>, tree: &FileTree) -> io::Result<()> {
        if tree.is_empty() {
            return Ok(());
        }
        writeln!(out, "  <structure>")?;
        out.write_all(tree.render_xml(4).as_bytes())?;
        writeln!(out, "  </structure>")?;
        Ok(())
    }

    fn begin_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()> {
        writeln!(out, "  <files>")?;
        self.files_section_opened = true;
        Ok(())
    }

    fn end_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()> {
        if self.files_section_opened {
            writeln!(out, "  </files>")?;
            self.files_section_opened = false;
        }
        Ok(())
    }

    fn file(&mut self, out: &mut MeteredSink<'_>, entry: &FileEntry<'_>) -> io::Result<()> {
        let compressed = if entry.compressed {
            " compressed=\"true\""
        } else {
            ""
        };
        write!(
            out,
            "    <file path=\"{}\"{compressed}>",
            escape_attribute(entry.path)
        )?;
        out.write_all(cdata_sections(entry.content).as_bytes())?;
        writeln!(out, "</file>")?;
        Ok(())
    }

    fn finish(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        if let Some(footer) = &ctx.footer_text {
            writeln!(out, "  <footer>{}</footer>", cdata_sections(footer))?;
        }
        writeln!(out, "</digest>")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenated payload of every CDATA section, which must equal the original content.
    fn reassemble(markup: &str) -> String {
        let mut out = String::new();
        let mut rest = markup;
        while let Some(start) = rest.find("<![CDATA[") {
            rest = &rest[start + 9..];
            let end = rest.find("]]>").expect("unterminated CDATA section");
            out.push_str(&rest[..end]);
            rest = &rest[end + 3..];
        }
        out
    }

    #[test]
    fn plain_content_round_trips() {
        let content = "fn main() {}\n";
        assert_eq!(reassemble(&cdata_sections(content)), content);
    }

    #[test]
    fn terminator_never_appears_inside_a_section() {
        let content = "before ]]> after";
        let markup = cdata_sections(content);
        assert_eq!(reassemble(&markup), content);
    }

    #[test]
    fn adjacent_and_overlapping_terminators_round_trip() {
        for content in ["]]>]]>", "]]]>", "]]]]>>", "]]>", "]]", "]"] {
            assert_eq!(reassemble(&cdata_sections(content)), content, "{content}");
        }
    }
}
