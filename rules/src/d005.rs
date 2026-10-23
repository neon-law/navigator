//! `D005` — a section must be one the page's kind permits on that lens.
//!
//! [`D002`](crate::D002OutOfCatalogSection) asks whether a kind may carry a
//! section at all. This rule asks the narrower question: on which faces. A
//! kind's [`Skeleton::lawyer_only`](crate::dashboard::Skeleton::lawyer_only)
//! sections are the firm's working surface for that page — a reviewer's
//! status setter, an assertion still under verification, a prebill review —
//! so a client or clerk lens that declares one is composed wrong, whatever
//! the authorization layer would later have shown.

use crate::dashboard::{skeleton, Lens, Section};
use crate::pages::{declared_pages, violation};
use crate::{Rule, SourceFile, Violation};

/// `D005` — a lawyer-only section declared on another lens.
pub struct D005LensSection;

impl D005LensSection {
    pub const CODE: &'static str = "D005";
}

impl Rule for D005LensSection {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let mut out = Vec::new();
        for page in declared_pages(&file.contents) {
            let Some(skeleton) = skeleton(page.kind) else {
                continue;
            };
            for (lens_name, names) in page.lenses.iter().flatten() {
                // An unknown lens is D004's report.
                let Some(lens) = Lens::parse(lens_name) else {
                    continue;
                };
                for name in names {
                    let Some(section) = Section::parse(name) else {
                        continue;
                    };
                    if skeleton.permits(lens, section) {
                        continue;
                    }
                    out.push(violation(
                        Self::CODE,
                        file,
                        page.line_of(&file.contents, name),
                        format!(
                            "Page `{}` lens `{lens_name}` declares `{name}`, which `{}` permits \
                             only on the lawyer lens",
                            page.slug,
                            page.kind.as_str(),
                        ),
                    ));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::D005LensSection;
    use crate::pages::one_page;
    use crate::Rule;

    const LAWYER: &str = "engagement_letter, invoice, prebill_review, boundary_note, \
                          provenance_statement";

    #[test]
    fn a_lawyer_only_section_on_the_lawyer_lens_passes() {
        let file = one_page("engagement_billing_records", &[("lawyer", LAWYER)]);
        assert!(D005LensSection.lint(&file).is_empty());
    }

    #[test]
    fn a_lawyer_only_section_on_the_client_lens_is_flagged() {
        let file = one_page(
            "engagement_billing_records",
            &[
                ("lawyer", LAWYER),
                (
                    "client",
                    "invoice, prebill_review, boundary_note, provenance_statement",
                ),
            ],
        );
        let v = D005LensSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert_eq!(v[0].code, "D005");
        assert!(v[0].message.contains("`client`"), "{}", v[0].message);
        assert!(
            v[0].message.contains("`prebill_review`"),
            "{}",
            v[0].message
        );
    }

    #[test]
    fn the_clerk_lens_is_held_to_the_same_line() {
        let file = one_page(
            "verifier_split_view",
            &[(
                "clerk",
                "assertion_panel, boundary_note, provenance_statement",
            )],
        );
        assert_eq!(D005LensSection.lint(&file).len(), 1);
    }

    #[test]
    fn a_section_shared_across_lenses_passes() {
        let file = one_page(
            "engagement_billing_records",
            &[(
                "client",
                "engagement_letter, invoice, boundary_note, provenance_statement",
            )],
        );
        assert!(D005LensSection.lint(&file).is_empty());
    }
}
