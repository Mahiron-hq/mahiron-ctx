//! Readers that recover packaged content from a document.
//!
//! Deliberately independent of the writers: a round-trip test proves nothing if both
//! directions share the code that might be wrong.

#![allow(dead_code)]

use std::collections::BTreeMap;

use mahiron_ctx::config::OutputFormat;

pub type Extracted = BTreeMap<String, String>;

/// Packaged paths in the order the document lists them.
///
/// [`extract`] returns a map, which sorts its keys and so cannot answer questions about
/// order at all — a test asking about sequence has to read the document itself.
pub fn extract_order(format: OutputFormat, document: &str) -> Vec<String> {
    match format {
        OutputFormat::Markdown => document
            .split_inclusive('\n')
            .filter_map(|line| {
                line.trim_end_matches(['\n', '\r'])
                    .strip_prefix("### ")
                    .map(str::to_string)
            })
            .collect(),
        OutputFormat::Text => document
            .split_inclusive('\n')
            .filter_map(|line| {
                line.trim_end_matches(['\n', '\r'])
                    .strip_prefix("FILE: ")
                    .map(str::to_string)
            })
            .collect(),
        OutputFormat::Xml => {
            let mut paths = Vec::new();
            let mut rest = document;
            while let Some(start) = rest.find("<file path=\"") {
                let after = &rest[start + "<file path=\"".len()..];
                let Some(quote) = after.find('"') else { break };
                paths.push(unescape_attribute(&after[..quote]));
                rest = &after[quote..];
            }
            paths
        }
        OutputFormat::Json => {
            let parsed: serde_json::Value =
                serde_json::from_str(document).expect("the document is not valid JSON");
            parsed["files"]
                .as_array()
                .expect("the document has no file array")
                .iter()
                .map(|entry| entry["path"].as_str().unwrap_or_default().to_string())
                .collect()
        }
    }
}

pub fn extract(format: OutputFormat, document: &str) -> Extracted {
    match format {
        OutputFormat::Markdown => from_markdown(document),
        OutputFormat::Text => from_text(document),
        OutputFormat::Xml => from_xml(document),
        OutputFormat::Json => from_json(document),
    }
}

/// Whether the format's delimiters force a newline the source file may not have had.
pub fn delimiters_require_final_newline(format: OutputFormat) -> bool {
    matches!(format, OutputFormat::Markdown | OutputFormat::Text)
}

fn from_markdown(document: &str) -> Extracted {
    let mut found = Extracted::new();
    let lines: Vec<&str> = document.split_inclusive('\n').collect();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index].trim_end_matches(['\n', '\r']);
        let Some(path) = line.strip_prefix("### ") else {
            index += 1;
            continue;
        };
        index += 1;

        // Skip the blank line, and the note a compressed file carries.
        while index < lines.len() && !is_fence(lines[index]) {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }
        let fence = fence_prefix(lines[index]);
        index += 1;

        let start = index;
        while index < lines.len() && lines[index].trim_end_matches(['\n', '\r']) != fence {
            index += 1;
        }
        let body: String = lines[start..index].concat();
        found.insert(path.to_string(), body);
        index += 1;
    }
    found
}

fn is_fence(line: &str) -> bool {
    line.starts_with("```")
}

fn fence_prefix(line: &str) -> String {
    line.chars().take_while(|c| *c == '`').collect()
}

fn from_text(document: &str) -> Extracted {
    let mut found = Extracted::new();
    let lines: Vec<&str> = document.split_inclusive('\n').collect();
    let mut index = 0;

    while index < lines.len() {
        if !is_separator(lines[index]) {
            index += 1;
            continue;
        }
        let separator = lines[index].trim_end_matches(['\n', '\r']).to_string();
        let Some(header) = lines.get(index + 1) else {
            break;
        };
        let Some(path) = header.trim_end_matches(['\n', '\r']).strip_prefix("FILE: ") else {
            index += 1;
            continue;
        };

        let mut cursor = index + 2;
        while cursor < lines.len() && lines[cursor].trim_end_matches(['\n', '\r']) != separator {
            cursor += 1;
        }
        cursor += 1;

        let start = cursor;
        while cursor < lines.len() && !starts_next_section(&lines, cursor) {
            cursor += 1;
        }
        found.insert(path.to_string(), lines[start..cursor].concat());
        index = cursor;
    }
    found
}

fn is_separator(line: &str) -> bool {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    trimmed.len() >= 72 && trimmed.chars().all(|c| c == '=')
}

fn starts_next_section(lines: &[&str], index: usize) -> bool {
    is_separator(lines[index])
        && lines
            .get(index + 1)
            .is_some_and(|line| line.starts_with("FILE: "))
}

fn from_xml(document: &str) -> Extracted {
    let mut found = Extracted::new();
    let mut rest = document;

    while let Some(start) = rest.find("<file path=\"") {
        let after = &rest[start + "<file path=\"".len()..];
        let Some(quote) = after.find('"') else { break };
        let path = unescape_attribute(&after[..quote]);
        let Some(open) = after.find('>') else { break };
        let body_start = open + 1;
        let Some(close) = after[body_start..].find("</file>") else {
            break;
        };
        let body = &after[body_start..body_start + close];
        found.insert(path, join_cdata(body));
        rest = &after[body_start + close..];
    }
    found
}

/// Concatenating the payloads of adjacent sections is what a conforming XML reader does,
/// and is exactly what makes a split terminator whole again.
fn join_cdata(body: &str) -> String {
    let mut joined = String::new();
    let mut rest = body;
    while let Some(start) = rest.find("<![CDATA[") {
        let after = &rest[start + "<![CDATA[".len()..];
        let Some(end) = after.find("]]>") else { break };
        joined.push_str(&after[..end]);
        rest = &after[end + "]]>".len()..];
    }
    joined
}

fn unescape_attribute(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn from_json(document: &str) -> Extracted {
    let parsed: serde_json::Value =
        serde_json::from_str(document).expect("the document is not valid JSON");
    parsed["files"]
        .as_array()
        .expect("the document has no file array")
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap_or_default().to_string(),
                entry["content"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}
