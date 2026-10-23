//! `D004` — a matter page must declare its per-lens composition.
//!
//! The client, lawyer, and clerk section lists live in **one entry**, which
//! is what keeps a page's faces from drifting apart. An entry with no
//! `lenses:` mapping has no face at all, and a lens named something outside
//! the vocabulary is a face nothing will ever render.

use crate::dashboard::Lens;
use crate::pages::{declared_pages, violation};
use crate::{Rule, SourceFile, Violation};

/// `D004` — `lenses:` must be present and name only known lenses.
pub struct D004LensComposition;

impl D004LensComposition {
    pub const CODE: &'static str = "D004";
}

impl Rule for D004LensComposition {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let known: Vec<&str> = Lens::ALL.iter().map(|l| l.as_str()).collect();
        let mut out = Vec::new();
        for page in declared_pages(&file.contents) {
            let line = page.line_of(&file.contents, "lenses:");
            let Some(lenses) = &page.lenses else {
                out.push(violation(
                    Self::CODE,
                    file,
                    line,
                    format!(
                        "Page `{}` must declare a `lenses:` mapping naming at least one of: {}",
                        page.slug,
                        known.join(", "),
                    ),
                ));
                continue;
            };
            if lenses.is_empty() {
                out.push(violation(
                    Self::CODE,
                    file,
                    line,
                    format!(
                        "Page `{}` has an empty `lenses:` — declare at least one of: {}",
                        page.slug,
                        known.join(", "),
                    ),
                ));
                continue;
            }
            for (name, _) in lenses {
                if Lens::parse(name).is_none() {
                    out.push(violation(
                        Self::CODE,
                        file,
                        page.line_of(&file.contents, &format!("{name}:")),
                        format!(
                            "Page `{}`: `{name}` is not a lens (expected one of: {})",
                            page.slug,
                            known.join(", "),
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
    use super::D004LensComposition;
    use crate::pages::one_page;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    #[test]
    fn known_lenses_pass() {
        let file = one_page(
            "authority_library",
            &[("lawyer", "authority_table"), ("client", ""), ("clerk", "")],
        );
        assert!(D004LensComposition.lint(&file).is_empty());
    }

    #[test]
    fn a_missing_lenses_block_is_flagged() {
        let file = SourceFile {
            path: PathBuf::from("navigator.yaml"),
            contents: "pages:\n  - slug: hub\n    kind: authority_library\n    title: Hub\n".into(),
        };
        let v = D004LensComposition.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert_eq!(v[0].code, "D004");
        assert!(v[0].message.contains("lenses"), "{}", v[0].message);
    }

    #[test]
    fn an_empty_lenses_block_is_flagged() {
        let file = SourceFile {
            path: PathBuf::from("navigator.yaml"),
            contents:
                "pages:\n  - slug: hub\n    kind: authority_library\n    title: Hub\n    lenses: {}\n"
                    .into(),
        };
        let v = D004LensComposition.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert!(v[0].message.contains("empty"), "{}", v[0].message);
    }

    #[test]
    fn an_unknown_lens_is_flagged() {
        let file = one_page("authority_library", &[("lawyer", ""), ("partner", "")]);
        let v = D004LensComposition.lint(&file);
        assert_eq!(v.len(), 1, "got {v:?}");
        assert!(v[0].message.contains("`partner`"), "{}", v[0].message);
        assert!(v[0].message.contains("clerk"), "{}", v[0].message);
    }
}
