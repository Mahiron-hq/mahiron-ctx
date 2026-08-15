use std::collections::BTreeMap;

use crate::config::LineEnding;

use super::classify::{detect_bom, Classification};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("content could not be decoded as {encoding} without loss")]
    Lossy { encoding: &'static str },
    #[error("encoding could not be determined")]
    Undetermined,
}

#[derive(Debug, Clone)]
pub struct DecodedFile<'a> {
    pub text: std::borrow::Cow<'a, str>,
    pub encoding: &'static str,
    pub line_endings: BTreeMap<LineEnding, usize>,
}

/// Decode a complete file using the encoding discovered during classification.
///
/// A byte-order mark is consumed as part of decoding and never reaches the text, and a
/// decode that would substitute replacement characters is refused outright rather than
/// silently producing altered content.
pub fn decode<'a>(
    bytes: &'a [u8],
    classification: &Classification,
) -> Result<DecodedFile<'a>, DecodeError> {
    let (encoding, bom_length) = match classification {
        Classification::Text {
            encoding,
            bom_length,
        } => (*encoding, *bom_length),
        _ => return Err(DecodeError::Undetermined),
    };

    // The prefix inspected during discovery may have been too short to reveal a mark that
    // a complete read now shows, so the check is repeated against the whole file.
    let bom_length = detect_bom(bytes).map_or(bom_length, |(_, length)| length);
    let body = &bytes[bom_length.min(bytes.len())..];

    // Left borrowed where the bytes are already valid text, which is the common case and
    // saves a full copy of every file in the tree.
    let text = encoding
        .decode_without_bom_handling_and_without_replacement(body)
        .ok_or(DecodeError::Lossy {
            encoding: encoding.name(),
        })?;

    let line_endings = count_line_endings(&text);
    Ok(DecodedFile {
        text,
        encoding: encoding.name(),
        line_endings,
    })
}

/// Tally of each line-ending convention present, used to surface mixed conventions.
pub fn count_line_endings(text: &str) -> BTreeMap<LineEnding, usize> {
    let bytes = text.as_bytes();
    let (mut lf, mut crlf, mut cr) = (0_usize, 0_usize, 0_usize);

    // Counted into locals and collected once at the end: a map lookup per line ending
    // costs more than the scan itself on a file of any size.
    let mut index = 0;
    while let Some(offset) = memchr::memchr2(b'\n', b'\r', &bytes[index..]) {
        let position = index + offset;
        match bytes[position] {
            b'\n' => {
                lf += 1;
                index = position + 1;
            }
            _ if bytes.get(position + 1) == Some(&b'\n') => {
                crlf += 1;
                index = position + 2;
            }
            _ => {
                cr += 1;
                index = position + 1;
            }
        }
    }

    let mut counts = BTreeMap::new();
    for (ending, count) in [
        (LineEnding::Lf, lf),
        (LineEnding::Crlf, crlf),
        (LineEnding::Cr, cr),
    ] {
        if count > 0 {
            counts.insert(ending, count);
        }
    }
    counts
}

/// Characters no output format can carry, XML CDATA sections included.
///
/// Re-exported from [`super::classify`] rather than restated: the two definitions used to
/// disagree about the form feed, which meant a file containing one was classified as text
/// and then dropped much later under a reason that described something else.
pub use super::classify::contains_disallowed_control;

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::{SHIFT_JIS, UTF_8};

    #[test]
    fn byte_order_mark_never_reaches_the_text() {
        let bytes = b"\xEF\xBB\xBFhello";
        let decoded = decode(
            bytes,
            &Classification::Text {
                encoding: UTF_8,
                bom_length: 3,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "hello");
    }

    #[test]
    fn legacy_encoding_round_trips_to_the_same_characters() {
        let (bytes, _, _) = SHIFT_JIS.encode("日本語のテスト\n");
        let decoded = decode(
            &bytes,
            &Classification::Text {
                encoding: SHIFT_JIS,
                bom_length: 0,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "日本語のテスト\n");
    }

    #[test]
    fn undecodable_content_is_refused_rather_than_substituted() {
        let bytes = b"valid \xF0\x28\x8C\x28 invalid";
        let error = decode(
            bytes,
            &Classification::Text {
                encoding: UTF_8,
                bom_length: 0,
            },
        )
        .unwrap_err();
        assert!(matches!(error, DecodeError::Lossy { .. }));
    }

    #[test]
    fn mixed_conventions_are_counted_separately() {
        let counts = count_line_endings("a\nb\r\nc\rd");
        assert_eq!(counts.get(&LineEnding::Lf), Some(&1));
        assert_eq!(counts.get(&LineEnding::Crlf), Some(&1));
        assert_eq!(counts.get(&LineEnding::Cr), Some(&1));
    }

    #[test]
    fn preserved_content_is_byte_identical_for_utf8() {
        let original = "  indented\ttrailing   \n\n\nno newline at end";
        let decoded = decode(
            original.as_bytes(),
            &Classification::Text {
                encoding: UTF_8,
                bom_length: 0,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, original);
    }
}
