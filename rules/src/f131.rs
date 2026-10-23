//! `N131` — a notation template carries no drafting markers.
//!
//! Every document a Project produces is final (see [`crate::final_document`]),
//! so its template cannot carry what a final render refuses: a bracketed
//! drafting marker, a fill-in blank outside a signature block, or a drafting
//! note. `{{placeholders}}` stay legal here; a missing fact is a questionnaire
//! state the template declares, not a blank a person fills in by hand.
//!
//! A blank the signer completes by hand is a Signer Box: a questionnaire
//! state declared `completed_by: signer` and referenced by its placeholder.
//! The fill prints the rule, never the source, so a bare `____` in prose is
//! refused whatever the questionnaire declares, and so is the render's own
//! box marker typed into the body. Whether a placeholder names a declared
//! state is `N120`'s check (`N115` for a dotted path).

use crate::final_document::{drafting_markers, Marker};
use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F131TemplateHasNoDraftingMarkers;

impl F131TemplateHasNoDraftingMarkers {
    pub const CODE: &'static str = "N131";
}

impl Rule for F131TemplateHasNoDraftingMarkers {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(frontmatter_lines) = frontmatter::line_range(&file.contents) else {
            return Vec::new();
        };
        let body_start = *frontmatter_lines.end();
        let body = file
            .contents
            .lines()
            .skip(body_start)
            .collect::<Vec<_>>()
            .join("\n");
        drafting_markers(&body)
            .into_iter()
            .map(|finding| {
                let line = body_start + finding.line;
                let what = match finding.marker {
                    Marker::Bracket(text) => format!("the drafting marker `{text}`"),
                    Marker::Blank => "a fill-in blank outside a signature block".to_string(),
                    Marker::Note(note) => format!("the drafting note `{note}`"),
                    Marker::Instruction(label) => format!("the instruction `{label}:`"),
                    Marker::Service => {
                        "a certificate of service with a blank; render one only from recorded service"
                            .to_string()
                    }
                    Marker::SignerBox => format!(
                        "the render's own `{}` marker; a Signer Box is declared `completed_by: \
                         signer` and referenced by its placeholder",
                        crate::final_document::SIGNER_BOX
                    ),
                    Marker::Placeholder(_) => unreachable!("drafting_markers skips placeholders"),
                };
                Violation {
                    code: Self::CODE,
                    path: file.path.clone(),
                    line,
                    range: line_byte_range(&file.contents, line),
                    message: format!(
                        "this template carries {what}; a final document has none, so declare a \
                         questionnaire state and use its `{{{{placeholder}}}}` instead"
                    ),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::F131TemplateHasNoDraftingMarkers;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn lint(body: &str) -> Vec<crate::Violation> {
        F131TemplateHasNoDraftingMarkers.lint(&SourceFile {
            path: PathBuf::from("templates/letter.md"),
            contents: format!("---\nkind: letter\ntitle: T\n---\n\n{body}"),
        })
    }

    #[test]
    fn placeholders_and_signature_blanks_pass() {
        assert!(lint("Dear {{person__client.name}},\n\nBy: ________  Date: ________\n").is_empty());
    }

    #[test]
    fn a_body_blank_and_a_bracket_are_flagged_at_their_file_line() {
        let found = lint("Payment due Date: ________.\n\nThe fee is [INSERT FEE].\n");
        assert_eq!(
            found
                .iter()
                .map(|violation| (violation.code, violation.line))
                .collect::<Vec<_>>(),
            vec![("N131", 6), ("N131", 8)]
        );
        assert!(found[1].message.contains("`[INSERT FEE]`"), "{found:?}");
    }

    /// A template whose questionnaire declares a Signer Box, in the shape
    /// of a statutory initials box and a wet-ink share count.
    fn boxed(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("templates/power_of_attorney.md"),
            contents: format!(
                "---\nkind: letter\ntitle: T\nquestionnaire:\n  - person__client\n  - state: \
                 custom_text__initials_l\n    prompt: Initial to grant (L).\n    completed_by: \
                 signer\n  - state: custom_text__shares\n    prompt: Shares subscribed.\n    \
                 completed_by: signer\n---\n\n{body}"
            ),
        }
    }

    /// LAW-141: every blank outside a signature block is a declared Signer
    /// Box referenced by its placeholder. Such a template passes; a bare
    /// `____` in prose and the render's box marker are still refused.
    #[test]
    fn declared_boxes_pass_and_bare_blanks_and_typed_markers_are_refused() {
        let declared = boxed(
            "- ({{custom_text__initials_l}}) (L) retirement benefit transactions;\n\n\
             Shares: {{custom_text__shares}}\n\n## Signatures\n\nBy: ________  Date: ________\n",
        );
        assert!(
            F131TemplateHasNoDraftingMarkers.lint(&declared).is_empty(),
            "{:?}",
            F131TemplateHasNoDraftingMarkers.lint(&declared)
        );

        let bare = boxed("- (____) (L) retirement benefit transactions;\n");
        let found = F131TemplateHasNoDraftingMarkers.lint(&bare);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("fill-in blank"), "{found:?}");

        let typed = boxed(&format!("Shares: {}\n", crate::final_document::SIGNER_BOX));
        let found = F131TemplateHasNoDraftingMarkers.lint(&typed);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("completed_by"), "{found:?}");
    }

    /// An undeclared placeholder is `N120`'s refusal, so the gate holds the
    /// box to its declaration while `N131` reports each defect once.
    #[test]
    fn an_undeclared_box_placeholder_is_refused_by_n120_not_twice() {
        let undeclared = boxed("Units: {{custom_text__units}}\n");
        assert!(F131TemplateHasNoDraftingMarkers
            .lint(&undeclared)
            .is_empty());
        let found = crate::F120BodyStateGrounding.lint(&undeclared);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .message
                .contains("undeclared questionnaire state `custom_text__units`"),
            "{found:?}"
        );
    }

    #[test]
    fn a_file_without_frontmatter_is_not_a_template() {
        let file = SourceFile {
            path: PathBuf::from("README.md"),
            contents: "TODO [later] ____ here\n".to_string(),
        };
        assert!(F131TemplateHasNoDraftingMarkers.lint(&file).is_empty());
    }
}
