use std::io::{self, Write};

use super::language;
use super::{longest_run, DigestWriter, DocumentContext, FileEntry, FileTree, MeteredSink};
use crate::config::TreeStyle;

/// Shortest fence markdown permits, before any content-derived widening.
const MINIMUM_FENCE: usize = 3;

#[derive(Debug, Default)]
pub struct MarkdownWriter {
    files_section_opened: bool,
    style: TreeStyle,
}

/// Fence long enough that no run of backticks inside the content can close it early.
pub fn fence_for(content: &str) -> String {
    let width = (longest_run(content, '`') + 1).max(MINIMUM_FENCE);
    "`".repeat(width)
}

fn write_fenced(out: &mut MeteredSink<'_>, content: &str, info: &str) -> io::Result<()> {
    let fence = fence_for(content);
    writeln!(out, "{fence}{info}")?;
    out.write_all(content.as_bytes())?;
    // The newline before the closing fence belongs to the delimiter, not to the file:
    // content that already ends in one must not receive a second.
    if !content.is_empty() && !content.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    writeln!(out, "{fence}")?;
    Ok(())
}

impl DigestWriter for MarkdownWriter {
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
            writeln!(out, "# Project Digest")?;
            writeln!(out)?;
            writeln!(out, "Source: {}", ctx.source_label)?;
            writeln!(
                out,
                "Files included: {} of {} discovered",
                ctx.included, ctx.discovered
            )?;
            writeln!(out, "Format: {}", ctx.format.label())?;
            if !ctx.transformations.is_empty() {
                writeln!(out, "Transformations: {}", ctx.transformations.join("; "))?;
            }
            writeln!(out)?;
        }
        Ok(())
    }

    fn tree(&mut self, out: &mut MeteredSink<'_>, tree: &FileTree) -> io::Result<()> {
        if tree.is_empty() {
            return Ok(());
        }
        writeln!(out, "## Structure")?;
        writeln!(out)?;
        let rendered = match self.style {
            TreeStyle::Ascii => tree.render_ascii(),
            TreeStyle::Compact => tree.render_compact(),
        };
        write_fenced(out, &rendered, "text")?;
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
        if !self.files_section_opened {
            writeln!(out, "## Files")?;
            writeln!(out)?;
            self.files_section_opened = true;
        }
        writeln!(out, "### {}", entry.path)?;
        writeln!(out)?;
        if entry.compressed {
            writeln!(
                out,
                "_Structural signatures only; implementation bodies elided._"
            )?;
            writeln!(out)?;
        }
        write_fenced(out, entry.content, language::identifier(entry.path))?;
        writeln!(out)?;
        Ok(())
    }

    fn finish(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()> {
        if let Some(footer) = &ctx.footer_text {
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
    fn fence_widens_past_the_longest_run() {
        assert_eq!(fence_for("no ticks"), "```");
        assert_eq!(fence_for("``"), "```");
        assert_eq!(fence_for("```"), "````");
        assert_eq!(fence_for("a\n`````\nb"), "``````");
    }
}
