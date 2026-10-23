//! `D001` — a section named in a `pages:` entry's `lenses:` block must be a
//! recognized section type.
//!
//! An unrecognized name is a blocking error rather than a dropped section:
//! the renderer only ever sees registered sections, so a typo would
//! otherwise publish a page that silently lacks the thing the attorney
//! thought they put on it. If the shape genuinely does not exist, the fix is
//! a new [`Section`] with its component in `webapp::sections` — never a
//! hand-written page.

use crate::dashboard::Section;
use crate::pages::{declared_pages, violation};
use crate::{Rule, SourceFile, Violation};

/// `D001` — every section named in `lenses:` must be a known type.
pub struct D001UnknownSection;

impl D001UnknownSection {
    pub const CODE: &'static str = "D001";
}

impl Rule for D001UnknownSection {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let mut out = Vec::new();
        for page in declared_pages(&file.contents) {
            for (lens, sections) in page.lenses.iter().flatten() {
                for name in sections {
                    if Section::parse(name).is_some() {
                        continue;
                    }
                    out.push(violation(
                        Self::CODE,
                        file,
                        page.line_of(&file.contents, name),
                        format!(
                            "Page `{}` lens `{lens}` names `{name}`, which is not a section \
                             type. Known sections: {}",
                            page.slug,
                            crate::dashboard::section_names().join(", "),
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
    use super::D001UnknownSection;
    use crate::pages::one_page;
    use crate::Rule;

    #[test]
    fn a_known_section_passes() {
        let file = one_page("authority_library", &[("lawyer", "authority_table")]);
        assert!(D001UnknownSection.lint(&file).is_empty());
    }

    #[test]
    fn an_unknown_section_is_flagged_with_the_vocabulary() {
        let file = one_page("authority_library", &[("lawyer", "vibe_chart")]);
        let v = D001UnknownSection.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert_eq!(v[0].code, "D001");
        assert!(v[0].message.contains("`vibe_chart`"), "{}", v[0].message);
        assert!(
            v[0].message.contains("authority_table"),
            "the message must name the vocabulary: {}",
            v[0].message,
        );
        assert_eq!(v[0].line, 6, "underlines the offending lens line");
    }

    #[test]
    fn a_non_page_kind_is_left_to_y021() {
        let file = one_page("onboarding", &[("lawyer", "vibe_chart")]);
        assert!(D001UnknownSection.lint(&file).is_empty());
    }
}
