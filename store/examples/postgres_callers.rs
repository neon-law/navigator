//! Candidate caller inventory for the Surreal-to-PostgreSQL transition.
//!
//! Lexical matches are leads for review, not proof of a read/write dependency:
//! query fragments may be composed elsewhere. The schema remains authoritative
//! for the complete set of source tables.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[path = "postgres_callers/queries.rs"]
mod queries;

#[path = "postgres_callers/coverage.rs"]
mod coverage;

#[derive(Clone, Deserialize, Serialize)]
struct Ownership {
    scope: String,
    through: String,
    migration_slice: String,
    parity_scenario: String,
    /// Born in PostgreSQL with no Surreal source table to inventory.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    target_only: bool,
}

#[derive(Serialize)]
struct Entry {
    source_table: String,
    migration_disposition: &'static str,
    source_fields: BTreeMap<String, String>,
    target_table_present: bool,
    ownership: Ownership,
    destination: String,
    source_indexes: Vec<String>,
    source_field_rules: Vec<String>,
    source_table_declaration: String,
    candidate_rust_callers: Vec<String>,
    query_call_ids: Vec<usize>,
    review_state: &'static str,
    runtime_parity: coverage::Evidence,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "check".into());
    let root = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let (rendered, calls) = render(&root)?;
    let path = root.join("store/postgres/caller-inventory.json");
    let call_path = root.join("store/postgres/source-call-sites.jsonl");
    match command.as_str() {
        "write" => {
            fs::write(&path, rendered)?;
            fs::write(&call_path, calls)?;
        }
        "check" => {
            anyhow::ensure!(
                fs::read_to_string(&path)? == rendered,
                "caller inventory is stale"
            );
            anyhow::ensure!(
                fs::read_to_string(&call_path)? == calls,
                "source call sites are stale"
            );
        }
        _ => anyhow::bail!(
            "usage: cargo run -p store --example postgres_callers -- [check|write] [root]"
        ),
    }
    Ok(())
}

fn render(root: &Path) -> anyhow::Result<(String, String)> {
    let target = store::postgres_schema::parse_schema(store::postgres_schema::canonical_schema())?;
    let files = rust_files(root)?;
    let ownership: BTreeMap<String, Ownership> = serde_json::from_str(&fs::read_to_string(
        root.join("store/postgres/ownership.json"),
    )?)?;
    let tables = store::schema::table_names();
    validate_ownership(&tables, &target, &ownership)?;
    let evidence: BTreeMap<String, coverage::Evidence> = serde_json::from_str(
        &fs::read_to_string(root.join("store/postgres/runtime-parity.json"))?,
    )?;
    coverage::validate_tables(&tables, &evidence)?;
    let calls = files
        .iter()
        .map(|(file, contents)| queries::inventory(file, contents, &tables))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let schema = fs::read_to_string(root.join("store/src/schema/navigator.surql"))?;
    let declarations = schema
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut entries = Vec::new();
    for table in tables {
        let source_fields = store::schema::table_columns(&table)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let owner = ownership
            .get(&table)
            .ok_or_else(|| anyhow::anyhow!("missing owner for {table}"))?;
        let runtime_parity = evidence
            .get(&table)
            .ok_or_else(|| anyhow::anyhow!("missing runtime evidence for {table}"))?;
        coverage::validate(root, &owner.parity_scenario, runtime_parity)
            .map_err(|error| anyhow::anyhow!("runtime evidence for {table}: {error}"))?;
        let candidate_rust_callers = files
            .iter()
            .filter(|(_, contents)| contains_identifier(contents, &table))
            .map(|(path, _)| path.clone())
            .collect();
        entries.push(Entry {
            source_table: table.clone(),
            migration_disposition: match table.as_str() {
                "draft" => "derived_view",
                "schema_version" => "migration_ledger",
                "notation_meter_entry" => "pending_retirement_decision",
                _ => "migrate",
            },
            source_fields,
            target_table_present: target.contains_key(&table),
            ownership: owner.clone(),
            destination: match table.as_str() {
                "draft" => "SQL view over template".into(),
                "relationship" => "relationship SQL rows with a rebuildable AGE projection".into(),
                _ => format!("PostgreSQL {table}"),
            },
            source_indexes: declarations.split(';').filter(|statement| {
                statement.trim_start().starts_with("DEFINE INDEX")
                    && statement.split_whitespace().collect::<Vec<_>>().windows(2)
                        .any(|pair| pair[0] == "ON" && pair[1] == table)
            }).map(|statement| statement.split_whitespace().collect::<Vec<_>>().join(" ")).collect(),
            source_field_rules: declarations.split(';').filter(|statement| {
                statement.trim_start().starts_with("DEFINE FIELD")
                    && statement.split_whitespace().collect::<Vec<_>>().windows(2)
                        .any(|pair| pair[0] == "ON" && pair[1] == table)
            }).map(|statement| statement.split_whitespace().collect::<Vec<_>>().join(" ")).collect(),
            source_table_declaration: declarations.split(';').find(|statement| {
                statement.trim_start().starts_with("DEFINE TABLE") && contains_identifier(statement, &table)
            }).map_or_else(String::new, |statement| statement.split_whitespace().collect::<Vec<_>>().join(" ")),
            candidate_rust_callers,
            query_call_ids: calls.iter().enumerate().filter(|(_, call)| call.tables.contains(&table))
                .map(|(index, _)| index).collect(),
            review_state:
                "runtime test references are reviewed assertion scope, not execution results or application-read parity",
            runtime_parity: runtime_parity.clone(),
        });
    }
    let mut call_lines = String::new();
    for call in &calls {
        call_lines.push_str(&serde_json::to_string(call)?);
        call_lines.push('\n');
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&entries)?),
        call_lines,
    ))
}

