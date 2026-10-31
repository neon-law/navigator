//! Deterministic PostgreSQL migration inventory, schema parity, and ERD output.
//!
//! The PostgreSQL files are a target schema under construction. The Surreal
//! statement of the present remains authoritative until the application
//! migration is complete. This module makes the covered tables and fields
//! explicit so parity advances by reviewable migration rather than inference.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_yaml::Value;
use thiserror::Error;

const CANONICAL_SCHEMA: &str = include_str!("../postgres/schema.sql");
const INITIAL_MIGRATION: &str =
    include_str!("../postgres/migrations/20261010120000_create_core_tenancy.sql");
const POST_MIGRATION: &str = include_str!("../postgres/migrations/20261010130000_create_post.sql");
const BLOG_VIDEO_MIGRATION: &str =
    include_str!("../postgres/migrations/20261021170000_create_blog_video.sql");
const SOCIAL_MIGRATION: &str =
    include_str!("../postgres/migrations/20261011120000_create_social_handle_and_publication.sql");
const JOURNAL_MIGRATION: &str =
    include_str!("../postgres/migrations/20261011200000_create_firm_journal.sql");
const COMMUNICATIONS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261016110000_create_communications_conversations.sql");
const PROJECT_COSTS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261016100000_create_project_costs.sql");
const PROJECT_REFERENCE_MIGRATION: &str =
    include_str!("../postgres/migrations/20261017020000_create_project_reference.sql");
const EMAIL_DELIVERY_MIGRATION: &str =
    include_str!("../postgres/migrations/20261017030000_create_email_receipts_and_delivery.sql");
const MAILROOMS_LETTERS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261017110000_create_mailrooms_letters.sql");
const LEADS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261017120000_create_leads.sql");
const MATTER_PAGES_CAPTIONS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261018120000_create_matter_pages_and_captions.sql");
const ATTESTATION_MIGRATION: &str =
    include_str!("../postgres/migrations/20261021010000_create_attestation.sql");
const INBOX_MIGRATION: &str =
    include_str!("../postgres/migrations/20261020010000_create_inbox.sql");
const FIRM_INTEGRATION_SECRET_MIGRATION: &str =
    include_str!("../postgres/migrations/20261020100000_create_firm_integration_secret.sql");
const NOTATION_CLAUSE_MIGRATION: &str =
    include_str!("../postgres/migrations/20261020130000_create_notation_clause.sql");
const DBOS_WORKFLOW_REQUEST_MIGRATION: &str =
    include_str!("../postgres/migrations/20261022140000_create_dbos_workflow_request.sql");
const TENANCY_AUTHORITY_MIGRATION: &str =
    include_str!("../postgres/migrations/20261022200000_add_tenancy_authority_constraints.sql");
#[cfg(test)]
const SOURCE_DOMAIN_RECORDS_MIGRATION: &str =
    include_str!("../postgres/migrations/20261022120000_create_source_domain_records.sql");
const SURREAL_SCHEMA: &str = include_str!("schema/navigator.surql");

/// Nested source values that live in one PostgreSQL JSONB column each.
const JSONB_SOURCE_FIELDS: &[(&str, &str)] = &[
    ("post", "social_links"),
    ("communication", "call"),
    ("communication", "agreements"),
    ("matter_caption", "parties"),
    ("matter_caption", "counsel"),
    ("finance_aggregate", "parameters"),
];

/// One PostgreSQL table and its declared columns and foreign keys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Table {
    /// Lowercase SQL identifier.
    pub name: String,
    /// Declared columns in source order.
    pub columns: BTreeMap<String, String>,
    /// Foreign-key target tables in stable order.
    pub references: BTreeSet<String>,
    /// `(column, referenced table)` pairs, preserving relationship ownership.
    pub foreign_keys: BTreeSet<(String, String)>,
}

/// A mismatch between the current Surreal model and the PostgreSQL target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Difference {
    /// Surreal table absent from PostgreSQL.
    pub missing_table: Option<String>,
    /// PostgreSQL table absent from Surreal.
    pub extra_table: Option<String>,
    /// Existing table whose columns differ.
    pub table: Option<String>,
    /// Surreal column absent from PostgreSQL.
    pub missing_column: Option<String>,
    /// PostgreSQL column absent from Surreal.
    pub extra_column: Option<String>,
}

