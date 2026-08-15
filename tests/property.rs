//! Property tests for the two delimiter rules that fidelity depends on.
//!
//! Both rules are about content that contains the delimiter the format uses to end it,
//! which is precisely the case a fixed delimiter gets wrong.

mod support;

use proptest::prelude::*;

use mahiron_ctx::output::markdown::fence_for;
use mahiron_ctx::output::xml::cdata_sections;

use support::extract::extract;

/// Content built from the characters that participate in either rule, so the generator
/// spends its budget on the cases that can actually break.
fn hostile_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("`".to_string()),
            Just("]".to_string()),
            Just(">".to_string()),
            Just("]]>".to_string()),
            Just("\n".to_string()),
            Just("\r\n".to_string()),
            Just(" ".to_string()),
            Just("a".to_string()),
            Just("<![CDATA[".to_string()),
        ],
        0..64,
    )
    .prop_map(|parts| parts.concat())
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

proptest! {
    #[test]
    fn a_fence_is_never_closed_by_the_content_it_encloses(text in hostile_text()) {
        let fence = fence_for(&text);
        prop_assert!(fence.len() >= 3);
        prop_assert!(fence.len() > longest_backtick_run(&text));
        prop_assert!(!text.contains(&fence));
    }

    #[test]
    fn fenced_content_is_recovered_exactly(text in hostile_text()) {
        let fence = fence_for(&text);
        let mut document = String::from("## Files\n\n### f.txt\n\n");
        document.push_str(&fence);
        document.push('\n');
        document.push_str(&text);
        if !text.is_empty() && !text.ends_with('\n') {
            document.push('\n');
        }
        document.push_str(&fence);
        document.push('\n');

        let extracted = extract(mahiron_ctx::config::OutputFormat::Markdown, &document);
        let recovered = extracted.get("f.txt").expect("the section was not found");
        let expected = if text.is_empty() || text.ends_with('\n') {
            text.clone()
        } else {
            format!("{text}\n")
        };
        prop_assert_eq!(recovered, &expected);
    }

    #[test]
    fn no_section_carries_its_own_terminator(text in hostile_text()) {
        let sections = cdata_sections(&text);
        for payload in payloads(&sections) {
            prop_assert!(!payload.contains("]]>"));
        }
    }

    #[test]
    fn cdata_payloads_rejoin_into_the_original(text in hostile_text()) {
        let sections = cdata_sections(&text);
        let rejoined: String = payloads(&sections).concat();
        prop_assert_eq!(rejoined, text);
    }

    #[test]
    fn a_whole_xml_document_round_trips(text in hostile_text()) {
        let document = format!(
            "<digest schemaVersion=\"1.0\">\n  <files>\n    <file path=\"f.txt\">{}</file>\n  </files>\n</digest>\n",
            cdata_sections(&text)
        );
        let extracted = extract(mahiron_ctx::config::OutputFormat::Xml, &document);
        prop_assert_eq!(extracted.get("f.txt").cloned().unwrap_or_default(), text);
    }
}

/// The payload of each section, as a conforming XML reader would see them.
fn payloads(sections: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = sections;
    while let Some(start) = rest.find("<![CDATA[") {
        let after = &rest[start + "<![CDATA[".len()..];
        let Some(end) = after.find("]]>") else { break };
        found.push(&after[..end]);
        rest = &after[end + "]]>".len()..];
    }
    found
}
