//! Reviewed runtime evidence; references do not attest that a test has run.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    pub scenario: String,
    pub runtime_tests: Vec<Test>,
    pub remaining: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Test {
    pub file: String,
    pub function: String,
    pub proves: String,
}

pub(super) fn validate_tables(
    tables: &[String],
    evidence: &BTreeMap<String, Evidence>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        tables.iter().eq(evidence.keys()),
        "runtime parity evidence must cover exactly the source tables"
    );
    Ok(())
}

pub(super) fn validate(root: &Path, scenario: &str, evidence: &Evidence) -> anyhow::Result<()> {
    anyhow::ensure!(
        evidence.scenario == scenario,
        "stale runtime parity scenario"
    );
    anyhow::ensure!(
        (!evidence.runtime_tests.is_empty() || !evidence.remaining.is_empty())
            && evidence.remaining.iter().all(|gap| !gap.trim().is_empty()),
        "runtime evidence must link reviewed assertions or explain missing coverage"
    );
    let mut references = BTreeSet::new();
    for test in &evidence.runtime_tests {
        anyhow::ensure!(
            !test.proves.trim().is_empty(),
            "runtime test must name its reviewed assertion scope"
        );
        anyhow::ensure!(
            references.insert((&test.file, &test.function)),
            "duplicate runtime test reference"
        );
        let path = Path::new(&test.file);
        anyhow::ensure!(
            !test.file.is_empty()
                && path.extension().is_some_and(|extension| extension == "rs")
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "runtime test path must be repository-relative Rust source"
        );
        let path = root.join(path).canonicalize()?;
        anyhow::ensure!(
            path.starts_with(root.canonicalize()?),
            "runtime test path escapes repository"
        );
        let source = std::fs::read_to_string(path)?;
        let file = syn::parse_file(&source)?;
        anyhow::ensure!(
            runtime_tests(&file.items, &test.function) == 1,
            "{} must name one non-ignored async test in {}",
            test.function,
            test.file
        );
    }
    Ok(())
}

fn runtime_tests(items: &[syn::Item], name: &str) -> usize {
    items
        .iter()
        .map(|item| match item {
            syn::Item::Fn(function) => usize::from(
                function.sig.ident == name
                    && function.sig.asyncness.is_some()
                    && function.attrs.iter().any(|attribute| {
                        attribute.path().is_ident("test")
                            || attribute
                                .path()
                                .segments
                                .last()
                                .is_some_and(|segment| segment.ident == "test")
                    })
                    && !function
                        .attrs
                        .iter()
                        .any(|attribute| attribute.path().is_ident("ignore")),
            ),
            syn::Item::Mod(module) => module
                .content
                .as_ref()
                .map_or(0, |(_, items)| runtime_tests(items, name)),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(dir: &Path) -> Evidence {
        std::fs::write(
            dir.join("runtime.rs"),
            "#[tokio::test]\nasync fn round_trip() {}\nfn helper() {}\n#[tokio::test]\n#[ignore]\nasync fn ignored() {}",
        )
        .unwrap();
        Evidence {
            scenario: "stable identity".into(),
            runtime_tests: vec![Test {
                file: "runtime.rs".into(),
                function: "round_trip".into(),
                proves: "mirror round trip".into(),
            }],
            remaining: vec!["application query parity".into()],
        }
    }

    #[test]
    fn stale_scenario_and_missing_test_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut row = evidence(dir.path());
        assert!(validate(dir.path(), "changed scenario", &row).is_err());
        row.runtime_tests[0].function = "renamed".into();
        assert!(validate(dir.path(), "stable identity", &row).is_err());
    }

    #[test]
    fn helpers_ignored_tests_and_paths_outside_the_repository_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for function in ["helper", "ignored"] {
            let mut row = evidence(dir.path());
            row.runtime_tests[0].function = function.into();
            assert!(validate(dir.path(), "stable identity", &row).is_err());
        }
        for file in ["../runtime.rs", "/runtime.rs"] {
            let mut row = evidence(dir.path());
            row.runtime_tests[0].file = file.into();
            assert!(validate(dir.path(), "stable identity", &row).is_err());
        }
    }

    #[test]
    fn reviewed_test_references_and_explicit_gaps_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let mut row = evidence(dir.path());
        validate(dir.path(), "stable identity", &row).unwrap();
        row.remaining.clear();
        validate(dir.path(), "stable identity", &row).unwrap();
        row.remaining = vec!["application query parity".into()];
        row.runtime_tests.clear();
        validate(dir.path(), "stable identity", &row).unwrap();
        row.remaining.clear();
        assert!(validate(dir.path(), "stable identity", &row).is_err());
    }

    #[test]
    fn uncovered_and_removed_source_tables_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let row = evidence(dir.path());
        let tables = vec!["person".into()];
        let mut rows = BTreeMap::from([("person".into(), row.clone())]);
        validate_tables(&tables, &rows).unwrap();
        rows.remove("person");
        assert!(validate_tables(&tables, &rows).is_err());
        rows.insert("removed".into(), row);
        assert!(validate_tables(&tables, &rows).is_err());
    }

    #[test]
    fn duplicate_references_and_blank_claims_or_gaps_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut row = evidence(dir.path());
        row.runtime_tests.push(row.runtime_tests[0].clone());
        assert!(validate(dir.path(), "stable identity", &row).is_err());
        row.runtime_tests.pop();
        row.runtime_tests[0].proves = " ".into();
        assert!(validate(dir.path(), "stable identity", &row).is_err());
        row.runtime_tests.clear();
        row.remaining = vec![" ".into()];
        assert!(validate(dir.path(), "stable identity", &row).is_err());
    }
}
