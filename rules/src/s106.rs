//! `S106` — Markdown UTF-8 and character hygiene.
//!
//! PR #142 shipped a corrupted `docs/glossary/will.md` — a recommit that
//! declared raw text as base64, so the committed bytes were binary soup
//! that rendered as CJK mojibake — and nothing in the repo noticed. This
//! rule is the guard that failure earned.
//!
//! It checks three things per file, all reported as errors:
//!
//! 1. **The file decodes as valid UTF-8.** This cannot be a per-character
//!    scan of `SourceFile::contents` — a `String` is valid UTF-8 by
//!    construction, so the invalid bytes never reach a `Rule`. The
//!    [`crate::engine`] walkers therefore decode each file's raw bytes
//!    first and emit a synthetic `S106` violation naming the path and the
//!    offending byte offsets when decoding fails.
//! 2. **No control characters** besides the line structure a text file
//!    legitimately carries: `\n` and `\t` (and `\r`, which a Windows
//!    checkout materialises in CRLF pairs — the `crlf_line_endings`
//!    concern, not this rule's).
//! 3. **No characters outside the allowed set**: ASCII, plus a documented
//!    allowlist of typography the tree actually uses. The allowlist is
//!    the brief's typographic core (dashes, curly quotes, ellipsis) plus
//!    characters a full-tree audit found in legitimate use: Latin-1
//!    letters and legal/scholarly symbols (section sign, middle dot,
//!    times, divide, plus-minus, registered, trademark), foreign-language
//!    letters, arrows and the command key, mathematical and comparison
//!    symbols, bullets, box-drawing characters (U+2500..=U+257F),
//!    triangles, and the zodiac glyphs the council documents use
//!    (U+2648..=U+2653).
//!
//! What the allowlist deliberately excludes is everything mojibake and
//! stray pastes produce: CJK ideographs, Hangul, Cyrillic, Arabic,
//! full-width forms, and unassigned codepoints. A binary blob committed
//! as text trips either the decode check or this allowlist; a
//! CJK-rendering corruption trips both.

use crate::{Rule, SourceFile, Violation};

pub struct S106MarkdownHygiene;

impl S106MarkdownHygiene {
    pub const CODE: &'static str = "S106";
    /// Characters outside ASCII the tree is allowed to carry, beyond the
    /// ranges below. Given as escapes so the source file itself cannot be
    /// corrupted without a visible compile error. See the module doc for
    /// what each group is for.
    pub const ALLOWED_CHARS: &[char] = &[
        // The typographic core: en dash, em dash, left/right single and
        // double curly quotes, ellipsis.
        '\u{2013}', '\u{2014}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2026}',
        // Latin-1 letters in legitimate use (foreign words in docs).
        '\u{00E0}', '\u{00F6}',
        // Legal and scholarly symbols: section sign, middle dot, times,
        // divide, plus-minus, registered, trademark.
        '\u{00A7}', '\u{00B7}', '\u{00D7}', '\u{00F7}', '\u{00B1}', '\u{00AE}', '\u{2122}',
        // Arrows and the command key symbol.
        '\u{2190}', '\u{2191}', '\u{2192}', '\u{2318}',
        // Mathematical and comparison symbols, true minus sign, one-third.
        '\u{2212}', '\u{2248}', '\u{2264}', '\u{2208}', '\u{2153}',
        // Typographic bullet.
        '\u{2022}', // Solid triangles (tree/graph renderings).
        '\u{25B2}', '\u{25BC}',
    ];

    /// Contiguous ranges of characters allowed wholesale. Box-drawing
    /// (U+2500..=U+257F) covers the tree diagrams in `docs/`; the zodiac
    /// block (U+2648..=U+2653) is the council documents' voice headings.
    pub const ALLOWED_RANGES: &[(u32, u32)] = &[(0x2500, 0x257F), (0x2648, 0x2653)];

    /// True when `ch` is allowed in a Markdown file under this rule.
    #[must_use]
    pub fn is_allowed(ch: char) -> bool {
        if ch.is_ascii() {
            return true;
        }
        if Self::ALLOWED_CHARS.contains(&ch) {
            return true;
        }
        let code = u32::from(ch);
        Self::ALLOWED_RANGES
            .iter()
            .any(|(low, high)| (*low..=*high).contains(&code))
    }

