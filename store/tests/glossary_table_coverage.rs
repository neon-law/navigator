//! Every stored record has one discoverable definition, including internal plumbing.
use std::collections::{BTreeMap, BTreeSet};

fn tables(schema: &str) -> BTreeSet<&str> {
    schema
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("DEFINE TABLE ")?;
            let rest = rest
                .strip_prefix("IF NOT EXISTS ")
                .or_else(|| rest.strip_prefix("OVERWRITE "))
                .unwrap_or(rest);
            rest.split_whitespace().next()
        })
        .collect()
}

fn coverage_errors(schema: &str, entries: &[(&str, &str)]) -> Vec<String> {
    let tables = tables(schema);
    let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut errors = Vec::new();
    for (slug, body) in entries {
        for line in body
            .lines()
            .filter(|line| line.starts_with("The table is "))
        {
            let Some(rest) = line.strip_prefix("The table is `") else {
                errors.push(format!("{slug}: malformed table attribution"));
                continue;
            };
            let Some((table, suffix)) = rest.split_once('`') else {
                errors.push(format!("{slug}: malformed table attribution"));
                continue;
            };
            if !suffix.starts_with('.') || table.is_empty() {
                errors.push(format!("{slug}: malformed table attribution"));
                continue;
            }
            owners.entry(table).or_default().push(slug);
        }
    }
    for table in &tables {
        match owners.get(table).map(Vec::as_slice) {
            Some([_]) => {}
            Some(slugs) => errors.push(format!("{table}: multiple definitions: {slugs:?}")),
            None => errors.push(format!("{table}: missing glossary definition")),
        }
    }
    for table in owners.keys().filter(|table| !tables.contains(*table)) {
        errors.push(format!("{table}: attribution names no schema table"));
    }
    errors
}

#[test]
fn every_schema_table_has_exactly_one_glossary_definition() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/glossary");
    let owned: Vec<_> = std::fs::read_dir(directory)
        .expect("glossary directory")
        .map(|entry| {
            let path = entry.expect("glossary entry").path();
            (
                path.file_name()
                    .expect("filename")
                    .to_string_lossy()
                    .into_owned(),
                path,
            )
        })
        .filter(|(name, path)| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && name != "README.md"
        })
        .map(|(name, path)| (name, std::fs::read_to_string(path).expect("glossary text")))
        .collect();
    let entries: Vec<_> = owned
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_str()))
        .collect();
    let errors = coverage_errors(include_str!("../src/schema/navigator.surql"), &entries);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn gate_detects_new_tables_duplicate_definitions_and_stale_attributions() {
    let schema =
        "DEFINE TABLE IF NOT EXISTS matter SCHEMAFULL;\nDEFINE TABLE OVERWRITE token SCHEMAFULL;";
    assert_eq!(tables(schema), BTreeSet::from(["matter", "token"]));
    let errors = coverage_errors(
        schema,
        &[
            ("matter", "The table is `matter`."),
            ("rival", "The table is `matter`."),
            ("stale", "The table is `removed`."),
            ("broken", "The table is token."),
        ],
    );
    assert!(errors.iter().any(|e| e.contains("matter: multiple")));
    assert!(errors.iter().any(|e| e.contains("token: missing")));
    assert!(errors.iter().any(|e| e.contains("removed: attribution")));
    assert!(errors.iter().any(|e| e.contains("broken: malformed")));
    assert!(coverage_errors(
        schema,
        &[
            ("matter", "The table is `matter`. A record."),
            ("token", "The table is `token`. Internal plumbing."),
        ]
    )
    .is_empty());
}
