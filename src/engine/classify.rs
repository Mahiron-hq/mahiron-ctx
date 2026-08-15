use chardetng::EncodingDetector;
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};

/// Bytes inspected when deciding what a file is; enough to be conclusive on real files
/// without turning discovery into a full read of the source.
pub const PREFIX_BYTES: usize = 8192;

/// What inspection of a file's actual bytes concluded about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Text {
        encoding: &'static Encoding,
        /// Number of leading bytes belonging to a byte-order mark.
        bom_length: usize,
    },
    Binary,
    /// Neither confidently text nor confidently binary; never guessed at.
    Undetermined,
}

/// Byte-order mark at the start of `bytes`, if any.
pub fn detect_bom(bytes: &[u8]) -> Option<(&'static Encoding, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((UTF_8, 3))
    } else if bytes.starts_with(&[0xFF, 0xFE]) && !bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        Some((UTF_16LE, 2))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((UTF_16BE, 2))
    } else {
        None
    }
}

/// Control characters that never appear in text this tool is willing to reproduce.
///
/// This is the single definition. XML 1.0 admits no way to carry any of them, not even
/// inside a CDATA section, and markdown and plain text would carry them straight into a
/// document nothing downstream can handle — so the same set governs classification and
/// the whole-file check in composition. Tab, line feed, carriage return are text.
///
/// Note what this does *not* guarantee on its own: classification reads only
/// [`PREFIX_BYTES`], so a control character further into a file passes here and is caught
/// by [`contains_disallowed_control`] once the file has been read in full.
pub fn is_disallowed_control(byte: u8) -> bool {
    matches!(byte, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F)
}

/// The same predicate over decoded text, for the whole-file check.
pub fn is_disallowed_control_char(ch: char) -> bool {
    u8::try_from(ch as u32)
        .map(is_disallowed_control)
        .unwrap_or(false)
}

/// Whether decoded text holds anything no output format can carry.
pub fn contains_disallowed_control(text: &str) -> bool {
    text.chars().any(is_disallowed_control_char)
}

/// Trim a trailing partial multi-byte sequence so a truncated prefix is not misjudged.
fn trim_partial_sequence(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    let floor = end.saturating_sub(4);
    while end > floor {
        // Continuation bytes cannot start a sequence, so back up past them and then past
        // the leading byte of the sequence they belong to.
        let byte = bytes[end - 1];
        if byte & 0b1100_0000 == 0b1000_0000 {
            end -= 1;
            continue;
        }
        if byte & 0b1000_0000 == 0 {
            return &bytes[..end];
        }
        return &bytes[..end - 1];
    }
    &bytes[..end]
}

/// Classify a file from a prefix of its content.
/// Best available guess for a file the caller has already decided is text, whatever its
/// bytes look like. A single-byte legacy encoding maps every byte to some character, so
/// an override can be honoured rather than failing on the first byte that is not UTF-8.
pub fn guess_encoding(prefix: &[u8], is_complete: bool) -> &'static Encoding {
    if let Some((encoding, _)) = detect_bom(prefix) {
        return encoding;
    }
    let mut detector = EncodingDetector::new();
    detector.feed(prefix, is_complete);
    let guess = detector.guess(None, true);
    if guess.decode_without_bom_handling(prefix).1 {
        return encoding_rs::WINDOWS_1252;
    }
    guess
}

pub fn classify(prefix: &[u8], is_complete: bool) -> Classification {
    if prefix.is_empty() {
        return Classification::Text {
            encoding: UTF_8,
            bom_length: 0,
        };
    }

    if let Some((encoding, bom_length)) = detect_bom(prefix) {
        return Classification::Text {
            encoding,
            bom_length,
        };
    }

    let inspected = if is_complete {
        prefix
    } else {
        trim_partial_sequence(prefix)
    };

    if inspected.iter().copied().any(is_disallowed_control) {
        return Classification::Binary;
    }

    if std::str::from_utf8(inspected).is_ok() {
        return Classification::Text {
            encoding: UTF_8,
            bom_length: 0,
        };
    }

    let mut detector = EncodingDetector::new();
    detector.feed(inspected, is_complete);
    let guess = detector.guess(None, true);

    let (decoded, _, had_errors) = guess.decode(inspected);
    if had_errors || decoded.contains('\u{FFFD}') {
        return Classification::Undetermined;
    }

    Classification::Text {
        encoding: guess,
        bom_length: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_files_are_text() {
        assert!(matches!(classify(b"", true), Classification::Text { .. }));
    }

    #[test]
    fn nul_bytes_mean_binary() {
        assert_eq!(classify(b"MZ\x00\x00\x90", true), Classification::Binary);
    }

    #[test]
    fn utf8_bom_is_detected_without_being_consumed_as_content() {
        let bytes = b"\xEF\xBB\xBFhello";
        assert_eq!(
            classify(bytes, true),
            Classification::Text {
                encoding: UTF_8,
                bom_length: 3
            }
        );
    }

    #[test]
    fn legacy_encodings_are_recognised_rather_than_rejected() {
        // Shift_JIS bytes for a short Japanese phrase.
        let bytes = b"\x93\xfa\x96\x7b\x8c\xea\x82\xcc\x83\x65\x83\x58\x83\x67\n";
        assert!(matches!(classify(bytes, true), Classification::Text { .. }));
    }

    #[test]
    fn truncated_sequences_do_not_flip_the_verdict() {
        let mut bytes = "日本語のテキストです".repeat(400).into_bytes();
        bytes.truncate(PREFIX_BYTES + 1);
        assert!(matches!(
            classify(&bytes[..PREFIX_BYTES], false),
            Classification::Text { .. }
        ));
    }

    #[test]
    fn a_form_feed_is_treated_the_same_way_everywhere() {
        // The two predicates used to disagree about 0x0C, so a file containing one was
        // classified as text and then dropped later with an unrelated reason.
        assert_eq!(classify(b"page\x0cbreak\n", true), Classification::Binary);
        assert!(contains_disallowed_control("page\u{c}break"));
    }

    #[test]
    fn control_characters_outside_tab_and_newline_mean_binary() {
        assert_eq!(classify(b"bell \x07\n", true), Classification::Binary);
        assert_eq!(classify(b"escape \x1b[31m\n", true), Classification::Binary);
        assert!(matches!(
            classify(b"tabs\tand\r\nnewlines\n", true),
            Classification::Text { .. }
        ));
    }
}
