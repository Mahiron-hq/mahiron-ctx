mod json;
pub mod language;
pub mod markdown;
mod text;
mod tree;
pub mod xml;

pub use tree::{FileTree, TreeEntry};

use std::io::{self, Write};

use crate::config::{OutputFormat, TokenEncoding, TreeStyle};
use crate::report::OutputStats;
use crate::tokens::{TokenCounter, Utf8Assembler};

/// Everything the document preface may report about a run.
#[derive(Debug, Clone)]
pub struct DocumentContext {
    pub source_label: String,
    pub format: OutputFormat,
    pub discovered: usize,
    pub included: usize,
    pub transformations: Vec<String>,
    pub include_preface: bool,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
    pub tree_style: TreeStyle,
}

/// One file's contribution to the document.
#[derive(Debug, Clone, Copy)]
pub struct FileEntry<'a> {
    pub path: &'a str,
    pub content: &'a str,
    /// Set when the content is a structural signature rather than the file in full.
    pub compressed: bool,
}

/// Format-specific composition of the output document.
///
/// Every method writes straight through to the destination; no implementation may retain
/// the assembled document, since a run's memory use must not scale with the source's size.
pub trait DigestWriter {
    fn begin(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()>;
    fn tree(&mut self, out: &mut MeteredSink<'_>, tree: &FileTree) -> io::Result<()>;
    fn begin_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()>;
    fn file(&mut self, out: &mut MeteredSink<'_>, entry: &FileEntry<'_>) -> io::Result<()>;
    fn end_files(&mut self, out: &mut MeteredSink<'_>) -> io::Result<()>;
    fn finish(&mut self, out: &mut MeteredSink<'_>, ctx: &DocumentContext) -> io::Result<()>;
}

pub fn writer_for(format: OutputFormat) -> Box<dyn DigestWriter> {
    match format {
        OutputFormat::Markdown => Box::new(markdown::MarkdownWriter::default()),
        OutputFormat::Text => Box::new(text::TextWriter::default()),
        OutputFormat::Xml => Box::new(xml::XmlWriter::default()),
        OutputFormat::Json => Box::new(json::JsonWriter::default()),
    }
}

/// Destination wrapper that measures the document as it passes through.
///
/// Byte count, line count and token count are all derived from the exact bytes
/// delivered, so the reported figures describe the finished document rather than an
/// approximation of it taken earlier in the pipeline.
pub struct MeteredSink<'a> {
    inner: Box<dyn Write + 'a>,
    counter: Option<TokenCounter>,
    assembler: Utf8Assembler,
    encoding: TokenEncoding,
    bytes: u64,
    newlines: u64,
    last_byte: Option<u8>,
}

impl<'a> MeteredSink<'a> {
    pub fn new(
        inner: Box<dyn Write + 'a>,
        counter: Option<TokenCounter>,
        encoding: TokenEncoding,
    ) -> Self {
        Self {
            inner,
            counter,
            assembler: Utf8Assembler::default(),
            encoding,
            bytes: 0,
            newlines: 0,
            last_byte: None,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Line count under the convention that a final line without a terminator still counts.
    pub fn line_count(&self) -> u64 {
        match self.last_byte {
            None => 0,
            Some(b'\n') => self.newlines,
            Some(_) => self.newlines + 1,
        }
    }

    /// Flush the destination and settle the final statistics.
    pub fn finish(mut self) -> io::Result<OutputStats> {
        let bytes = self.bytes;
        let lines = self.line_count();
        self.inner.flush()?;
        let trailing = self.assembler.finish();
        let tokens = match self.counter.take() {
            Some(mut counter) => {
                if !trailing.is_empty() {
                    counter.push(&trailing);
                }
                counter.finish()
            }
            None => 0,
        };
        Ok(OutputStats {
            bytes,
            lines,
            tokens,
            token_encoding: self.encoding.label().to_string(),
        })
    }
}

impl Write for MeteredSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write_all(buf)?;
        self.bytes += buf.len() as u64;
        self.newlines += memchr::memchr_iter(b'\n', buf).count() as u64;
        if let Some(last) = buf.last() {
            self.last_byte = Some(*last);
        }
        if let Some(counter) = self.counter.as_mut() {
            let text = self.assembler.push(buf);
            if !text.is_empty() {
                counter.push(text.as_ref());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Longest unbroken run of `needle` anywhere in `haystack`.
pub(crate) fn longest_run(haystack: &str, needle: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in haystack.chars() {
        if ch == needle {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_run_finds_maximum() {
        assert_eq!(longest_run("a``b````c```", '`'), 4);
        assert_eq!(longest_run("no ticks", '`'), 0);
    }

    #[test]
    fn line_count_counts_unterminated_final_line() {
        let mut sink = MeteredSink::new(Box::new(Vec::new()), None, TokenEncoding::Cl100kBase);
        sink.write_all(b"one\ntwo").unwrap();
        assert_eq!(sink.line_count(), 2);
        sink.write_all(b"\n").unwrap();
        assert_eq!(sink.line_count(), 2);
    }
}
