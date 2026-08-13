//! Sanitization of untrusted text for display and logging.

/// Strip terminal-control and Unicode bidi/format characters from untrusted
/// text. Line breaks are preserved (post bodies legitimately contain them);
/// every other control character (ESC, CSI, OSC, ...) and the bidi/zero-width
/// format range is removed, so attacker-controlled strings cannot inject
/// escape sequences into the terminal or visually reorder surrounding UI text.
pub fn clean_text(input: &str) -> String {
    input
        .chars()
        .filter(|character| {
            if matches!(character, '\n' | '\t' | '\r') {
                return true;
            }
            if character.is_control() {
                return false;
            }
            !is_format_character(*character)
        })
        .collect()
}

/// Unicode format characters (`Cf` category) that can reorder or hide text in
/// bidi-aware terminals: bidi embedding/override controls, right-to-left
/// marks, and zero-width joiners/spaces. These are not control characters
/// (`char::is_control` is false for them), so ratatui's own buffer filter
/// lets them through.
fn is_format_character(character: char) -> bool {
    matches!(character,
        '\u{00AD}'                      // soft hyphen
        | '\u{061C}'                    // Arabic letter mark
        | '\u{180E}'                    // Mongolian vowel separator
        | '\u{200B}'..='\u{200F}'       // zero-width space … right-to-left mark
        | '\u{202A}'..='\u{202E}'       // bidi embedding/override controls
        | '\u{2060}'..='\u{206F}'       // word joiner … invisible operators
        | '\u{FEFF}'                    // zero-width no-break space
    )
}

#[cfg(test)]
mod tests {
    use super::clean_text;

    #[test]
    fn keeps_line_breaks_and_plain_text() {
        assert_eq!(clean_text("hello\nworld\t!"), "hello\nworld\t!");
    }

    #[test]
    fn strips_escape_sequences() {
        // ESC itself is a control character and is removed; the trailing
        // characters are inert without it.
        assert_eq!(clean_text("a\x1b[2Jb"), "a[2Jb");
        assert_eq!(clean_text("a\x1b]0;title\x07b"), "a]0;titleb");
    }

    #[test]
    fn strips_bidi_and_zero_width_characters() {
        assert_eq!(clean_text("a\u{202E}b"), "ab");
        assert_eq!(clean_text("a\u{200B}b"), "ab");
        assert_eq!(clean_text("a\u{202D}b\u{2066}c\u{2069}"), "abc");
        assert_eq!(clean_text("a\u{FEFF}b"), "ab");
    }
}