/// Failures in deterministic SQL and seed inventory parsing.
#[derive(Debug, Error)]
pub enum Error {
    /// A SQL declaration is outside the supported canonical format.
    #[error("unsupported PostgreSQL schema declaration: {0}")]
    UnsupportedSql(String),
    /// A synthetic seed file cannot be parsed.
    #[error("cannot parse seed {path}: {source}")]
    SeedYaml {
        /// Workspace-relative seed path.
        path: String,
        /// YAML error.
        source: serde_yaml::Error,
    },
    /// The deterministic JSON parity manifest could not be encoded.
    #[error("cannot serialize the parity manifest: {0}")]
    Json(#[from] serde_json::Error),
    /// The input seed document is missing its records sequence.
    #[error("seed {0} must contain a records sequence")]
    MissingRecords(String),
}

/// Parses the canonical PostgreSQL DDL in a deliberately small, stable subset.
/// The subset is sufficient for the checked-in migrations: CREATE TABLE,
/// column declarations, inline REFERENCES, named table constraints,
/// standalone indexes, and named CHECK constraints (which do not change
/// table/column parity).
#[expect(
    clippy::too_many_lines,
    reason = "single ordered pass tracks SQL table and procedural block state"
)]
pub fn parse_schema(sql: &str) -> Result<BTreeMap<String, Table>, Error> {
    let mut tables = BTreeMap::new();
    let mut current: Option<Table> = None;
    let mut procedural = false;
    let mut index_continuation = false;

    for raw in sql.lines() {
        let line = raw.trim();
        if procedural {
            if line == "$$;" || line == "END $$;" {
                procedural = false;
            }
            continue;
        }
        if index_continuation {
            if line.ends_with(';') {
                index_continuation = false;
            }
            continue;
        }
        if current.is_none()
            && (line.starts_with("CREATE OR REPLACE FUNCTION ")
                || line.starts_with("CREATE FUNCTION ")
                || line.starts_with("DO $$"))
        {
            procedural = true;
            continue;
        }
        if current.is_none() && line.starts_with("CREATE TRIGGER ") {
            index_continuation = true;
            continue;
        }
        if line.is_empty() || line.starts_with("--") || line == ")" || line == ");" {
            if line == ");" {
                if let Some(table) = current.take() {
                    tables.insert(table.name.clone(), table);
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("CREATE TABLE ") {
            if current.is_some() {
                return Err(Error::UnsupportedSql(line.to_string()));
            }
            let rest = rest.strip_prefix("IF NOT EXISTS ").unwrap_or(rest);
            let name = rest
                .split_whitespace()
                .next()
                .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?
                .trim_matches('(')
                .trim_matches('"')
                .to_string();
            current = Some(Table {
                name,
                columns: BTreeMap::new(),
                references: BTreeSet::new(),
                foreign_keys: BTreeSet::new(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("ALTER TABLE ") {
            if current.is_some() || !line.ends_with(';') {
                return Err(Error::UnsupportedSql(line.to_string()));
            }
            let words = rest
                .trim_end_matches(';')
                .split_whitespace()
                .collect::<Vec<_>>();
            if let Some((name, constraint_clause)) = rest.split_once(" ADD CONSTRAINT ") {
                if let Some((constraint, expression)) = constraint_clause.split_once(" CHECK ") {
                    let valid_name = !constraint.is_empty()
                        && constraint
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '_');
                    let expression = expression.strip_suffix(';').unwrap_or_default();
                    let valid_expression = expression.starts_with('(')
                        && expression.ends_with(')')
                        && expression.len() > 2
                        && !expression.contains("--")
                        && !expression.contains(';')
                        && expression.bytes().all(|byte| byte.is_ascii());
                    if !valid_name || !valid_expression || !tables.contains_key(name) {
                        return Err(Error::UnsupportedSql(line.to_string()));
                    }
                    let mut depth = 0_i32;
                    let mut quoted = false;
                    let mut chars = expression.chars().peekable();
                    while let Some(character) = chars.next() {
                        if character == '\'' {
                            if quoted && chars.peek() == Some(&'\'') {
                                chars.next();
                                continue;
                            }
                            quoted = !quoted;
                        } else if !quoted {
                            match character {
                                '(' => depth += 1,
                                ')' => depth -= 1,
                                _ => {}
                            }
                            if depth < 0 {
                                return Err(Error::UnsupportedSql(line.to_string()));
                            }
                        }
                    }
                    if quoted || depth != 0 {
                        return Err(Error::UnsupportedSql(line.to_string()));
                    }
                    continue;
                }
            }
            if let [name, "ADD", "CONSTRAINT", _, "FOREIGN", "KEY", column, "REFERENCES", target] =
                words.as_slice()
            {
                let column = column
                    .strip_prefix('(')
                    .and_then(|value| value.strip_suffix(')'))
                    .filter(|value| !value.is_empty() && !value.contains(','))
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                let target = target
                    .strip_suffix("(id)")
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                let table = tables
                    .get_mut(*name)
                    .filter(|table| table.columns.contains_key(column))
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                table.references.insert(target.to_string());
                table
                    .foreign_keys
                    .insert((column.to_string(), target.to_string()));
                continue;
            }
            if let [name, "ADD", "CONSTRAINT", constraint, "CHECK", ..] = words.as_slice() {
                let declaration = rest.trim_end_matches(';');
                let expression = declaration
                    .split_once(" CHECK ")
                    .map(|(_, expression)| expression)
                    .filter(|expression| valid_check_expression(expression))
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                let valid_identifier = !constraint.is_empty()
                    && constraint
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && constraint.as_bytes()[0].is_ascii_alphabetic();
                if expression.is_empty() || !valid_identifier || !tables.contains_key(*name) {
                    return Err(Error::UnsupportedSql(line.to_string()));
                }
                continue;
            }
            let (name, column, sql_type) = match words.as_slice() {
                [name, "ADD", "COLUMN", column, sql_type, ..] => (*name, *column, *sql_type),
                _ => return Err(Error::UnsupportedSql(line.to_string())),
            };
            let table = tables
                .get_mut(name)
                .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
            table
                .columns
                .insert(column.to_string(), sql_type.to_string());
            if let Some(index) = words.iter().position(|word| *word == "REFERENCES") {
                let target = words
                    .get(index + 1)
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?
                    .split('(')
                    .next()
                    .unwrap_or_default();
                table.references.insert(target.to_string());
                table
                    .foreign_keys
                    .insert((column.to_string(), target.to_string()));
            }
            continue;
        }
        if line.starts_with("CREATE INDEX ") || line.starts_with("CREATE UNIQUE INDEX ") {
            index_continuation = !line.ends_with(';');
            continue;
        }
        if let Some(table) = current.as_mut() {
            let normalized = line.trim_end_matches(',');
            let words = normalized.split_whitespace().collect::<Vec<_>>();
            let Some(first) = words.first().copied() else {
                continue;
            };
            if matches!(
                first,
                "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN"
            ) {
                if let Some(index) = words.iter().position(|word| *word == "REFERENCES") {
                    let target = words
                        .get(index + 1)
                        .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?
                        .split('(')
                        .next()
                        .unwrap_or_default();
                    table.references.insert(target.to_string());
                }
                continue;
            }
            let column = first.trim_matches('"');
            let sql_type = words
                .get(1)
                .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?
                .to_string();
            table.columns.insert(column.to_string(), sql_type);
            if let Some(index) = words.iter().position(|word| *word == "REFERENCES") {
                let target = words
                    .get(index + 1)
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?
                    .split('(')
                    .next()
                    .unwrap_or_default();
                table.references.insert(target.to_string());
                table
                    .foreign_keys
                    .insert((column.to_string(), target.to_string()));
            }
        } else {
            return Err(Error::UnsupportedSql(line.to_string()));
        }
    }

    if let Some(table) = current {
        return Err(Error::UnsupportedSql(format!(
            "unterminated CREATE TABLE {}",
            table.name
        )));
    }
    Ok(tables)
}

fn valid_check_expression(expression: &str) -> bool {
    if !expression.starts_with('(') || !expression.ends_with(')') || expression.len() <= 2 {
        return false;
    }
    let mut depth = 0_usize;
    for character in expression.chars() {
        match character {
            '(' => depth += 1,
            ')' => {
                let Some(remaining) = depth.checked_sub(1) else {
                    return false;
                };
                depth = remaining;
            }
            _ => {}
        }
    }
    depth == 0
}

/// Assembles timestamped SQL migrations in version order.
///
/// Names use `YYYYMMDDHHMMSS_description.sql`, matching Rails' migration
/// ordering while keeping each change as ordinary reviewed PostgreSQL SQL.
pub fn assemble_migrations(migrations: &[(&str, &str)]) -> Result<String, Error> {
    let mut ordered = migrations.to_vec();
    ordered.sort_by(|left, right| left.0.cmp(right.0));
    let mut previous = None;
    let mut schema = String::new();
    for (name, sql) in ordered {
        let Some((timestamp, description)) = name.split_once('_') else {
            return Err(Error::UnsupportedSql(name.to_string()));
        };
        if timestamp.len() != 14
            || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
            || description.is_empty()
            || std::path::Path::new(name)
                .extension()
                .is_none_or(|extension| extension != "sql")
            || previous.is_some_and(|version| version >= timestamp)
        {
            return Err(Error::UnsupportedSql(name.to_string()));
        }
        previous = Some(timestamp);
        schema.push_str("-- Migration: ");
        schema.push_str(name);
        schema.push('\n');
        schema.push_str(sql.trim());
        schema.push_str("\n\n");
    }
    // Keep the blank separator between migrations without adding a second
    // trailing blank line to the assembled schema file.
    if schema.ends_with("\n\n") {
        schema.pop();
    }
    Ok(schema)
}

/// Returns differences in stable table and column order.
#[must_use]
pub fn compare_to_surreal(
    postgres: &BTreeMap<String, Table>,
    surreal_tables: &[String],
    surreal_columns: impl Fn(&str) -> Vec<(String, String)>,
) -> Vec<Difference> {
    let surreal: BTreeSet<_> = surreal_tables.iter().cloned().collect();
    let postgres_names: BTreeSet<_> = postgres.keys().cloned().collect();
    let mut differences = Vec::new();

    for name in surreal.difference(&postgres_names) {
        differences.push(Difference {
            missing_table: Some(name.clone()),
            extra_table: None,
            table: None,
            missing_column: None,
            extra_column: None,
        });
    }
    for name in postgres_names.difference(&surreal) {
        differences.push(Difference {
            missing_table: None,
            extra_table: Some(name.clone()),
            table: None,
            missing_column: None,
            extra_column: None,
        });
    }
    for name in surreal.intersection(&postgres_names) {
        let target: BTreeSet<_> = postgres[name].columns.keys().cloned().collect();
        let source: BTreeSet<_> = surreal_columns(name)
            .into_iter()
            .filter_map(|(column, _)| {
                let jsonb_parent = JSONB_SOURCE_FIELDS.iter().find_map(|(table, field)| {
                    (*table == name.as_str() && column.starts_with(&format!("{field}.")))
                        .then_some(*field)
                });
                if let Some(parent) = jsonb_parent {
                    target.contains(parent).then_some(parent.to_string())
                } else {
                    Some(column)
                }
            })
            .collect();
        for column in source.difference(&target) {
            differences.push(Difference {
                missing_table: None,
                extra_table: None,
                table: Some(name.clone()),
                missing_column: Some(column.clone()),
                extra_column: None,
            });
        }
        for column in target.difference(&source) {
            differences.push(Difference {
                missing_table: None,
                extra_table: None,
                table: Some(name.clone()),
                missing_column: None,
                extra_column: Some(column.clone()),
            });
        }
    }
    for (table, columns) in surreal.iter().map(|table| (table, surreal_columns(table))) {
        for (column, surreal_type) in columns {
            let Some(target) = postgres.get(table) else {
                continue;
            };
            if let Some(referenced_table) = surreal_record_target(&surreal_type) {
                let has_foreign_key = target
                    .foreign_keys
                    .contains(&(column.clone(), referenced_table.clone()));
                // A UUID column is enough to preserve the exact source link,
                // including a historical orphan. New source-domain tables
                // intentionally keep these values without a PostgreSQL FK so
                // import never drops or rewrites an orphaned reference.
                let preserves_uuid = postgres
                    .get(table)
                    .and_then(|target_table| target_table.columns.get(&column))
                    .is_some_and(|sql_type| sql_type.eq_ignore_ascii_case("UUID"));
                if !has_foreign_key && !preserves_uuid {
                    differences.push(Difference {
                        missing_table: None,
                        extra_table: None,
                        table: Some(table.clone()),
                        missing_column: Some(format!("relationship {column} → {referenced_table}")),
                        extra_column: None,
                    });
                }
            }
        }
    }
    differences.sort_by_key(|difference| {
        (
            difference.missing_table.clone(),
            difference.extra_table.clone(),
            difference.table.clone(),
            difference.missing_column.clone(),
            difference.extra_column.clone(),
        )
    });
    differences
}

/// Renders a stable Mermaid ERD from the canonical PostgreSQL schema.
pub fn render_erd() -> Result<String, Error> {
    let tables = parse_schema(CANONICAL_SCHEMA)?;
    let mut output = String::from("erDiagram\n");
    for table in tables.values() {
        let _ = writeln!(output, "    {} {{", table.name);
        for (column, sql_type) in &table.columns {
            let _ = writeln!(output, "        {} {}", erd_type(sql_type), column);
        }
        output.push_str("    }\n");
    }
    for table in tables.values() {
        for target in &table.references {
            if tables.contains_key(target) {
                let _ = writeln!(output, "    {} }}o--|| {} : references", target, table.name);
            }
        }
    }
    Ok(output)
}

/// Renders every table, field, and declared relationship in the current
/// Surreal schema. This source inventory remains complete while PostgreSQL
/// migrations are still being built out.
#[must_use]
pub fn render_surreal_source_erd() -> String {
    let tables = crate::schema::table_names();
    let mut output = String::from("erDiagram\n");
    for table in &tables {
        let _ = writeln!(output, "    {table} {{");
        let mut columns = crate::schema::table_columns(table);
        if columns.is_empty() {
            columns.push(("id".to_string(), "record".to_string()));
        }
        for (column, surreal_type) in columns {
            let _ = writeln!(
                output,
                "        {} {}",
                surreal_erd_type(&surreal_type),
                column
            );
        }
        output.push_str("    }\n");
    }
    for table in &tables {
        for (column, surreal_type) in crate::schema::table_columns(table) {
            if let Some(target) = surreal_record_target(&surreal_type) {
                let _ = writeln!(output, "    {target} }}o--|| {table} : {column}");
            }
        }
    }
    for line in SURREAL_SCHEMA.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("DEFINE TABLE IF NOT EXISTS ") else {
            continue;
        };
        let Some((relation, declaration)) = rest.split_once(" TYPE RELATION FROM ") else {
            continue;
        };
        let Some((from, to)) = declaration.split_once(" TO ") else {
            continue;
        };
        let from_types = from
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .split('|');
        let to_types = to.split_whitespace().next().unwrap_or_default().split('|');
        for from_type in from_types.clone() {
            for to_type in to_types.clone() {
                let _ = writeln!(output, "    {from_type} }}o--o{{ {to_type} : {relation}");
            }
        }
    }
    output
}

/// Deterministic JSON inventory of every source table/field and PostgreSQL
/// coverage difference. BTree collections make byte output stable.
pub fn render_parity_manifest() -> Result<String, Error> {
    let postgres = parse_schema(CANONICAL_SCHEMA)?;
    let source_tables = crate::schema::table_names();
    let source = source_tables
        .iter()
        .map(|name| {
            let mut columns = crate::schema::table_columns(name);
            if columns.is_empty() {
                columns.push(("id".to_string(), "record".to_string()));
            }
            (
                name.clone(),
                columns.into_iter().collect::<BTreeMap<_, _>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut differences =
        compare_to_surreal(&postgres, &source_tables, crate::schema::table_columns);
    differences.sort_by_key(|difference| {
        (
            difference.missing_table.clone(),
            difference.extra_table.clone(),
            difference.table.clone(),
            difference.missing_column.clone(),
            difference.extra_column.clone(),
        )
    });
    let manifest = serde_json::json!({
        "source": {
            "name": "SurrealDB",
            "table_count": source_tables.len(),
            "tables": source,
        },
        "target": {
            "name": "PostgreSQL",
            "table_count": postgres.len(),
            "tables": postgres,
        },
        "differences": differences,
    });
    serde_json::to_string_pretty(&manifest)
        .map(|json| format!("{json}\n"))
        .map_err(Error::Json)
}

fn surreal_record_target(surreal_type: &str) -> Option<String> {
    let unwrapped = surreal_type
        .strip_prefix("option<")
        .and_then(|ty| ty.strip_suffix('>'))
        .unwrap_or(surreal_type);
    unwrapped
        .strip_prefix("record<")
        .and_then(|ty| ty.strip_suffix('>'))
        .map(ToOwned::to_owned)
}

fn surreal_erd_type(surreal_type: &str) -> &'static str {
    if surreal_record_target(surreal_type).is_some() || surreal_type == "uuid" {
        "uuid"
    } else if surreal_type.contains("bool") {
        "boolean"
    } else if surreal_type.contains("int") || surreal_type.contains("float") {
        "number"
    } else if surreal_type.contains("datetime") {
        "datetime"
    } else if surreal_type == "any"
        || surreal_type.starts_with("array")
        || surreal_type.starts_with("object")
    {
        "json"
    } else {
        "string"
    }
}

fn erd_type(sql_type: &str) -> &'static str {
    match sql_type {
        "UUID" => "uuid",
        "BOOLEAN" => "boolean",
        "BIGINT" | "INTEGER" | "SMALLINT" => "int",
        "TIMESTAMPTZ" | "TIMESTAMP" | "DATE" => "datetime",
        "JSONB" | "JSON" => "json",
        _ => "string",
    }
}

/// Counts target seed coverage without materializing any seed rows.
///
/// The input is the existing synthetic YAML catalog. Filename-to-table
/// mapping follows its CamelCase filenames, with `User.yaml` intentionally
/// mapped to `person` because it supplies Person role defaults.
pub fn seed_schema_differences(
    seeds: &[(&str, &str)],
    postgres: &BTreeMap<String, Table>,
) -> Result<Vec<String>, Error> {
    let mut missing = BTreeSet::new();
    for (path, yaml) in seeds {
        let value: Value = serde_yaml::from_str(yaml).map_err(|source| Error::SeedYaml {
            path: (*path).to_string(),
            source,
        })?;
        let records = value
            .get("records")
            .and_then(Value::as_sequence)
            .ok_or_else(|| Error::MissingRecords((*path).to_string()))?;
        let table_name = seed_table_name(path);
        let user_role_seed = path.rsplit('/').next() == Some("User.yaml");
        // A Brand is named by its key, not a surrogate id, in the seeds whose
        // tables belong to a Brand's Posts.
        let brand_keyed_seed = matches!(
            path.rsplit('/').next(),
            Some("Post.yaml" | "SocialHandle.yaml")
        );
        let question_seed = path.rsplit('/').next() == Some("Question.yaml");
        let Some(table) = postgres.get(&table_name) else {
            missing.insert(format!("{path}: target table {table_name} is absent"));
            continue;
        };
        if let Some(lookup_fields) = value.get("lookup_fields").and_then(Value::as_sequence) {
            for field in lookup_fields.iter().filter_map(Value::as_str) {
                let root = if user_role_seed && field == "person_id" {
                    "email"
                } else {
                    field.split('.').next().unwrap_or(field)
                };
                let column = if brand_keyed_seed && root == "brand" {
                    "brand_key"
                } else {
                    lookup_column(root)
                };
                if !table.columns.contains_key(column) {
                    missing.insert(format!(
                        "{path}: lookup field {field} requires {table_name}.{column}"
                    ));
                }
            }
        }
        for (record_index, record) in records.iter().enumerate() {
            if let Some(fields) = record.as_mapping() {
                for key in fields.keys().filter_map(Value::as_str) {
                    if user_role_seed && key == "person" {
                        if let Some(person) = fields.get(Value::String(key.to_string())) {
                            if let Some(person_fields) = person.as_mapping() {
                                for nested_key in person_fields.keys().filter_map(Value::as_str) {
                                    if !table.columns.contains_key(nested_key) {
                                        missing.insert(format!(
                                            "{path}: record {} uses person.{nested_key}, absent from PostgreSQL",
                                            record_index + 1
                                        ));
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    // A Question's `help_text` is read from the YAML where it
                    // renders and never stored; its `question_type` is
                    // stored as `answer_type`.
                    if question_seed && key == "help_text" {
                        continue;
                    }
                    let column = if brand_keyed_seed && key == "brand" {
                        "brand_key"
                    } else if table_name == "post" && key == "social" {
                        "social_links"
                    } else if question_seed && key == "question_type" {
                        "answer_type"
                    } else {
                        lookup_column(key)
                    };
                    if !table.columns.contains_key(column) {
                        missing.insert(format!(
                            "{path}: record {} uses {table_name}.{column}, absent from PostgreSQL",
                            record_index + 1
                        ));
                    }
                }
            }
        }
    }
    Ok(missing.into_iter().collect())
}

fn lookup_column(field: &str) -> &str {
    match field {
        "project" | "person" | "entity" | "firm" | "brand" | "jurisdiction" | "entity_type"
        | "mailroom" | "address" | "asset" | "question" => {
            // Nested YAML references resolve through `<entity>_id` columns.
            // Direct scalar columns already end in `_id` and pass through.
            match field {
                "project" => "project_id",
                "person" => "person_id",
                "entity" => "entity_id",
                "firm" => "firm_id",
                "brand" => "brand_id",
                "jurisdiction" => "jurisdiction_id",
                "entity_type" => "entity_type_id",
                "mailroom" => "mailroom_id",
                "address" => "address_id",
                "asset" => "asset_id",
                "question" => "question_id",
                _ => field,
            }
        }
        _ => field,
    }
}

fn seed_table_name(path: &str) -> String {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let stem = filename.strip_suffix(".yaml").unwrap_or(filename);
    if stem == "User" {
        return "person".to_string();
    }
    let mut result = String::new();
    for character in stem.chars() {
        if character.is_ascii_uppercase() {
            if !result.is_empty() {
                result.push('_');
            }
            result.push(character.to_ascii_lowercase());
        } else {
            result.push(character);
        }
    }
    result
}

/// Canonical PostgreSQL schema source, assembled from ordered migration files.
#[must_use]
pub const fn canonical_schema() -> &'static str {
    CANONICAL_SCHEMA
}

/// The migration that introduces durable Notation attestations.
#[must_use]
pub const fn attestation_migration() -> &'static str {
    ATTESTATION_MIGRATION
}

/// The initial checked-in migration used to verify the schema assembly.
#[must_use]
pub const fn initial_migration() -> &'static str {
    INITIAL_MIGRATION
}

/// The timestamped PostgreSQL migration that introduces the `post` table.
#[must_use]
pub const fn post_migration() -> &'static str {
    POST_MIGRATION
}

#[must_use]
pub const fn blog_video_migration() -> &'static str {
    BLOG_VIDEO_MIGRATION
}

/// The timestamped PostgreSQL migration that introduces `social_handle` and
/// `social_publication`, the tables a Post needs beside itself.
#[must_use]
pub const fn social_migration() -> &'static str {
    SOCIAL_MIGRATION
}

/// The relational, operational Firm journal migration.
#[must_use]
pub const fn journal_migration() -> &'static str {
    JOURNAL_MIGRATION
}

/// The migration that introduces Communications and email conversation tables.
#[must_use]
pub const fn communications_migration() -> &'static str {
    COMMUNICATIONS_MIGRATION
}

/// The relational Project cost records and audit events.
#[must_use]
pub const fn project_costs_migration() -> &'static str {
    PROJECT_COSTS_MIGRATION
}

/// The migration that introduces verified Project Reference rows.
#[must_use]
pub const fn project_reference_migration() -> &'static str {
    PROJECT_REFERENCE_MIGRATION
}

/// The timestamped migration that mirrors inbound email receipts and delivery state.
#[must_use]
pub const fn email_delivery_migration() -> &'static str {
    EMAIL_DELIVERY_MIGRATION
}

