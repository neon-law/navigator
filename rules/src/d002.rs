//! `D002` — a section must be in the declared kind's catalog.
//!
//! Pick-and-choose is real, but it stays inside the kind. A section that
//! exists in the registry yet does not belong to the page being declared is
//! an error, because that is what makes per-kind checking mean anything: an
//! authority table on a deliverable package is not a deliberate choice, it
//! is a kind that was declared wrong.

use crate::dashboard::{catalog, Section};
use crate::pages::{declared_pages, violation};
use crate::{Rule, SourceFile, Violation};

/// `D002` — every section must be in the declared kind's catalog.
pub struct D002OutOfCatalogSection;

impl D002OutOfCatalogSection {
    pub const CODE: &'static str = "D002";
}

impl Rule for D002OutOfCatalogSection {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let mut out = Vec::new();
        for page in declared_pages(&file.contents) {
            let allowed = catalog(page.kind);
            let allowed_names: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
            for (lens, sections) in page.lenses.iter().flatten() {
                for name in sections {
                    // An unrecognized name is D001's report, not this one.
                    let Some(section) = Section::parse(name) else {
                        continue;
                    };
                    if allowed.contains(&section) {
                        continue;
                    }
                    out.push(violation(
                        Self::CODE,
                        file,
                        page.line_of(&file.contents, name),
                        format!(
                            "Page `{}` lens `{lens}` declares `{name}`, which is not in the \
                             `{}` catalog. This kind may carry: {}",
                            page.slug,
                            page.kind.as_str(),
                            allowed_names.join(", "),
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
    use super::D002OutOfCatalogSection;
    use crate::pages::one_page;
    use crate::Rule;

    #[test]
    fn a_section_in_the_catalog_passes() {
        let file = one_page(
            "deliverable_package",
            &[("lawyer", "package_manifest, download_list")],
        );
        assert!(D002OutOfCatalogSection.lint(&file).is_empty());
    }

    #[test]
    fn a_real_section_from_another_kind_is_flagged() {
        let file = one_page(
            "deliverable_package",
            &[("lawyer", "package_manifest, discovery_pairs")],
        );
        let v = D002OutOfCatalogSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert_eq!(v[0].code, "D002");
        assert!(
            v[0].message.contains("`discovery_pairs`"),
            "{}",
            v[0].message
        );
        assert!(
            v[0].message.contains("package_manifest"),
            "the message must name the catalog: {}",
            v[0].message,
        );
    }

    #[test]
    fn an_unknown_name_is_left_to_d001() {
        // Exactly one diagnostic per mistake: a name that is in no
        // vocabulary is D001's, and this rule stays silent on it.
        let file = one_page("deliverable_package", &[("lawyer", "vibe_chart")]);
        assert!(D002OutOfCatalogSection.lint(&file).is_empty());
    }
}
