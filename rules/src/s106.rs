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
//!    letters and legal/scholarly symbols (section sign, pilcrow, middle dot,
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
//!
//! A disallowed character with an obvious ASCII spelling — the vulgar
//! fractions, `⅔` → `2/3`, `½` → `1/2` — is named in the finding and
//! autofixed. The fix re-wraps the paragraph with `S102`'s repack when
//! the longer spelling would carry a line past `S101`'s limit.

use crate::{line_byte_range, Rule, S102LinePacking, SourceFile, TextEdit, Violation};

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
        // Legal and scholarly symbols: section sign, pilcrow (the
        // pleading-paragraph pinpoint, `¶ 12` / `¶¶ 12–14`), middle dot,
        // times, divide, plus-minus, registered, trademark.
        '\u{00A7}', '\u{00B6}', '\u{00B7}', '\u{00D7}', '\u{00F7}', '\u{00B1}', '\u{00AE}', '\u{2122}',
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

    /// The ASCII spelling of a disallowed character that has an obvious
    /// one: the vulgar fractions (U+00BC..=U+00BE, U+2150..=U+215E).
    /// The finding names it, and the fix writes it. One-third (U+2153)
    /// is on the allowlist, so it is never flagged.
    #[must_use]
    pub fn ascii_spelling(ch: char) -> Option<&'static str> {
        Some(match ch {
            '\u{00BC}' => "1/4",
            '\u{00BD}' => "1/2",
            '\u{00BE}' => "3/4",
            '\u{2150}' => "1/7",
            '\u{2151}' => "1/9",
            '\u{2152}' => "1/10",
            '\u{2153}' => "1/3",
            '\u{2154}' => "2/3",
            '\u{2155}' => "1/5",
            '\u{2156}' => "2/5",
            '\u{2157}' => "3/5",
            '\u{2158}' => "4/5",
            '\u{2159}' => "1/6",
            '\u{215A}' => "5/6",
            '\u{215B}' => "1/8",
            '\u{215C}' => "3/8",
            '\u{215D}' => "5/8",
            '\u{215E}' => "7/8",
            _ => return None,
        })
    }

    /// The text that replaces the disallowed `ch` at byte `offset`, if it
    /// has an ASCII spelling. A fraction straight after a digit gains a
    /// space, so `13½` becomes `13 1/2` rather than `131/2`.
    fn replacement(contents: &str, offset: usize, ch: char) -> Option<String> {
        if Self::is_allowed(ch) {
            return None;
        }
        let spelling = Self::ascii_spelling(ch)?;
        let after_digit = contents[..offset]
            .chars()
            .next_back()
            .is_some_and(|prev| prev.is_ascii_digit());
        Some(if after_digit {
            format!(" {spelling}")
        } else {
            spelling.to_string()
        })
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
                    let hint = Self::ascii_spelling(ch).map_or_else(String::new, |spelling| {
                        format!(
                            "; write `{spelling}` instead (`navigator project gate` without `--ci` applies it)"
                        )
                    });
                    Some(format!(
                        "character U+{code:04X} ({ch:?}) is outside the allowed Markdown character set{hint}"
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

    /// Spell a vulgar fraction in ASCII, re-wrapping its paragraph when
    /// the longer spelling would carry the line past `S101`'s limit.
    ///
    /// The line is measured with every fraction on it spelled out, not
    /// just this one, so two fractions that each fit alone cannot
    /// together leave an over-long line no rule can fix. When the line
    /// fits, the edit is the single replacement; when it does not, the
    /// edit is `S102`'s greedy repack of the paragraph with that line's
    /// fractions spelled out, and any other finding inside it lands on
    /// the next pass of the fix loop. Where the repack declines — a table
    /// row, a heading, a fence, a hard break — the replacement still
    /// lands and `S101` reports the line to a human.
    fn fix(&self, file: &SourceFile, violation: &Violation) -> Option<TextEdit> {
        let contents = &file.contents;
        let ch = contents.get(violation.range.clone())?.chars().next()?;
        let single = TextEdit {
            range: violation.range.clone(),
            new_text: Self::replacement(contents, violation.range.start, ch)?,
        };
        let line = line_byte_range(contents, violation.line);
        if !line.contains(&violation.range.start) {
            return Some(single);
        }
        let mut spelled = String::with_capacity(line.len() + 8);
        for (offset, c) in contents[line.clone()].char_indices() {
            match Self::replacement(contents, line.start + offset, c) {
                Some(text) => spelled.push_str(&text),
                None => spelled.push(c),
            }
        }
        let max = S102LinePacking::DEFAULT_MAX;
        if spelled.chars().count() <= max {
            return Some(single);
        }
        let grow = spelled.len() - line.len();
        let patched = SourceFile {
            path: file.path.clone(),
            contents: format!(
                "{}{spelled}{}",
                &contents[..line.start],
                &contents[line.end..]
            ),
        };
        let Some(wrap) = S102LinePacking::default().rewrap_line(&patched, violation.line) else {
            return Some(single);
        };
        // The repack covers whole lines, so it spans the spelled line;
        // shrink its end back into the original file's offsets.
        if wrap.range.start > line.start || wrap.range.end < line.end + grow {
            return Some(single);
        }
        Some(TextEdit {
            range: wrap.range.start..wrap.range.end - grow,
            new_text: wrap.new_text,
        })
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

    /// Apply every S106 fix the way `cli`'s `fix_directory` does: lint,
    /// collect edits, keep non-overlapping ones, apply highest-offset
    /// first, and repeat until the file stops changing.
    fn fixed(contents: &str) -> String {
        let mut text = contents.to_string();
        for _ in 0..8 {
            let file = SourceFile {
                path: PathBuf::from("docs/sample.md"),
                contents: text.clone(),
            };
            let mut edits: Vec<_> = S106MarkdownHygiene
                .lint(&file)
                .iter()
                .filter_map(|v| S106MarkdownHygiene.fix(&file, v))
                .collect();
            edits.sort_by_key(|e| e.range.start);
            let mut kept: Vec<crate::TextEdit> = Vec::new();
            for edit in edits {
                if kept
                    .last()
                    .is_some_and(|prev| edit.range.start < prev.range.end)
                {
                    continue;
                }
                kept.push(edit);
            }
            if kept.is_empty() {
                break;
            }
            for edit in kept.iter().rev() {
                text.replace_range(edit.range.clone(), &edit.new_text);
            }
        }
        text
    }

    #[test]
    fn the_pilcrow_and_section_sign_are_allowed() {
        // The profession's own citation marks: a pleading pinpoint and a
        // statutory section.
        let contents =
            "See Compl. \u{B6} 12 and \u{B6}\u{B6} 14\u{2013}16; 28 U.S.C. \u{A7} 1331.\n";
        assert!(lint(contents).is_empty(), "{:#?}", lint(contents));
    }

    #[test]
    fn a_vulgar_fraction_finding_names_its_ascii_spelling() {
        let two_thirds = lint("A 66\u{2154}% membership interest.\n");
        assert_eq!(two_thirds.len(), 1);
        assert!(two_thirds[0].contains("`2/3`"), "{two_thirds:?}");
        let half = lint("Thirteen and \u{BD} weeks.\n");
        assert!(half[0].contains("`1/2`"), "{half:?}");
        for (ch, spelling) in [('\u{BC}', "1/4"), ('\u{BE}', "3/4"), ('\u{215E}', "7/8")] {
            let messages = lint(&format!("x {ch} y\n"));
            assert!(
                messages[0].contains(&format!("`{spelling}`")),
                "{messages:?}"
            );
        }
    }

    #[test]
    fn a_character_with_no_ascii_spelling_has_no_fix() {
        let file = SourceFile {
            path: PathBuf::from("docs/sample.md"),
            contents: "\u{4E2D} mojibake\n".to_string(),
        };
        let violations = S106MarkdownHygiene.lint(&file);
        assert!(S106MarkdownHygiene.fix(&file, &violations[0]).is_none());
        assert!(!violations[0].message.contains("write `"));
    }

    #[test]
    fn the_fix_spells_a_fraction_in_ascii() {
        assert_eq!(
            fixed("Unit \u{BD} of the lot.\n"),
            "Unit 1/2 of the lot.\n"
        );
        // After a digit the fraction gets a space, so thirteen and a half
        // never reads as 131/2.
        assert_eq!(
            fixed("A 66\u{2154}% interest for 13\u{BD} weeks.\n"),
            "A 66 2/3% interest for 13 1/2 weeks.\n"
        );
    }

    #[test]
    fn the_fix_rewraps_a_paragraph_the_spelling_pushes_past_the_limit() {
        // Line one is exactly 120 characters with the fraction; spelled
        // out it would be 122.
        let first = format!("{} the \u{BD} shares xy", "word ".repeat(21).trim_end());
        assert_eq!(first.chars().count(), 120, "{first}");
        let contents = format!("# Heading\n\n{first}\ncontinues here.\n\nNext paragraph.\n");
        let out = fixed(&contents);
        assert!(!out.contains('\u{BD}'), "{out}");
        assert!(out.contains(" 1/2 "), "{out}");
        assert!(
            out.lines().all(|line| line.chars().count() <= 120),
            "a line still exceeds 120: {out}"
        );
        // Re-wrapping moves words, never adds or drops them, and stays
        // inside the paragraph.
        let words = |s: &str| {
            s.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(words(&out), words(&contents.replace('\u{BD}', "1/2")));
        assert!(out.starts_with("# Heading\n\n"), "{out}");
        assert!(out.ends_with("\n\nNext paragraph.\n"), "{out}");
    }

    #[test]
    fn the_fix_rewraps_every_fraction_on_an_overlong_line_at_once() {
        // Each fraction alone fits; together their spellings do not. The
        // fix measures the whole line so the pair cannot slip past S101.
        let first = format!("{} \u{BD} and \u{BC} x", "word ".repeat(22).trim_end());
        assert_eq!(first.chars().count(), 119, "{first}");
        let out = fixed(&format!("{first}\n"));
        assert!(out.contains("1/2") && out.contains("1/4"), "{out}");
        assert!(out.lines().all(|line| line.chars().count() <= 120), "{out}");
    }

    #[test]
    fn the_fix_leaves_a_table_row_unwrapped() {
        // A table row's line break is structure, so the spelling lands
        // and S101 is left to a human.
        let row = format!("| {} \u{BD} |", "cell ".repeat(23).trim_end());
        assert_eq!(row.chars().count(), 120, "{row}");
        let out = fixed(&format!("{row}\n"));
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains(" 1/2 |"), "{out}");
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
