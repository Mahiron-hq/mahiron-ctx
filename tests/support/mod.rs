//! The fidelity corpus.
//!
//! Every case is written here as bytes rather than as a checked-in file, so that no
//! editor, archive tool or version-control setting can quietly repair one of them
//! before a test reads it.

#![allow(dead_code)]

pub mod extract;

use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub struct Case {
    /// Path relative to the corpus root.
    pub path: &'static str,
    pub bytes: &'static [u8],
    /// The exact text a faithful packaging run must reproduce for this case, where the
    /// case is meant to be readable at all.
    pub expected_text: Option<&'static str>,
    pub note: &'static str,
}

pub const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
pub const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
pub const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];

/// Cases whose content must survive packaging byte for byte once decoded.
pub fn cases() -> Vec<Case> {
    vec![
        Case {
            path: "empty.txt",
            bytes: b"",
            expected_text: Some(""),
            note: "an empty file is a file, not an absence",
        },
        Case {
            path: "no-trailing-newline.txt",
            bytes: b"last line without a newline",
            expected_text: Some("last line without a newline"),
            note: "a missing final newline must not be supplied",
        },
        Case {
            path: "trailing-blank-lines.txt",
            bytes: b"content\n\n\n\n",
            expected_text: Some("content\n\n\n\n"),
            note: "trailing blank lines are content until asked otherwise",
        },
        Case {
            path: "crlf.txt",
            bytes: b"first\r\nsecond\r\nthird\r\n",
            expected_text: Some("first\r\nsecond\r\nthird\r\n"),
            note: "carriage returns are preserved",
        },
        Case {
            path: "mixed-endings.txt",
            bytes: b"lf\ncrlf\r\ncr\rlf\n",
            expected_text: Some("lf\ncrlf\r\ncr\rlf\n"),
            note: "a file may mix all three conventions",
        },
        Case {
            path: "utf8-bom.txt",
            bytes: b"\xEF\xBB\xBFcontent after a byte order mark\n",
            expected_text: Some("content after a byte order mark\n"),
            note: "the mark is removed and never re-emitted",
        },
        Case {
            path: "utf16le-bom.txt",
            bytes: &[0xFF, 0xFE, 0x68, 0x00, 0x69, 0x00, 0x0A, 0x00],
            expected_text: Some("hi\n"),
            note: "a legacy encoding is decoded, and its mark removed",
        },
        Case {
            path: "utf16be-bom.txt",
            bytes: &[0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69, 0x00, 0x0A],
            expected_text: Some("hi\n"),
            note: "byte order is detected rather than assumed",
        },
        Case {
            path: "indentation.txt",
            bytes: b"\ttab\n    four spaces\n \tmixed\n\t \tmore\n",
            expected_text: Some("\ttab\n    four spaces\n \tmixed\n\t \tmore\n"),
            note: "indentation is content and is never normalised",
        },
        Case {
            path: "trailing-whitespace.txt",
            bytes: b"line with trailing spaces   \nline with a trailing tab\t\n",
            expected_text: Some("line with trailing spaces   \nline with a trailing tab\t\n"),
            note: "trailing whitespace survives unless removing it was requested",
        },
        Case {
            path: "fence-three.md",
            bytes: b"```\ncode\n```\n",
            expected_text: Some("```\ncode\n```\n"),
            note: "the commonest fence length must not close the section early",
        },
        Case {
            path: "fence-four.md",
            bytes: b"````\nnested\n````\n",
            expected_text: Some("````\nnested\n````\n"),
            note: "a longer run demands a longer fence still",
        },
        Case {
            path: "fence-boundary.md",
            bytes: b"text\n``````````\nten backticks\n``````````\n",
            expected_text: Some("text\n``````````\nten backticks\n``````````\n"),
            note: "the fence is derived from the longest run, not from a fixed table",
        },
        Case {
            path: "fence-inline.md",
            bytes: b"a ```` b ``` c\n",
            expected_text: Some("a ```` b ``` c\n"),
            note: "runs need not begin a line to matter",
        },
        Case {
            path: "cdata-terminator.xml",
            bytes: b"<node>]]></node>\n",
            expected_text: Some("<node>]]></node>\n"),
            note: "the section terminator appearing in content must be split around",
        },
        Case {
            path: "cdata-adjacent.xml",
            bytes: b"]]>]]>\n",
            expected_text: Some("]]>]]>\n"),
            note: "adjacent terminators must not merge into one another when split",
        },
        Case {
            path: "cdata-overlapping.xml",
            bytes: b"]]]]>>\n",
            expected_text: Some("]]]]>>\n"),
            note: "the naive replacement of this case produces a terminator of its own",
        },
        Case {
            path: "cdata-only.xml",
            bytes: b"]]>",
            expected_text: Some("]]>"),
            note: "a file consisting of nothing but a terminator",
        },
        Case {
            path: "json-escapes.json",
            bytes: b"{\"quote\":\"\\\"\",\"backslash\":\"\\\\\",\"newline\":\"\\n\"}\n",
            expected_text: Some(
                "{\"quote\":\"\\\"\",\"backslash\":\"\\\\\",\"newline\":\"\\n\"}\n",
            ),
            note: "content that is itself escaped must not be double-escaped",
        },
        Case {
            path: "control-characters.txt",
            bytes: b"bell \x07 escape \x1B end\n",
            expected_text: None,
            note: "control characters XML cannot represent are treated as binary",
        },
        Case {
            path: "unicode.txt",
            bytes: "\u{1F600} emoji, \u{4F60}\u{597D} han, \u{0301} combining\n".as_bytes(),
            expected_text: Some("\u{1F600} emoji, \u{4F60}\u{597D} han, \u{0301} combining\n"),
            note: "characters outside the basic plane survive intact",
        },
        Case {
            path: "latin1.txt",
            bytes: &[
                0x53, 0x6D, 0xF6, 0x72, 0x67, 0xE5, 0x73, 0x62, 0x6F, 0x72, 0x64, 0x0A,
            ],
            expected_text: None,
            note: "an encoding-ambiguous file is decoded by detection, not by assumption",
        },
        Case {
            path: "invalid-utf8.bin",
            bytes: &[0x00, 0x01, 0x02, 0xFF, 0xFE, 0x00, 0x03],
            expected_text: None,
            note: "an undecodable file is reported, never reproduced approximately",
        },
        Case {
            path: "combination.md",
            bytes: b"\xEF\xBB\xBF```````\r\n\t]]>  \r\n```````",
            expected_text: Some("```````\r\n\t]]>  \r\n```````"),
            note: "the hard cases combined into one file",
        },
    ]
}

/// Write the corpus into a fresh directory and return it.
pub fn materialise() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("could not create the corpus directory");
    let root = directory.path().join("corpus");
    std::fs::create_dir_all(&root).expect("could not create the corpus root");
    for case in cases() {
        write_case(&root, case.path, case.bytes);
    }
    (directory, root)
}

/// Write a corpus subset, for tests that need one property in isolation.
pub fn materialise_subset(paths: &[&str]) -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("could not create the corpus directory");
    let root = directory.path().join("corpus");
    std::fs::create_dir_all(&root).expect("could not create the corpus root");
    for case in cases().into_iter().filter(|c| paths.contains(&c.path)) {
        write_case(&root, case.path, case.bytes);
    }
    (directory, root)
}

fn write_case(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("could not create a corpus directory");
    }
    std::fs::write(&path, bytes).expect("could not write a corpus case");
}

/// The cases whose content a faithful run must reproduce exactly.
pub fn readable_cases() -> Vec<Case> {
    cases()
        .into_iter()
        .filter(|case| case.expected_text.is_some())
        .collect()
}
