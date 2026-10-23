//! `N130` — a `kind: pleading` body carries no caption markup.
//!
//! A pleading's court, case number, judicial officer, parties, and counsel
//! block are the Project's `matter:` in `navigator.yaml`; its caption title,
//! footer title, and hearing are the notation's own frontmatter. The
//! renderer composes the first-page caption and the running footer from
//! those, so a body that also carries a caption block, the retired
//! "Parties / Case information" table, or one of the retired caption
//! placeholders would print the caption twice and let the copies drift.

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F130PleadingBodyHasNoCaption;

impl F130PleadingBodyHasNoCaption {
    pub const CODE: &'static str = "N130";
}

/// The caption placeholders the retired scaffold substituted.
const RETIRED_PLACEHOLDERS: [&str; 5] = [
    "attorney_block",
    "court_name",
    "party_caption",
    "case_number",
    "document_title",
];

const MIGRATION: &str = "declare the court, case number, judicial officer, parties, and counsel \
     once under `matter:` in navigator.yaml, and the caption_title, footer_title, and optional \
     hearing_date, hearing_time, and reservation_id in this notation\'s frontmatter; the body \
     starts at the document title";

fn is_caption_fence(line: &str) -> bool {
    let line = line.trim_start();
    ["```", "~~~"].iter().any(|fence| {
        line.strip_prefix(fence)
            .is_some_and(|info| info.split_whitespace().next() == Some("caption"))
    })
}

fn is_party_table_header(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|')
        && line
            .split('|')
            .map(|cell| cell.trim().to_ascii_lowercase())
            .any(|cell| cell == "parties" || cell == "case information")
}

fn retired_placeholder(line: &str) -> Option<&'static str> {
    RETIRED_PLACEHOLDERS
        .iter()
        .find(|code| line.contains(&format!("{{{{{code}}}}}")))
        .copied()
}

impl Rule for F130PleadingBodyHasNoCaption {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let is_pleading = frontmatter::extract(&file.contents)
            .and_then(|fm| frontmatter::field(fm, "kind"))
            .is_some_and(|kind| kind == "pleading");
        if !is_pleading {
            return Vec::new();
        }
        let frontmatter_lines = frontmatter::line_range(&file.contents);
        let mut found = Vec::new();
        let mut in_fence = false;
        for (index, text) in file.contents.lines().enumerate() {
            let line = index + 1;
            if frontmatter_lines
                .as_ref()
                .is_some_and(|range| range.contains(&line))
            {
                continue;
            }
            let trimmed = text.trim_start();
            let opens_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
            let what = if in_fence {
                in_fence = !opens_fence;
                None
            } else if opens_fence {
                in_fence = true;
                is_caption_fence(text).then(|| "a `caption` block".to_string())
            } else if is_party_table_header(text) {
                Some("a Parties / Case information table".to_string())
            } else {
                retired_placeholder(text).map(|code| format!("the `{{{{{code}}}}}` placeholder"))
            };
            if let Some(what) = what {
                found.push(Violation {
                    code: Self::CODE,
                    path: file.path.clone(),
                    line,
                    range: line_byte_range(&file.contents, line),
                    message: format!(
                        "a pleading body carries {what}, which duplicates the caption the \
                         renderer composes; {MIGRATION}"
                    ),
                });
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::F130PleadingBodyHasNoCaption;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn lint(kind: &str, body: &str) -> Vec<crate::Violation> {
        F130PleadingBodyHasNoCaption.lint(&SourceFile {
            path: PathBuf::from("templates/motion.md"),
            contents: format!("---\nkind: {kind}\ntitle: T\n---\n\n{body}"),
        })
    }

    #[test]
    fn a_body_that_starts_at_the_title_is_clean() {
        assert!(lint("pleading", "# MOTION\n\nText.\n").is_empty());
    }

    #[test]
    fn a_caption_block_in_either_fence_style_is_flagged_with_its_line() {
        let v = lint("pleading", "```caption\ncourt: X\n```\n");
        assert_eq!((v.len(), v[0].code, v[0].line), (1, "N130", 6));
        assert!(v[0].message.contains("matter:"), "{v:?}");
        assert_eq!(lint("pleading", "~~~caption\ncourt: X\n~~~\n").len(), 1);
    }

    #[test]
    fn the_retired_scaffold_table_and_placeholders_are_flagged() {
        let body = "{{attorney_block}}\n\n**{{court_name}}**\n\n| Parties | Case information |\n| --- | --- |\n| {{party_caption}} | Case No. {{case_number}}; {{document_title}} |\n";
        assert_eq!(lint("pleading", body).len(), 4);
    }

    #[test]
    fn other_kinds_and_other_code_fences_are_untouched() {
        assert!(lint("letter", "```caption\nx\n```\n").is_empty());
        assert!(lint("pleading", "```rust\nfn main() {}\n```\n").is_empty());
        assert!(lint("pleading", "| Item | Amount |\n| --- | --- |\n").is_empty());
    }
}