    /// The violation the engine's byte-level decode pass emits when a
    /// file is not valid UTF-8 at all — the PR #142 failure mode.
    #[must_use]
    pub fn decode_violation(path: &std::path::Path, bad_byte_offset: usize) -> Violation {
        Violation {
            code: Self::CODE,
            path: path.to_path_buf(),
            line: 0,
            range: bad_byte_offset..bad_byte_offset,
            message: format!(
                "file is not valid UTF-8: the byte at offset {bad_byte_offset} does not start or continue a valid UTF-8 sequence"
            ),
        }
    }
}

impl Rule for S106MarkdownHygiene {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        "Markdown must be valid UTF-8 with no control characters or disallowed non-ASCII characters"
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let contents = &file.contents;
        contents
            .char_indices()
            .filter_map(|(offset, ch)| {
                let code = u32::from(ch);
                let reason = if ch.is_control() && !matches!(ch, '\n' | '\t' | '\r') {
                    Some(format!("control character U+{code:04X}"))
                } else if ch == '\u{FFFD}' {
                    Some(
                        "U+FFFD REPLACEMENT CHARACTER (a lossy decode already corrupted this file)"
                            .to_string(),
                    )
                } else if !Self::is_allowed(ch) {
                    Some(format!(
                        "character U+{code:04X} ({ch:?}) is outside the allowed Markdown character set"
                    ))
                } else {
                    None
                }?;
                let line = contents[..offset].matches('\n').count() + 1;
                Some(Violation {
                    code: Self::CODE,
                    path: file.path.clone(),
                    line,
                    range: offset..offset + ch.len_utf8(),
                    message: reason,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::S106MarkdownHygiene;
    use crate::{severity_for_code, Rule, Severity, SourceFile};
    use std::path::PathBuf;

    fn lint(contents: &str) -> Vec<String> {
        S106MarkdownHygiene
            .lint(&SourceFile {
                path: PathBuf::from("docs/sample.md"),
                contents: contents.to_string(),
            })
            .into_iter()
            .map(|v| v.message)
            .collect()
    }

    #[test]
    fn s106_is_an_error() {
        assert_eq!(
            severity_for_code(S106MarkdownHygiene::CODE),
            Severity::Error
        );
    }

    #[test]
    fn a_clean_english_file_passes() {
        let contents = "# Title\n\nPlain English prose with an em dash \u{2014} and \u{201C}curly quotes\u{201D}.\n";
        assert!(lint(contents).is_empty());
    }

    #[test]
    fn every_allowed_character_passes() {
        let mut contents = S106MarkdownHygiene::ALLOWED_CHARS
            .iter()
            .collect::<String>();
        contents.push('\n');
        let mut contents = contents
            + &S106MarkdownHygiene::ALLOWED_RANGES
                .iter()
                .flat_map(|(low, high)| (*low..=*high).filter_map(char::from_u32))
                .collect::<String>();
        contents.push('\n');
        assert!(
            lint(&contents).is_empty(),
            "allowed typography tripped: {:#?}",
            lint(&contents)
        );
    }

    #[test]
    fn a_control_character_is_flagged() {
        let messages = lint("a\u{00}b\n");
        assert!(messages
            .iter()
            .any(|m| m.contains("control character U+0000")));
    }

    #[test]
    fn a_replacement_character_is_flagged() {
        let messages = lint("before\u{FFFD}after\n");
        assert!(messages.iter().any(|m| m.contains("U+FFFD")));
    }

    #[test]
    fn cjk_mojibake_is_flagged() {
        let messages = lint("\u{4E2D}\u{6587} mojibake\n");
        assert!(messages
            .iter()
            .all(|m| m.contains("outside the allowed Markdown character set")));
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn cyrillic_is_flagged() {
        assert!(!lint("\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442}\n").is_empty());
    }

    #[test]
    fn violations_point_at_the_right_line_and_offset() {
        let contents = "line one\nline two \u{4E2D}\u{6587}\n";
        let file = SourceFile {
            path: PathBuf::from("docs/sample.md"),
            contents: contents.to_string(),
        };
        let violations = S106MarkdownHygiene.lint(&file);
        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].line, 2);
        assert_eq!(
            violations[0].range.start,
            contents.find('\u{4E2D}').unwrap()
        );
    }
}
