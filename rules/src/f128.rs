//! `N128` — only a form notation may declare `origin_url:`.
//!
//! `origin_url:` names the public government source of a bundled blank
//! form: `N110` requires an HTTPS `.gov` one on every template under
//! `notations/forms/`, and `N109` requires one beside `output: form`. This
//! rule is the converse. A firm-authored template has no government
//! original, so an `origin_url:` on one is provenance the renderer would
//! show a reader as the document's source — a stray link to wherever the
//! text was adapted from, not a form the firm fills.
//!
//! A form is a template on the `notations/forms/` shelf or one that
//! declares `output: form`; everything else must leave the key off.

use std::path::Path;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F128OriginUrlOnlyOnForms;

impl F128OriginUrlOnlyOnForms {
    pub const CODE: &'static str = "N128";

    /// Whether `path` sits on the `templates/notations/forms/` shelf.
    fn on_forms_shelf(path: &Path) -> bool {
        let segments: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(seg) => seg.to_str(),
                _ => None,
            })
            .collect();
        segments
            .windows(3)
            .any(|w| w == ["templates", "notations", "forms"])
    }
}

impl Rule for F128OriginUrlOnlyOnForms {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        if frontmatter::field(fm, "origin_url").is_none() {
            return Vec::new();
        }
        let output_form = frontmatter::field(fm, "output").as_deref() == Some("form");
        if output_form || Self::on_forms_shelf(&file.path) {
            return Vec::new();
        }
        vec![Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line: 1,
            range: line_byte_range(&file.contents, 1),
            message: "Only a form notation (under `notations/forms/` or `output: form`) may \
                      declare `origin_url:`; remove it"
                .to_string(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::F128OriginUrlOnlyOnForms;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn lint(path: &str, frontmatter: &str) -> Vec<crate::Violation> {
        F128OriginUrlOnlyOnForms.lint(&SourceFile {
            path: PathBuf::from(path),
            contents: format!("---\n{frontmatter}\n---\n\nBody.\n"),
        })
    }

    #[test]
    fn flags_origin_url_on_a_firm_template() {
        let v = lint(
            "templates/notations/neon_law/mutual_nda.md",
            "title: T\norigin_url: https://github.com/example/templates",
        );
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N128");
    }

    #[test]
    fn allows_origin_url_on_the_forms_shelf() {
        let v = lint(
            "templates/notations/forms/united_states/nevada/state/nv__llc_formation.md",
            "title: T\norigin_url: https://www.nvsos.gov/forms",
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn allows_origin_url_beside_output_form() {
        let v = lint(
            "templates/some_form.md",
            "title: T\noutput: form\norigin_url: https://www.nvsos.gov/forms",
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn ignores_a_template_without_origin_url() {
        assert!(lint("templates/notations/neon_law/mutual_nda.md", "title: T").is_empty());
    }
}
