use std::io::{self, Write};

use super::{longest_run, DigestWriter, DocumentContext, FileEntry, FileTree, MeteredSink};
use crate::config::TreeStyle;

/// Baseline separator width, long enough that ordinary content never reaches it.
const MINIMUM_SEPARATOR: usize = 72;

#[derive(Debug, Default)]
pub struct TextWriter {
    style: TreeStyle,
}

/// Separator line no run of `=` inside the content can reproduce.
fn separator_for(content: &str) -> String {
    let width = (longest_run(content, '=') + 1).max(MINIMUM_SEPARATOR);
    "=".repeat(width)
}

impl DigestWriter for TextWriter {
    fn begin(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        self.style = ctx.tree_style;
        if let Some(header) = &ctx.header_text {
            out.write_all(header.as_bytes())?;
            if !header.ends_with('\n') {
                out.write_all(b"\n")?;
            }
            writeln!(out)?;
        }
        if ctx.include_preface {
            writeln!(out, "PROJECT DIGEST")?;
            writeln!(out, "source: {}", ctx.source_label)?;
            writeln!(
                out,
                "files: {} included of {} discovered",
                ctx.included, ctx.discovered
            )?;
            writeln!(out, "format: {}", ctx.format.label())?;
            if !ctx.transformations.is_empty() {
                writeln!(out, "transformations: {}", ctx.transformations.join("; "))?;
            }
            writeln!(out)?;
        }
        Ok(())
    }

    fn tree(&mut self, out: &mut MeteredSink<'_>, tree: &FileTree) -> io::Result<()> {
        if tree.is_empty() {
            return Ok(());
        }
        let rendered = match self.style {
            TreeStyle::Ascii => tree.render_ascii(),
            TreeStyle::Compact => tree.render_compact(),
        };
        writeln!(out, "STRUCTURE")?;
        out.write_all(rendered.as_bytes())?;
        writeln!(out)?;
        Ok(())
    }

    fn begin_files(&mut self, _out: &mut MeteredSink<'_>) -> io::Result<()> {
        Ok(())
    }

    fn end_files(&mut self, _out: &mut MeteredSink<'_>) -> io::Result<()> {
        Ok(())
    }

    fn file(&mut self, out: &mut MeteredSink<'_>, entry: &FileEntry<'_>) -> io::Result<()> {
        let separator = separator_for(entry.content);
        writeln!(out, "{separator}")?;
        writeln!(out, "FILE: {}", entry.path)?;
        if entry.compressed {
            writeln!(out, "NOTE: structural signatures only")?;
        }
        writeln!(out, "{separator}")?;
        out.write_all(entry.content.as_bytes())?;
        if !entry.content.is_empty() && !entry.content.ends_with('\n') {
            out.write_all(b"\n")?;
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        if let Some(footer) = &ctx.footer_text {
            writeln!(out)?;
            out.write_all(footer.as_bytes())?;
            if !footer.ends_with('\n') {
                out.write_all(b"\n")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_outgrows_content() {
        assert_eq!(separator_for("plain").len(), MINIMUM_SEPARATOR);
        let heavy = "=".repeat(MINIMUM_SEPARATOR + 4);
        assert_eq!(separator_for(&heavy).len(), MINIMUM_SEPARATOR + 5);
    }
}
