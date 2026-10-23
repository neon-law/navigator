//! The catalog's acceptance test: scaffolding every matter page kind
//! produces a `pages:` entry that validates, and the fixtures under
//! `tests/fixtures/pages/` each fail with exactly the code they name.
//!
//! This runs the same D-family rule set the CLI's manifest pass runs over
//! `navigator.yaml`, so the scaffold an attorney is handed and the validator
//! that then judges their manifest agree on the first keystroke.

use rules::dashboard::{catalog, scaffold, Section, UNIVERSAL};
use rules::{lint_pages, lint_source_classified, DocumentKind, Kind, SourceFile};
use std::path::{Path, PathBuf};

fn dashboard_kinds() -> Vec<Kind> {
    Kind::ALL
        .iter()
        .copied()
        .filter(|k| k.is_dashboard())
        .collect()
}

fn manifest(contents: String) -> SourceFile {
    SourceFile {
        path: PathBuf::from("navigator.yaml"),
        contents,
    }
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pages")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn codes(file: &SourceFile) -> Vec<&'static str> {
    lint_pages(file).into_iter().map(|v| v.code).collect()
}

fn scaffolded(kind: Kind) -> String {
    format!(
        "pages:\n{}",
        scaffold(kind, "hub", "Sample hub").expect("a page kind scaffolds")
    )
}

#[test]
fn scaffolding_every_kind_produces_an_entry_that_validates() {
    assert!(!dashboard_kinds().is_empty(), "the catalog is not empty");
    for kind in dashboard_kinds() {
        let file = manifest(scaffolded(kind));
        let violations = lint_pages(&file);
        assert!(
            violations.is_empty(),
            "scaffolding `{}` produced an entry that does not validate: {violations:#?}",
            kind.as_str(),
        );
        assert!(
            rules::pages::shape_problems(&file.contents).is_empty(),
            "{}",
            kind.as_str()
        );
    }
}

#[test]
fn the_pages_rule_set_is_exactly_the_d_family() {
    let codes: Vec<&str> = rules::navigator_pages_rules()
        .iter()
        .map(|r| r.code())
        .collect();
    assert_eq!(codes, ["D001", "D002", "D003", "D004", "D005"]);
}

#[test]
fn every_scaffold_lists_only_sections_in_its_own_catalog() {
    for kind in dashboard_kinds() {
        let document: serde_yaml::Value = serde_yaml::from_str(&scaffolded(kind)).unwrap();
        let pages = rules::pages::entries(document.as_mapping().unwrap()).unwrap();
        let allowed = catalog(kind);
        for (lens, names) in &pages[0].lenses {
            for name in names {
                let section = Section::parse(name)
                    .unwrap_or_else(|| panic!("{} scaffolded unknown `{name}`", kind.as_str()));
                assert!(
                    allowed.contains(&section),
                    "{} lens `{lens}` scaffolded `{name}`, which is out of its catalog",
                    kind.as_str(),
                );
                for universal in UNIVERSAL {
                    assert!(names.iter().any(|n| n == universal.as_str()));
                }
            }
        }
    }
}

#[test]
fn the_valid_fixture_passes() {
    let file = manifest(fixture("valid.yaml"));
    assert!(codes(&file).is_empty(), "{:#?}", lint_pages(&file));
    assert!(rules::pages::shape_problems(&file.contents).is_empty());
}

#[test]
fn the_duplicate_slug_fixture_is_a_shape_problem() {
    let problems = rules::pages::shape_problems(&fixture("y021_duplicate_slug.yaml"));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].1.contains("timeline"), "{}", problems[0].1);
}

#[test]
fn each_d_fixture_fails_with_its_code() {
    for (name, code) in [
        ("d002_out_of_catalog.yaml", "D002"),
        ("d003_missing_boundary_note.yaml", "D003"),
        ("d005_lawyer_only_on_client.yaml", "D005"),
    ] {
        let file = manifest(fixture(name));
        assert_eq!(codes(&file), [code], "{name}: {:#?}", lint_pages(&file));
    }
}

#[test]
fn a_frontmatter_dashboard_is_prose_refused_by_s103() {
    let file = SourceFile {
        path: PathBuf::from("timeline.md"),
        contents: fixture("retired_dashboard.md.fixture"),
    };
    assert_eq!(rules::classify_source(&file), DocumentKind::Markdown);
    let violations = lint_source_classified(&file);
    let s103: Vec<_> = violations.iter().filter(|v| v.code == "S103").collect();
    assert_eq!(s103.len(), 1, "{violations:#?}");
    assert!(s103[0].message.contains("pages:"), "{}", s103[0].message);
    assert!(
        !violations.iter().any(|v| v.code.starts_with('D')),
        "the D-family never runs on Markdown: {violations:#?}"
    );
}
