//! `D003` — a matter page must carry its skeleton.
//!
//! Two checks, both structural:
//!
//! - **Every declared lens carries the universal sections.** A boundary
//!   note and a provenance statement appear on every surface in the
//!   surveyed corpus, and they are the two that say what the page is
//!   *not*. Leaving them to the author is how a client face ships
//!   asserting more certainty than the firm has, so they are part of the
//!   skeleton and this rule holds every lens to them.
//! - **The kind's spine appears somewhere.** A review queue workbench
//!   with no item rail is not a review queue workbench. The spine is
//!   required in at least one lens, not in all of them — a client face
//!   legitimately shows less than the firm's.

use crate::dashboard::{skeleton, Section, UNIVERSAL};
use crate::pages::{declared_pages, violation};
use crate::{Rule, SourceFile, Violation};

/// `D003` — required sections must be present.
pub struct D003RequiredSection;

impl D003RequiredSection {
    pub const CODE: &'static str = "D003";
}

impl Rule for D003RequiredSection {
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
            // A missing `lenses:` block is D004's report; without one there
            // is nothing here to check against.
            let Some(lenses) = &page.lenses else {
                continue;
            };
            let line = page.line_of(&file.contents, "lenses:");
            for (lens, names) in lenses {
                let sections: Vec<Section> =
                    names.iter().filter_map(|n| Section::parse(n)).collect();
                for universal in UNIVERSAL {
                    if !sections.contains(universal) {
                        out.push(violation(
                            Self::CODE,
                            file,
                            line,
                            format!(
                                "Page `{}` lens `{lens}` is missing `{}` — {}. Every lens of \
                                 every page kind carries it.",
                                page.slug,
                                universal.as_str(),
                                universal.describe().to_lowercase(),
                            ),
                        ));
                    }
                }
            }
            let declared_anywhere: Vec<Section> = lenses
                .iter()
                .flat_map(|(_, names)| names.iter())
                .filter_map(|n| Section::parse(n))
                .collect();
            for required in skeleton.required {
                if !declared_anywhere.contains(required) {
                    out.push(violation(
                        Self::CODE,
                        file,
                        line,
                        format!(
                            "Page `{}`: `{}` requires `{}` in at least one lens — {}.",
                            page.slug,
                            page.kind.as_str(),
                            required.as_str(),
                            required.describe().to_lowercase(),
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
    use super::D003RequiredSection;
    use crate::pages::one_page;
    use crate::Rule;

    const FULL: &str = "authority_table, boundary_note, provenance_statement";
    const UNIVERSAL: &str = "boundary_note, provenance_statement";

    #[test]
    fn a_complete_composition_passes() {
        let file = one_page(
            "authority_library",
            &[("lawyer", FULL), ("client", UNIVERSAL)],
        );
        let v = D003RequiredSection.lint(&file);
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn a_lens_without_a_boundary_note_is_flagged() {
        let file = one_page(
            "authority_library",
            &[("lawyer", "authority_table, provenance_statement")],
        );
        let v = D003RequiredSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert_eq!(v[0].code, "D003");
        assert!(v[0].message.contains("boundary_note"), "{}", v[0].message);
        assert!(v[0].message.contains("`lawyer`"), "{}", v[0].message);
    }

    #[test]
    fn the_client_lens_is_held_to_the_universal_sections_too() {
        // The failure this rule exists to prevent: a client face that
        // never says what the page is not.
        let file = one_page(
            "authority_library",
            &[("lawyer", FULL), ("client", "provenance_statement")],
        );
        let v = D003RequiredSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert!(v[0].message.contains("`client`"), "{}", v[0].message);
    }

    #[test]
    fn a_missing_spine_section_is_flagged() {
        let file = one_page(
            "review_queue_workbench",
            &[(
                "lawyer",
                "queue_rail, item_detail, boundary_note, provenance_statement",
            )],
        );
        let v = D003RequiredSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert!(
            v[0].message.contains("item_status_setter"),
            "{}",
            v[0].message,
        );
    }

    #[test]
    fn a_non_page_kind_is_left_alone() {
        let file = one_page("onboarding", &[("lawyer", "")]);
        assert!(D003RequiredSection.lint(&file).is_empty());
    }
}
