//! `N133` — a pleading in a Project repository does not declare `docket:`.
//!
//! A Project may litigate more than one case, so a pleading's frontmatter
//! says which one it serves: `docket:` names a case number the Project's
//! `navigator.yaml` declares under `dockets:` (or the `matter:` block's own
//! `case_number`). The renderer resolves the value case-insensitively into
//! the declared spelling and refuses an undeclared one, and a pleading with
//! no `docket:` captions itself with the `matter:` case number — correct
//! for a single-case Project, silent for a multi-case one. This advisory
//! is the missing-declaration signal: it reports a Project pleading that
//! declares no `docket:` at all, so the caption's case number is never a
//! quiet default. It lives only in Project trees; the catalog ships
//! pleading templates, not filings.

use crate::{frontmatter, line_byte_range, Kind, Rule, SourceFile, Violation};

/// The frontmatter key that binds a pleading to a declared docket.
const DOCKET_KEY: &str = "docket";

pub struct F133PleadingDocketUndeclared;

impl F133PleadingDocketUndeclared {
    pub const CODE: &'static str = "N133";
}

impl Rule for F133PleadingDocketUndeclared {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        let pleading =
            frontmatter::field(fm, "kind").is_some_and(|kind| kind == Kind::Pleading.as_str());
        if !pleading || !crate::f110::is_project_repository_tree(&file.path) {
            return Vec::new();
        }
        let declared =
            frontmatter::field(fm, DOCKET_KEY).is_some_and(|value| !value.trim().is_empty());
        if declared {
            return Vec::new();
        }
        vec![Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line: 1,
            range: line_byte_range(&file.contents, 1),
            message: "this pleading does not declare `docket:` — the caption cannot say \
                      which case the filing serves; name one of the Project's `dockets:` \
                      in `navigator.yaml`"
                .to_string(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::F133PleadingDocketUndeclared;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    /// A file under a tempdir Project tree, whose `navigator.yaml` carries
    /// the `project` key the tree check looks for. The caller holds the
    /// tempdir so the tree outlives the lint call.
    fn project_tree(relative: &str, fm: &str) -> (tempfile::TempDir, SourceFile) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  name: sample-litigation\n",
        )
        .unwrap();
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        (
            dir,
            SourceFile {
                path,
                contents: format!("{fm}\n"),
            },
        )
    }

    fn catalog_pleading() -> SourceFile {
        SourceFile {
            path: PathBuf::from("templates/notations/neon_law/complaint__letter.md"),
            contents: "---\nkind: pleading\ntitle: T\ncaption_title: C\n---\n\n# Body\n"
                .to_string(),
        }
    }

    #[test]
    fn a_project_pleading_without_a_docket_key_is_warned() {
        let (_tree, file) = project_tree(
            "templates/complaint.md",
            "---\nkind: pleading\ntitle: T\ncaption_title: C\n---\n\n# Body\n",
        );
        let found = F133PleadingDocketUndeclared.lint(&file);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].code, "N133");
        assert_eq!(found[0].line, 1);
        assert!(found[0].message.contains("`docket:`"), "{found:?}");
    }

    #[test]
    fn a_catalog_pleading_without_a_docket_key_is_silent() {
        assert!(F133PleadingDocketUndeclared
            .lint(&catalog_pleading())
            .is_empty());
    }

    #[test]
    fn a_declared_docket_is_silent() {
        let (_tree, file) = project_tree(
            "templates/complaint.md",
            "---\nkind: pleading\ntitle: T\ncaption_title: C\ndocket: 00CV000000\n---\n\n# Body\n",
        );
        let found = F133PleadingDocketUndeclared.lint(&file);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_declared_nevada_docket_is_silent() {
        let (_tree, file) = project_tree(
            "templates/complaint.md",
            "---\nkind: pleading\ntitle: T\ncaption_title: C\ndocket: D-26-000000-D\n---\n\n# Body\n",
        );
        let found = F133PleadingDocketUndeclared.lint(&file);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_non_pleading_kind_is_silent() {
        let (_tree, file) = project_tree(
            "templates/letter.md",
            "---\nkind: letter\ntitle: T\n---\n\n# Body\n",
        );
        let found = F133PleadingDocketUndeclared.lint(&file);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_bare_docket_key_is_warned() {
        let (_tree, file) = project_tree(
            "templates/complaint.md",
            "---\nkind: pleading\ntitle: T\ncaption_title: C\ndocket:\n---\n\n# Body\n",
        );
        let found = F133PleadingDocketUndeclared.lint(&file);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].code, "N133");
    }
}
