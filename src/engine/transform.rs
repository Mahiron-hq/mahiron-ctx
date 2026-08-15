use std::borrow::Cow;

use crate::config::TransformSettings;

/// Split text into (line body, terminator) pairs, keeping each line's own terminator.
fn lines_with_terminators(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let bytes = rest.as_bytes();
        let break_at = bytes.iter().position(|b| matches!(b, b'\n' | b'\r'));
        match break_at {
            None => {
                let line = rest;
                rest = "";
                Some((line, ""))
            }
            Some(index) => {
                let terminator_len =
                    if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                let line = &rest[..index];
                let terminator = &rest[index..index + terminator_len];
                rest = &rest[index + terminator_len..];
                Some((line, terminator))
            }
        }
    })
}

/// Apply the mechanical, language-agnostic transformations the user opted into.
///
/// With none selected the input is handed back untouched, so an opt-in feature cannot
/// affect a run that never mentioned it.
pub fn apply<'a>(text: &'a str, settings: &TransformSettings) -> Cow<'a, str> {
    if !settings.remove_blank_lines
        && !settings.trim_trailing_whitespace
        && settings.normalize_line_endings.is_none()
    {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    for (line, terminator) in lines_with_terminators(text) {
        let body = if settings.trim_trailing_whitespace {
            line.trim_end_matches([' ', '\t', '\u{b}', '\u{c}'])
        } else {
            line
        };

        if settings.remove_blank_lines && body.trim().is_empty() && !terminator.is_empty() {
            continue;
        }

        out.push_str(body);
        if terminator.is_empty() {
            continue;
        }
        match settings.normalize_line_endings {
            Some(ending) => out.push_str(ending.as_str()),
            None => out.push_str(terminator),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::LineEnding;

    fn settings(blank: bool, trim: bool, normalize: Option<LineEnding>) -> TransformSettings {
        TransformSettings {
            remove_blank_lines: blank,
            trim_trailing_whitespace: trim,
            normalize_line_endings: normalize,
            ..Default::default()
        }
    }

    #[test]
    fn no_transformation_borrows_the_original() {
        let text = "  a  \n\n\tb\r\n";
        assert!(matches!(
            apply(text, &settings(false, false, None)),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn blank_line_removal_keeps_the_remaining_terminators() {
        let text = "a\n\nb\r\n\r\nc";
        assert_eq!(apply(text, &settings(true, false, None)), "a\nb\r\nc");
    }

    #[test]
    fn trimming_leaves_indentation_untouched() {
        let text = "    indented   \n\tkeep\t\n";
        assert_eq!(
            apply(text, &settings(false, true, None)),
            "    indented\n\tkeep\n"
        );
    }

    #[test]
    fn normalisation_only_happens_when_requested() {
        let text = "a\r\nb\rc\n";
        assert_eq!(apply(text, &settings(false, false, None)), text);
        assert_eq!(
            apply(text, &settings(false, false, Some(LineEnding::Lf))),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn final_line_without_terminator_survives() {
        let text = "a\nb";
        assert_eq!(apply(text, &settings(true, true, None)), "a\nb");
    }
}