/// The migration that introduces physical Mailroom and Letter tables.
#[must_use]
pub const fn mailrooms_letters_migration() -> &'static str {
    MAILROOMS_LETTERS_MIGRATION
}

/// The migration that introduces public Leads and immutable consent history.
#[must_use]
pub const fn leads_migration() -> &'static str {
    LEADS_MIGRATION
}

/// The migration that introduces synced Matter Pages and Matter Captions.
#[must_use]
pub const fn matter_pages_captions_migration() -> &'static str {
    MATTER_PAGES_CAPTIONS_MIGRATION
}

/// The migration that mirrors Firm Inbox conversations and their transcript.
#[must_use]
pub const fn inbox_migration() -> &'static str {
    INBOX_MIGRATION
}

/// The migration that mirrors custom clauses attached to a Notation.
#[must_use]
pub const fn notation_clause_migration() -> &'static str {
    NOTATION_CLAUSE_MIGRATION
}

#[must_use]
pub const fn firm_integration_secret_migration() -> &'static str {
    FIRM_INTEGRATION_SECRET_MIGRATION
}

#[must_use]
pub const fn dbos_workflow_request_migration() -> &'static str {
    DBOS_WORKFLOW_REQUEST_MIGRATION
}

#[must_use]
pub const fn tenancy_authority_migration() -> &'static str {
    TENANCY_AUTHORITY_MIGRATION
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_ERD: &str = include_str!("../postgres/surreal-source-erd.mmd");
    const PARITY_MANIFEST: &str = include_str!("../postgres/parity-manifest.json");

    #[test]
    fn inbox_tables_preserve_every_source_column() {
        let target = parse_schema(canonical_schema()).expect("canonical schema parses");
        for name in [
            "inbox_conversation",
            "message",
            "inbox_ingress",
            "inbox_assignment",
            "inbox_delivery_attempt",
        ] {
            let source = crate::schema::table_columns(name);
            let columns = &target[name].columns;
            assert_eq!(
                source
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<BTreeSet<_>>(),
                columns.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                "{name} column parity"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // The explicit ordered migration inventory is the contract under test.
    fn canonical_schema_is_the_ordered_migration_assembly() {
        let assembled = assemble_migrations(&[
            (
                "20261010120000_create_core_tenancy.sql",
                initial_migration(),
            ),
            ("20261010130000_create_post.sql", post_migration()),
            (
                "20261011120000_create_social_handle_and_publication.sql",
                social_migration(),
            ),
            (
                "20261011200000_create_firm_journal.sql",
                journal_migration(),
            ),
            (
                "20261012210000_create_clause.sql",
                include_str!("../postgres/migrations/20261012210000_create_clause.sql"),
            ),
            (
                "20261013020000_create_tenancy_membership.sql",
                include_str!("../postgres/migrations/20261013020000_create_tenancy_membership.sql"),
            ),
            (
                "20261013030000_add_brand_website_url.sql",
                include_str!("../postgres/migrations/20261013030000_add_brand_website_url.sql"),
            ),
            (
                "20261013100000_create_identity_detail.sql",
                include_str!("../postgres/migrations/20261013100000_create_identity_detail.sql"),
            ),
            (
                "20261013110000_create_catalog.sql",
                include_str!("../postgres/migrations/20261013110000_create_catalog.sql"),
            ),
            (
                "20261013120001_add_asset_retention.sql",
                include_str!("../postgres/migrations/20261013120001_add_asset_retention.sql"),
            ),
            (
                "20261013120002_create_notation_workflow.sql",
                include_str!("../postgres/migrations/20261013120002_create_notation_workflow.sql"),
            ),
            (
                "20261013130000_add_firm_slug.sql",
                include_str!("../postgres/migrations/20261013130000_add_firm_slug.sql"),
            ),
            (
                "20261015203853_create_summary_sender_approvals.sql",
                include_str!(
                    "../postgres/migrations/20261015203853_create_summary_sender_approvals.sql"
                ),
            ),
            (
                "20261015220000_create_project_deadlines.sql",
                include_str!("../postgres/migrations/20261015220000_create_project_deadlines.sql"),
            ),
            (
                "20261016100000_create_project_costs.sql",
                project_costs_migration(),
            ),
            (
                "20261016110000_create_communications_conversations.sql",
                communications_migration(),
            ),
            (
                "20261016120000_create_template.sql",
                include_str!("../postgres/migrations/20261016120000_create_template.sql"),
            ),
            (
                "20261016123000_create_sent_email.sql",
                include_str!("../postgres/migrations/20261016123000_create_sent_email.sql"),
            ),
            (
                "20261016130000_add_workflow_template_pins.sql",
                include_str!(
                    "../postgres/migrations/20261016130000_add_workflow_template_pins.sql"
                ),
            ),
            (
                "20261016140000_create_project_service.sql",
                include_str!("../postgres/migrations/20261016140000_create_project_service.sql"),
            ),
            (
                "20261017020000_create_project_reference.sql",
                project_reference_migration(),
            ),
            (
                "20261017030000_create_email_receipts_and_delivery.sql",
                email_delivery_migration(),
            ),
            (
                "20261017100000_create_social_account.sql",
                include_str!("../postgres/migrations/20261017100000_create_social_account.sql"),
            ),
            (
                "20261017110000_create_mailrooms_letters.sql",
                mailrooms_letters_migration(),
            ),
            ("20261017120000_create_leads.sql", leads_migration()),
            (
                "20261018120000_create_matter_pages_and_captions.sql",
                matter_pages_captions_migration(),
            ),
            (
                "20261018150000_create_project_archive.sql",
                include_str!("../postgres/migrations/20261018150000_create_project_archive.sql"),
            ),
            (
                "20261021130000_create_social_account_selection.sql",
                include_str!("../postgres/migrations/20261021130000_create_social_account_selection.sql"),
            ),
            (
                "20261018160000_create_project_source_import.sql",
                include_str!(
                    "../postgres/migrations/20261018160000_create_project_source_import.sql"
                ),
            ),
            (
                "20261018170000_create_document_retention.sql",
                include_str!("../postgres/migrations/20261018170000_create_document_retention.sql"),
            ),
            (
                "20261018180000_create_project_handoff.sql",
                include_str!("../postgres/migrations/20261018180000_create_project_handoff.sql"),
            ),
            (
                "20261018190000_create_project_handoff_acknowledgment.sql",
                include_str!(
                    "../postgres/migrations/20261018190000_create_project_handoff_acknowledgment.sql"
                ),
            ),
            ("20261020010000_create_inbox.sql", inbox_migration()),
            (
                "20261020100000_create_firm_integration_secret.sql",
                firm_integration_secret_migration(),
            ),
            (
                "20261020120000_create_purchase.sql",
                include_str!("../postgres/migrations/20261020120000_create_purchase.sql"),
            ),
            (
                "20261020130000_create_notation_clause.sql",
                notation_clause_migration(),
            ),
            (
                "20261021010000_create_attestation.sql",
                attestation_migration(),
            ),
            (
                "20261021140000_create_project_records.sql",
                include_str!("../postgres/migrations/20261021140000_create_project_records.sql"),
            ),
            (
                "20261021143000_create_notation_inputs.sql",
                include_str!("../postgres/migrations/20261021143000_create_notation_inputs.sql"),
            ),
            (
                "20261021150000_create_git_access.sql",
                include_str!("../postgres/migrations/20261021150000_create_git_access.sql"),
            ),
            (
                "20261021160000_create_mirror_backfill.sql",
                include_str!("../postgres/migrations/20261021160000_create_mirror_backfill.sql"),
            ),
            ("20261021170000_create_blog_video.sql", blog_video_migration()),
            (
                "20261022140000_create_dbos_workflow_request.sql",
                dbos_workflow_request_migration(),
            ),
            (
                "20261022200000_add_tenancy_authority_constraints.sql",
                tenancy_authority_migration(),
            ),
            (
                "20261022120000_create_source_domain_records.sql",
                SOURCE_DOMAIN_RECORDS_MIGRATION,
            ),
            (
                "20261022210000_enforce_project_code.sql",
                include_str!("../postgres/migrations/20261022210000_enforce_project_code.sql"),
            ),
            (
                "20261025120000_add_project_deadline_title.sql",
                include_str!("../postgres/migrations/20261025120000_add_project_deadline_title.sql"),
            ),
            (
                "20261026120000_add_deadline_calendar_sequence.sql",
                include_str!("../postgres/migrations/20261026120000_add_deadline_calendar_sequence.sql"),
            ),
            (
                "20261026130000_add_deadline_description.sql",
                include_str!("../postgres/migrations/20261026130000_add_deadline_description.sql"),
            ),
            (
                "20261029010000_add_communication_receipts.sql",
                include_str!("../postgres/migrations/20261029010000_add_communication_receipts.sql"),
            ),
            (
                "20261027120000_add_project_calendar_and_booking_url.sql",
                include_str!(
                    "../postgres/migrations/20261027120000_add_project_calendar_and_booking_url.sql"
                ),
            ),
            (
                "20261030010000_add_template_firm.sql",
                include_str!("../postgres/migrations/20261030010000_add_template_firm.sql"),
            ),
            (
                "20261030020000_add_asset_evidence.sql",
                include_str!("../postgres/migrations/20261030020000_add_asset_evidence.sql"),
            ),
            (
                "20261030030000_create_inbox_message_asset.sql",
                include_str!("../postgres/migrations/20261030030000_create_inbox_message_asset.sql"),
            ),
            (
                "20261030120000_create_project_task.sql",
                include_str!("../postgres/migrations/20261030120000_create_project_task.sql"),
            ),
            (
                "20261031010000_create_navigator_graph.sql",
                include_str!("../postgres/migrations/20261031010000_create_navigator_graph.sql"),
            ),
            (
                "20261031020000_create_graph_view_readers.sql",
                include_str!("../postgres/migrations/20261031020000_create_graph_view_readers.sql"),
            ),
            (
                "20261031120000_add_project_onboarding_jurisdiction.sql",
                include_str!("../postgres/migrations/20261031120000_add_project_onboarding_jurisdiction.sql"),
            ),
        ])
        .expect("timestamped migration assembles");
        assert_eq!(canonical_schema().trim(), assembled.trim());
    }

    #[test]
    fn sent_email_audit_has_identity_scope_and_timestamps() {
        let schema = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let email = schema.get("sent_email").expect("outbound audit table");
        for column in [
            "id",
            "recipient",
            "subject",
            "sender",
            "project_id",
            "template_slug",
            "body",
            "outcome",
            "sg_message_id",
            "sent_at",
            "inserted_at",
            "updated_at",
        ] {
            assert!(email.columns.contains_key(column), "sent_email.{column}");
        }
        for column in ["sent_at", "inserted_at", "updated_at"] {
            assert_eq!(email.columns[column], "TIMESTAMPTZ");
        }
        assert!(email.references.contains("project"));
    }

    #[test]
    fn social_account_mirror_keeps_encrypted_bundle_and_ownership_links() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let account = tables
            .get("social_account")
            .expect("Social Account mirror is present");
        for column in [
            "id",
            "firm_id",
            "brand_key",
            "provider",
            "provider_account_id",
            "display_name",
            "scopes",
            "expires_at",
            "ciphertext",
            "wrapped_dek",
            "kms_key_version",
            "kms_context",
            "status",
            "connected_by_person_id",
            "revoked_by_person_id",
            "revoked_at",
            "inserted_at",
            "updated_at",
        ] {
            assert!(
                account.columns.contains_key(column),
                "social_account.{column}"
            );
        }
        assert!(account.references.contains("firm"));
        assert!(account.references.contains("brand"));
        assert!(account.references.contains("person"));
    }

    #[test]
    fn matter_pages_and_captions_carry_their_project_and_firm() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for name in ["matter_page", "matter_caption"] {
            let table = tables.get(name).expect("matter target table");
            assert_eq!(
                table.foreign_keys,
                [
                    ("firm_id".into(), "firm".into()),
                    ("project_id".into(), "project".into())
                ]
                .into(),
                "{name}"
            );
        }
        let page = &tables["matter_page"];
        for lens in ["client_sections", "lawyer_sections", "clerk_sections"] {
            assert_eq!(page.columns[lens], "TEXT[]");
        }
        let caption = &tables["matter_caption"];
        assert_eq!(caption.columns["parties"], "JSONB");
        assert_eq!(caption.columns["counsel"], "JSONB");
        let matter_tables = ["matter_page", "matter_caption"];
        let differences: Vec<_> = compare_to_surreal(
            &tables,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        )
        .into_iter()
        .filter(|difference| {
            [&difference.table, &difference.missing_table]
                .into_iter()
                .flatten()
                .any(|table| matter_tables.contains(&table.as_str()))
        })
        .collect();
        assert_eq!(
            differences
                .iter()
                .map(|difference| (
                    difference.table.as_deref(),
                    difference.extra_column.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                (Some("matter_caption"), Some("firm_id")),
                (Some("matter_page"), Some("firm_id"))
            ],
            "only the denormalized owning Firm differs from the source: {differences:?}"
        );
        for statement in [
            "FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE",
            "project_id UUID NOT NULL UNIQUE REFERENCES project(id)",
        ] {
            assert!(
                matter_pages_captions_migration().contains(statement),
                "{statement}"
            );
        }
    }

    #[test]
    fn mail_rows_keep_source_parent_links_and_weak_optional_project_attribution() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let mailroom = tables.get("mailroom").expect("Mailroom target table");
        assert_eq!(
            mailroom.foreign_keys,
            [("address_id".into(), "address".into())].into()
        );
        let letter = tables.get("letter").expect("Letter target table");
        assert_eq!(
            letter.foreign_keys,
            [("mailroom_id".into(), "mailroom".into())].into()
        );
        assert_eq!(letter.columns["project_id"], "UUID");
        assert!(!letter
            .foreign_keys
            .iter()
            .any(|(column, _)| column == "project_id"));
    }

    #[test]
    fn project_service_mirror_preserves_project_document_and_sender_scope() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let service = tables
            .get("project_service")
            .expect("Project Service is mirrored to PostgreSQL");
        for column in [
            "id",
            "project_id",
            "document_id",
            "recipient",
            "recipient_person_id",
            "recipient_entity_id",
            "counsel_for",
            "method",
            "served_on",
            "address",
            "declarant",
            "declarant_person_id",
            "letter_id",
            "sent_email_id",
            "inserted_at",
            "updated_at",
        ] {
            assert!(
                service.columns.contains_key(column),
                "project_service.{column}"
            );
        }
        assert_eq!(
            service.foreign_keys,
            BTreeSet::from([
                ("project_id".into(), "project".into()),
                ("document_id".into(), "asset".into()),
                ("recipient_person_id".into(), "person".into()),
                ("recipient_entity_id".into(), "entity".into()),
                ("declarant_person_id".into(), "person".into()),
                ("sent_email_id".into(), "sent_email".into()),
            ])
        );
        assert!(crate::schema::table_names().contains(&"project_service".to_string()));
    }

    #[test]
    fn workflow_template_pins_are_foreign_keys() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for (table, column) in [
            ("notation", "template_id"),
            ("notation_event", "template_version_id"),
            ("notation_package_template", "template_id"),
        ] {
            assert!(
                tables[table]
                    .foreign_keys
                    .contains(&(column.into(), "template".into())),
                "{table}.{column} must pin a retained Template version"
            );
        }
    }

    #[test]
    fn clause_is_a_target_only_postgresql_table() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let clause = postgres
            .get("clause")
            .expect("Clause is a PostgreSQL table");
        for column in [
            "id",
            "code",
            "version",
            "kind",
            "inputs",
            "body",
            "body_sha256",
            "is_current",
            "retired_at",
            "inserted_at",
        ] {
            assert!(clause.columns.contains_key(column), "clause.{column}");
        }
        assert_eq!(clause.columns["inputs"], "JSONB");
        assert!(clause.references.is_empty(), "a shared catalog");
        assert!(!crate::schema::table_names().contains(&"clause".to_string()));
    }

    #[test]
    fn template_mirror_preserves_version_scope_and_current_key() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let template = postgres
            .get("template")
            .expect("Template is mirrored to PostgreSQL");
        for column in [
            "id",
            "code",
            "title",
            "respondent_type",
            "project_id",
            "firm_id",
            "is_current",
            "asset_id",
            "form_code",
            "kind",
            "source_commit_sha",
            "body_sha256",
            "current_key",
            "inserted_at",
            "updated_at",
        ] {
            assert!(template.columns.contains_key(column), "template.{column}");
        }
        assert_eq!(
            template.foreign_keys,
            BTreeSet::from([
                ("project_id".to_string(), "project".to_string()),
                ("firm_id".to_string(), "firm".to_string()),
                ("asset_id".to_string(), "asset".to_string()),
            ])
        );
        assert!(crate::schema::table_names().contains(&"template".to_string()));
    }

    #[test]
    fn post_is_a_postgresql_table_and_the_seed_is_covered() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let post = postgres
            .get("post")
            .expect("the PostgreSQL target table uses the ontology name Post");
        assert!(post.columns.contains_key("body_markdown"));
        assert!(!postgres.contains_key("blog_post"));
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert!(
            !parity
                .iter()
                .any(|difference| difference.table.as_deref() == Some("post")),
            "{parity:?}"
        );
        let seed = include_str!("../seeds/neon/Post.yaml");
        let seed_differences = seed_schema_differences(&[("neon/Post.yaml", seed)], &postgres)
            .expect("Post seed parses");
        assert!(seed_differences.is_empty(), "{seed_differences:?}");
    }

    #[test]
    fn a_posts_handles_and_publications_are_postgresql_tables_with_their_seed() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for table in ["social_handle", "social_publication"] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
        }
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in ["social_handle", "social_publication"] {
            assert!(
                !parity
                    .iter()
                    .any(|difference| difference.table.as_deref() == Some(table)),
                "{table}: {parity:?}"
            );
        }
        assert!(
            postgres["social_publication"]
                .foreign_keys
                .contains(&("post_id".to_string(), "post".to_string())),
            "a publication belongs to a Post"
        );
        let seed = include_str!("../seeds/neon/SocialHandle.yaml");
        let differences = seed_schema_differences(&[("neon/SocialHandle.yaml", seed)], &postgres)
            .expect("SocialHandle seed parses");
        assert!(differences.is_empty(), "{differences:?}");
    }

    #[test]
    fn catalog_tables_match_their_surreal_source_and_the_question_seed() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in [
            "authority",
            "glossary_term",
            "question",
            "notation_package",
            "notation_package_template",
            "notation_rate",
        ] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            let differences: Vec<_> = parity
                .iter()
                .filter(|difference| difference.table.as_deref() == Some(table))
                .collect();
            assert!(differences.is_empty(), "{table}: {differences:?}");
        }
        let seed = include_str!("../seeds/Question.yaml");
        let differences = seed_schema_differences(&[("Question.yaml", seed)], &postgres)
            .expect("Question seed parses");
        assert!(differences.is_empty(), "{differences:?}");
    }

    #[test]
    fn firm_brand_and_membership_tables_match_their_surreal_source() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in ["firm_brand", "person_firm_role", "person_project_role"] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            assert!(
                !parity
                    .iter()
                    .any(|difference| difference.table.as_deref() == Some(table)),
                "{table}: {parity:?}"
            );
        }
        for (table, column, target) in [
            ("firm_brand", "firm_id", "firm"),
            ("person_firm_role", "firm_id", "firm"),
            ("person_firm_role", "person_id", "person"),
            ("person_project_role", "project_id", "project"),
            ("person_project_role", "person_id", "person"),
        ] {
            assert!(
                postgres[table]
                    .foreign_keys
                    .contains(&(column.to_string(), target.to_string())),
                "{table}.{column} references {target}"
            );
        }
        assert!(postgres["firm_brand"].references.contains("brand"));
    }

    #[test]
    fn identity_detail_tables_match_their_surreal_source() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in [
            "address",
            "credential",
            "email_token",
            "entity_role",
            "person_delegate",
            "person_external_identity",
            "person_mailbox",
        ] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            let differences: Vec<_> = parity
                .iter()
                .filter(|difference| difference.table.as_deref() == Some(table))
                .map(|difference| difference.extra_column.as_deref())
                .collect();
            // A Surreal relation keeps its endpoints in `in` and `out`, which
            // the source inventory does not list as fields.
            let expected: &[Option<&str>] = if table == "entity_role" {
                &[Some("entity_id"), Some("person_id")]
            } else {
                &[]
            };
            assert_eq!(differences, expected, "{table}: {parity:?}");
        }
        assert_eq!(postgres["person_mailbox"].columns["id"], "TEXT");
        assert!(postgres["entity_role"]
            .foreign_keys
            .contains(&("person_id".to_string(), "person".to_string())));
        assert!(postgres["entity_role"]
            .foreign_keys
            .contains(&("entity_id".to_string(), "entity".to_string())));
    }

    #[test]
    fn one_line_add_constraint_check_is_validated_without_changing_parity_shape() {
        let ddl = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));";
        let tables = parse_schema(ddl).expect("narrow one-line CHECK parses");
        assert_eq!(tables["brand"].columns.len(), 1);
        assert!(parse_schema("ALTER TABLE brand ADD CONSTRAINT x CHECK (a);\n").is_err());
        assert!(parse_schema("ALTER TABLE absent ADD CONSTRAINT x CHECK (a);\n").is_err());
        assert!(parse_schema("ALTER TABLE brand ADD CONSTRAINT x CHECK a;\n").is_err());
    }

    #[test]
    fn an_added_column_joins_its_table_and_brand_carries_its_website_url() {
        let sql = "CREATE TABLE firm (\n id UUID PRIMARY KEY\n);\n\
                   CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD COLUMN firm_id UUID REFERENCES firm(id);\n\
                   ALTER TABLE brand ADD COLUMN website_url TEXT CHECK (website_url <> '');";
        let tables = parse_schema(sql).expect("ALTER TABLE ADD COLUMN parses");
        assert_eq!(tables["brand"].columns["website_url"], "TEXT");
        assert!(tables["brand"]
            .foreign_keys
            .contains(&("firm_id".to_string(), "firm".to_string())));
        assert!(parse_schema("ALTER TABLE brand DROP COLUMN name;").is_err());
        assert!(parse_schema("ALTER TABLE absent ADD COLUMN name TEXT;").is_err());

        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        assert!(postgres["brand"].columns.contains_key("website_url"));
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert!(
            !parity.iter().any(|difference| {
                difference.table.as_deref() == Some("brand")
                    && (difference.missing_column.as_deref() == Some("website_url")
                        || difference.extra_column.as_deref() == Some("website_url"))
            }),
            "{parity:?}"
        );
    }

    #[test]
    fn a_named_check_constraint_on_an_existing_table_parses() {
        let sql = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));";
        let tables = parse_schema(sql).expect("named CHECK constraint parses");
        assert!(tables.contains_key("brand"));
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT broken CHECK;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE absent ADD CONSTRAINT missing CHECK (id IS NOT NULL);"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT unbalanced CHECK (id IS NOT NULL;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT invalid-name CHECK (id IS NOT NULL);"
        )
        .is_err());
    }

    #[test]
    fn migrations_sort_by_timestamp_and_reject_invalid_versions() {
        let assembled = assemble_migrations(&[
            ("20261011120000_second.sql", "SELECT 2;"),
            ("20261010120000_first.sql", "SELECT 1;"),
        ])
        .expect("migrations assemble");
        assert!(
            assembled.find("SELECT 1;").expect("first")
                < assembled.find("SELECT 2;").expect("second")
        );
        assert!(assemble_migrations(&[("first.sql", "SELECT 1;")]).is_err());
    }

    #[test]
    fn current_surreal_tables_and_columns_produce_stable_differences() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let actual = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        let second = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert_eq!(actual, second);
        assert!(!actual
            .iter()
            .any(|difference| { difference.missing_table.as_deref() == Some("relationship") }));
        assert!(actual.iter().any(|difference| {
            difference.extra_column.as_deref() == Some("memo")
                && difference.table.as_deref() == Some("journal_entry")
        }));
        assert!(
            !actual
                .iter()
                .any(|difference| difference.table.as_deref() == Some("asset")),
            "asset carries every Surreal column and relationship"
        );
    }

    #[test]
    fn source_links_require_a_foreign_key_or_a_uuid_column() {
        let source_tables = vec!["child".to_string(), "parent".to_string()];
        let columns = |table: &str| match table {
            "child" => vec![
                ("id".to_string(), "record".to_string()),
                ("parent_id".to_string(), "record<parent>".to_string()),
            ],
            _ => vec![("id".to_string(), "record".to_string())],
        };

        let without_column = parse_schema(
            "CREATE TABLE child (\n id UUID PRIMARY KEY\n);\nCREATE TABLE parent (\n id UUID PRIMARY KEY\n);",
        )
        .unwrap();
        assert!(compare_to_surreal(&without_column, &source_tables, columns)
            .iter()
            .any(|difference| difference.missing_column.as_deref()
                == Some("relationship parent_id → parent")));

        let retained_uuid = parse_schema(
            "CREATE TABLE child (\n id UUID PRIMARY KEY,\n parent_id UUID\n);\nCREATE TABLE parent (\n id UUID PRIMARY KEY\n);",
        )
        .unwrap();
        assert!(!compare_to_surreal(&retained_uuid, &source_tables, columns)
            .iter()
            .any(|difference| difference.missing_column.as_deref()
                == Some("relationship parent_id → parent")));
    }

    #[test]
    fn erd_generation_is_deterministic_and_has_relationships() {
        let first = render_erd().expect("ERD renders");
        let second = render_erd().expect("ERD renders again");
        assert_eq!(first, second);
        assert!(first.starts_with("erDiagram\n"));
        assert!(first.contains("firm }o--|| brand : references"));
        let source_first = render_surreal_source_erd();
        let source_second = render_surreal_source_erd();
        assert_eq!(source_first.as_bytes(), source_second.as_bytes());
        assert_eq!(
            source_first
                .lines()
                .filter(|line| line.starts_with("    ") && line.ends_with(" {"))
                .count(),
            crate::schema::table_names().len(),
            "source ERD includes every declared Surreal table"
        );
        assert!(source_first.contains("project {"));
        assert!(source_first.contains("relationship_log {"));
        assert!(source_first.contains("project }o--o{ person : project_reference"));
        assert_eq!(
            render_parity_manifest().expect("manifest serializes"),
            render_parity_manifest().expect("manifest serializes again")
        );
    }

    #[test]
    fn checked_in_source_erd_and_manifest_match_the_surreal_schema() {
        assert_eq!(
            SOURCE_ERD,
            render_surreal_source_erd(),
            "regenerate store/postgres/surreal-source-erd.mmd after source schema changes"
        );
        assert_eq!(
            PARITY_MANIFEST,
            render_parity_manifest().expect("manifest serializes"),
            "regenerate store/postgres/parity-manifest.json after schema or migration changes"
        );
    }

    #[test]
    fn seed_coverage_flags_missing_target_tables_and_columns() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [(
            "Person.yaml",
            "records:\n  - email: x@example.test\n    name: Synthetic\n",
        )];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert_eq!(
            differences,
            vec!["Person.yaml: record 1 uses person.name, absent from PostgreSQL"]
        );
    }

    #[test]
    fn seed_contract_passes_when_lookup_and_synthetic_fields_are_represented() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL,\n name TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [("Person.yaml", "lookup_fields:\n  - email\nrecords:\n  - email: synthetic@example.test\n    name: Synthetic Person\n")];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert!(differences.is_empty());
    }

    #[test]
    fn role_seed_uses_person_email_instead_of_inventing_a_person_id_field() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL,\n role TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [("User.yaml", "lookup_fields:\n  - person_id\nrecords:\n  - person:\n      email: synthetic@example.invalid\n    role: admin\n")];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert!(differences.is_empty());
    }

    #[test]
    fn malformed_ddl_is_refused_line_by_line() {
        for ddl in [
            "CREATE TABLE brand (\nCREATE TABLE firm (\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD COLUMN x TEXT\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY,\nALTER TABLE brand ADD COLUMN x TEXT;\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD CONSTRAINT x CHECK (a))(b);\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD CONSTRAINT x CHECK (a = 'b);\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n",
        ] {
            assert!(
                matches!(parse_schema(ddl), Err(Error::UnsupportedSql(_))),
                "{ddl}"
            );
        }
        let escaped = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                       ALTER TABLE brand ADD CONSTRAINT brand_quote_check CHECK (name <> 'it''s');";
        assert!(parse_schema(escaped).is_ok());
        assert!(valid_check_expression("(a (b))"));
        assert!(!valid_check_expression("(a))(b"));
        assert!(!valid_check_expression("()"));
        assert!(!valid_check_expression("a"));
    }

    #[test]
    fn seed_coverage_names_absent_tables_and_columns_without_values() {
        let postgres = parse_schema(
            "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT\n);\n\
             CREATE TABLE question (\n id UUID PRIMARY KEY,\n answer_type TEXT\n);\n\
             CREATE TABLE post (\n id UUID PRIMARY KEY,\n brand_key TEXT,\n social_links JSONB\n);\n",
        )
        .unwrap();
        assert!(matches!(
            seed_schema_differences(&[("neon/Person.yaml", "records: [")], &postgres),
            Err(Error::SeedYaml { .. })
        ));
        assert!(matches!(
            seed_schema_differences(&[("neon/Person.yaml", "lookup_fields: []\n")], &postgres),
            Err(Error::MissingRecords(_))
        ));
        let differences = seed_schema_differences(
            &[
                ("neon/AbsentThing.yaml", "records: []\n"),
                (
                    "neon/User.yaml",
                    "lookup_fields:\n  - person_id\n  - project.code\nrecords:\n  - person:\n      email: synthetic@example.invalid\n      nickname: Synthetic\n",
                ),
                (
                    "neon/Question.yaml",
                    "records:\n  - help_text: Synthetic help\n    question_type: string\n",
                ),
                (
                    "neon/Post.yaml",
                    "lookup_fields:\n  - brand\nrecords:\n  - brand: synthetic\n    social: []\n    entity: Synthetic\n",
                ),
            ],
            &postgres,
        )
        .unwrap();
        assert_eq!(
            differences,
            vec![
                "neon/AbsentThing.yaml: target table absent_thing is absent".to_string(),
                "neon/Post.yaml: record 1 uses post.entity_id, absent from PostgreSQL".to_string(),
                "neon/User.yaml: lookup field project.code requires person.project_id".to_string(),
                "neon/User.yaml: record 1 uses person.nickname, absent from PostgreSQL".to_string(),
            ]
        );
        assert!(!format!("{differences:?}").contains("synthetic@example.invalid"));
    }
}