fn validate_ownership<T>(
    tables: &[String],
    target: &BTreeMap<String, T>,
    ownership: &BTreeMap<String, Ownership>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        tables.iter().eq(ownership
            .iter()
            .filter(|(_, owner)| !owner.target_only)
            .map(|(table, _)| table)),
        "ownership matrix must cover exactly the source tables"
    );
    for (table, owner) in ownership {
        anyhow::ensure!(
            !owner.target_only || (target.contains_key(table) && !tables.contains(table)),
            "target-only {table} must be a PostgreSQL table with no Surreal source"
        );
        anyhow::ensure!(
            !owner.scope.is_empty()
                && !owner.through.is_empty()
                && !owner.migration_slice.is_empty()
                && !owner.parity_scenario.is_empty(),
            "incomplete ownership contract for {table}"
        );
    }
    Ok(())
}

fn rust_files(root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let source = entry?.path().join("src");
        if source.is_dir() {
            collect(root, &source, &mut files)?;
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push((
                path.strip_prefix(root)?.to_string_lossy().into_owned(),
                fs::read_to_string(path)?,
            ));
        }
    }
    Ok(())
}

fn contains_identifier(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        boundary(before) && boundary(after)
    })
}

#[cfg(test)]
mod tests {
    use super::{contains_identifier, validate_ownership, Ownership};
    use std::collections::BTreeMap;

    #[test]
    fn table_name_matching_uses_identifier_boundaries() {
        assert!(contains_identifier("SELECT * FROM project;", "project"));
        assert!(!contains_identifier("project_id project_header", "project"));
    }

    #[test]
    fn ownership_matrix_rejects_uncovered_and_stale_tables() {
        let owners: BTreeMap<String, Ownership> =
            serde_json::from_str(include_str!("../postgres/ownership.json")).unwrap();
        let tables = store::schema::table_names();
        let target =
            store::postgres_schema::parse_schema(store::postgres_schema::canonical_schema())
                .unwrap();
        validate_ownership(&tables, &target, &owners).unwrap();
        assert!(owners["clause"].target_only);
        let mut missing = owners.clone();
        missing.remove("project");
        assert!(validate_ownership(&tables, &target, &missing).is_err());
        let mut extra = owners.clone();
        extra.insert("untracked".into(), owners["project"].clone());
        assert!(validate_ownership(&tables, &target, &extra).is_err());
        let mut absent_target = owners.clone();
        absent_target.insert("untracked".into(), owners["clause"].clone());
        assert!(validate_ownership(&tables, &target, &absent_target).is_err());
        let mut source_marked_target_only = owners.clone();
        source_marked_target_only
            .get_mut("project")
            .unwrap()
            .target_only = true;
        assert!(validate_ownership(&tables, &target, &source_marked_target_only).is_err());
    }
}
