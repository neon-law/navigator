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

mod declarations;

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
#[cfg(test)]
const MESSAGE_PROCESS_STATE_MIGRATION: &str =
    include_str!("../postgres/migrations/20261031140000_add_message_process_state.sql");

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
/// standalone indexes, PUBLIC privilege revocations, and named CHECK constraints
/// and dropped named constraints (neither changes table/column parity).
#[expect(
    clippy::too_many_lines,
    reason = "single ordered pass tracks SQL table and procedural block state"
)]
pub fn parse_schema(sql: &str) -> Result<BTreeMap<String, Table>, Error> {
    let mut tables = BTreeMap::new();
    let mut current: Option<Table> = None;
    let mut procedural = false;
    let mut index_continuation = false;

    for raw in declarations::lines(sql) {
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
        // A view adds no table or column; skip its body through the `;`.
        if current.is_none()
            && (line.starts_with("CREATE VIEW ") || line.starts_with("CREATE OR REPLACE VIEW "))
        {
            index_continuation = !line.ends_with(';');
            continue;
        }
        if current.is_none() && line.starts_with("DROP VIEW ") && line.ends_with(';') {
            continue;
        }
        // Privileges do not change table, column, or reference shape.
        if let Some(revoked) = line.strip_prefix("REVOKE ALL ON ") {
            let target = revoked.strip_suffix(" FROM PUBLIC;");
            if current.is_some()
                || target.is_none_or(|target| target.trim().is_empty() || target.contains(';'))
            {
                return Err(Error::UnsupportedSql(line.to_string()));
            }
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
            let foreign_key_words = words.strip_suffix(&["NOT", "VALID"]).unwrap_or(&words);
            if let [name, "ADD", "CONSTRAINT", _, "FOREIGN", "KEY", column, "REFERENCES", target] =
                foreign_key_words
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
            if let Some((name, constraint)) =
                rest.trim_end_matches(';').split_once(" DROP CONSTRAINT ")
            {
                // Dropping a named constraint changes no table, column, or
                // foreign key the parity model records; validate the shape
                // and move on.
                let valid_identifier = !constraint.is_empty()
                    && constraint
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_');
                if !valid_identifier || !tables.contains_key(name) {
                    return Err(Error::UnsupportedSql(line.to_string()));
                }
                continue;
            }
            if let [name, "DROP", "COLUMN", column] = words.as_slice() {
                let table = tables
                    .get_mut(*name)
                    .filter(|table| table.columns.contains_key(*column))
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                table.columns.remove(*column);
                let dropped: Vec<String> = table
                    .foreign_keys
                    .iter()
                    .filter(|(source, _)| source == column)
                    .map(|(_, target)| target.clone())
                    .collect();
                table.foreign_keys.retain(|(source, _)| source != column);
                for target in dropped {
                    if !table.foreign_keys.iter().any(|(_, other)| *other == target) {
                        table.references.remove(&target);
                    }
                }
                continue;
            }
            if let [name, "ALTER", "COLUMN", column, "DROP", "NOT", "NULL"] = words.as_slice() {
                if !tables
                    .get(*name)
                    .is_some_and(|table| table.columns.contains_key(*column))
                {
                    return Err(Error::UnsupportedSql(line.to_string()));
                }
                continue;
            }
            if let [name, "ALTER", "COLUMN", column, "TYPE", sql_type] = words.as_slice() {
                let current = tables
                    .get_mut(*name)
                    .and_then(|table| table.columns.get_mut(*column))
                    .ok_or_else(|| Error::UnsupportedSql(line.to_string()))?;
                *current = (*sql_type).to_string();
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

fn erd_type(sql_type: &str) -> &'static str {
    match sql_type {
        "UUID" => "uuid",
        "BOOLEAN" => "boolean",
        "BIGINT" | "INTEGER" | "SMALLINT" => "int",
        "DOUBLE" | "DOUBLE PRECISION" | "REAL" | "FLOAT" => "number",
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

    #[test]
    fn not_valid_foreign_key_preserves_the_parent_reference() {
        let schema = parse_schema("CREATE TABLE notation (\n id UUID PRIMARY KEY\n);\nCREATE TABLE signature (\n notation_id UUID\n);\nALTER TABLE signature ADD CONSTRAINT signature_notation_id_fkey FOREIGN KEY (notation_id) REFERENCES notation(id) NOT VALID;\n").unwrap();
        assert!(schema["signature"]
            .foreign_keys
            .contains(&("notation_id".into(), "notation".into())));
        assert!(parse_schema("CREATE TABLE signature (\n id UUID PRIMARY KEY\n);\nALTER TABLE signature ADD CONSTRAINT bad FOREIGN KEY (missing) REFERENCES notation(id) NOT VALID;\n").is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // The explicit ordered migration inventory is the contract under test.
    fn canonical_schema_is_the_ordered_migration_assembly() {
        let assembled = assemble_migrations(&[
            (
                "20261010120000_create_core_tenancy.sql",
                initial_migration(),
            ),
            (
                "20261010130000_create_post.sql",
                post_migration(),
            ),
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
            (
                "20261017120000_create_leads.sql",
                leads_migration(),
            ),
            (
                "20261018120000_create_matter_pages_and_captions.sql",
                matter_pages_captions_migration(),
            ),
            (
                "20261018150000_create_project_archive.sql",
                include_str!("../postgres/migrations/20261018150000_create_project_archive.sql"),
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
            (
                "20261020010000_create_inbox.sql",
                inbox_migration(),
            ),
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
                "20261021130000_create_social_account_selection.sql",
                include_str!("../postgres/migrations/20261021130000_create_social_account_selection.sql"),
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
            (
                "20261021170000_create_blog_video.sql",
                blog_video_migration(),
            ),
            (
                "20261022120000_create_source_domain_records.sql",
                SOURCE_DOMAIN_RECORDS_MIGRATION,
            ),
            (
                "20261022140000_create_dbos_workflow_request.sql",
                dbos_workflow_request_migration(),
            ),
            (
                "20261022200000_add_tenancy_authority_constraints.sql",
                tenancy_authority_migration(),
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
                "20261027120000_add_project_calendar_and_booking_url.sql",
                include_str!(
                    "../postgres/migrations/20261027120000_add_project_calendar_and_booking_url.sql"
                ),
            ),
            (
                "20261029010000_add_communication_receipts.sql",
                include_str!("../postgres/migrations/20261029010000_add_communication_receipts.sql"),
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
                "20261030130000_create_person_phone.sql",
                include_str!("../postgres/migrations/20261030130000_create_person_phone.sql"),
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
                "20261031025000_graph_conflict_names_and_runtime_grants.sql",
                include_str!(
                    "../postgres/migrations/20261031025000_graph_conflict_names_and_runtime_grants.sql"
                ),
            ),
            (
                "20261031090000_extend_identity_systems.sql",
                include_str!("../postgres/migrations/20261031090000_extend_identity_systems.sql"),
            ),
            (
                "20261031120000_add_project_onboarding_jurisdiction.sql",
                include_str!("../postgres/migrations/20261031120000_add_project_onboarding_jurisdiction.sql"),
            ),
            (
                "20261031130000_create_case_stage.sql",
                include_str!("../postgres/migrations/20261031130000_create_case_stage.sql"),
            ),
            (
                "20261031140000_add_message_process_state.sql",
                MESSAGE_PROCESS_STATE_MIGRATION,
            ),
            (
                "20261031150000_add_project_cost_source_reference.sql",
                include_str!(
                    "../postgres/migrations/20261031150000_add_project_cost_source_reference.sql"
                ),
            ),
            (
                "20261101000000_create_time_entry.sql",
                include_str!("../postgres/migrations/20261101000000_create_time_entry.sql"),
            ),
            (
                "20261101010000_create_timer.sql",
                include_str!("../postgres/migrations/20261101010000_create_timer.sql"),
            ),
            (
                "20261101020000_create_attorney_rates.sql",
                include_str!("../postgres/migrations/20261101020000_create_attorney_rates.sql"),
            ),
            (
                "20261101030000_create_assistant_turn_and_sandbox_usage.sql",
                include_str!(
                    "../postgres/migrations/20261101030000_create_assistant_turn_and_sandbox_usage.sql"
                ),
            ),
            (
                "20261101040000_add_notation_draft_run.sql",
                include_str!("../postgres/migrations/20261101040000_add_notation_draft_run.sql"),
            ),
            (
                "20261101060000_create_project_folder.sql",
                include_str!("../postgres/migrations/20261101060000_create_project_folder.sql"),
            ),
            (
                "20261101100000_create_document_upload_session.sql",
                include_str!("../postgres/migrations/20261101100000_create_document_upload_session.sql"),
            ),
            (
                "20261101110000_add_deadline_category.sql",
                include_str!("../postgres/migrations/20261101110000_add_deadline_category.sql"),
            ),
            (
                "20261101130000_create_firm_forgejo_organization.sql",
                include_str!(
                    "../postgres/migrations/20261101130000_create_firm_forgejo_organization.sql"
                ),
            ),
            (
                "20261101140000_create_notation_machine_state.sql",
                include_str!("../postgres/migrations/20261101140000_create_notation_machine_state.sql"),
            ),
            (
                "20261101150000_create_brand_phone_number.sql",
                include_str!("../postgres/migrations/20261101150000_create_brand_phone_number.sql"),
            ),
            (
                "20261101160000_create_billed_call.sql",
                include_str!("../postgres/migrations/20261101160000_create_billed_call.sql"),
            ),
            (
                "20261101170000_create_ssh_public_key.sql",
                include_str!("../postgres/migrations/20261101170000_create_ssh_public_key.sql"),
            ),
            (
                "20261101190000_create_inbox_attachment_receipt.sql",
                include_str!("../postgres/migrations/20261101190000_create_inbox_attachment_receipt.sql"),
            ),
            (
                "20261101200000_create_inbox_triage.sql",
                include_str!("../postgres/migrations/20261101200000_create_inbox_triage.sql"),
            ),
            (
                "20261101210000_create_notation_transition.sql",
                include_str!("../postgres/migrations/20261101210000_create_notation_transition.sql"),
            ),
            (
                "20261102000000_allow_x_inbox_channel.sql",
                include_str!("../postgres/migrations/20261102000000_allow_x_inbox_channel.sql"),
            ),
            (
                "20261102010000_allow_voice_inbox_channel.sql",
                include_str!("../postgres/migrations/20261102010000_allow_voice_inbox_channel.sql"),
            ),
            (
                "20261102020000_linkedin_channel_and_raw_storage.sql",
                include_str!("../postgres/migrations/20261102020000_linkedin_channel_and_raw_storage.sql"),
            ),
            (
                "20261103000000_add_message_nef_outcome.sql",
                include_str!("../postgres/migrations/20261103000000_add_message_nef_outcome.sql"),
            ),
            (
                "20261103010000_create_matter_billing_batch.sql",
                include_str!("../postgres/migrations/20261103010000_create_matter_billing_batch.sql"),
            ),
            (
                "20261103120000_create_brand_letterhead.sql",
                include_str!("../postgres/migrations/20261103120000_create_brand_letterhead.sql"),
            ),
            (
                "20261104000000_create_person_directory_views.sql",
                include_str!("../postgres/migrations/20261104000000_create_person_directory_views.sql"),
            ),
            (
                "20261104120000_sent_email_append_only.sql",
                include_str!("../postgres/migrations/20261104120000_sent_email_append_only.sql"),
            ),
            (
                "20261105000000_person_email_citext.sql",
                include_str!("../postgres/migrations/20261105000000_person_email_citext.sql"),
            ),
            (
                "20261105120000_inbox_conversation_person_id.sql",
                include_str!(
                    "../postgres/migrations/20261105120000_inbox_conversation_person_id.sql"
                ),
            ),
            (
                "20261105130000_index_message_provider_message_id.sql",
                include_str!(
                    "../postgres/migrations/20261105130000_index_message_provider_message_id.sql"
                ),
            ),
            (
                "20261105140000_entity_metadata.sql",
                include_str!("../postgres/migrations/20261105140000_entity_metadata.sql"),
            ),
            (
                "20261105150000_create_esignature_webhook_journal.sql",
                include_str!(
                    "../postgres/migrations/20261105150000_create_esignature_webhook_journal.sql"
                ),
            ),
            (
                "20261106000000_create_contingency_term.sql",
                include_str!("../postgres/migrations/20261106000000_create_contingency_term.sql"),
            ),
            (
                "20261106010000_create_outcome_estimate.sql",
                include_str!("../postgres/migrations/20261106010000_create_outcome_estimate.sql"),
            ),
            (
                "20261106020000_create_matter_outcome.sql",
                include_str!("../postgres/migrations/20261106020000_create_matter_outcome.sql"),
            ),
            (
                "20261106030000_add_stage_default_odds.sql",
                include_str!("../postgres/migrations/20261106030000_add_stage_default_odds.sql"),
            ),
            (
                "20261106120000_backfill_deadline_category.sql",
                include_str!("../postgres/migrations/20261106120000_backfill_deadline_category.sql"),
            ),
            (
                "20261106130000_create_mail_signature.sql",
                include_str!("../postgres/migrations/20261106130000_create_mail_signature.sql"),
            ),
            (
                "20261107000000_create_legal_plan.sql",
                include_str!("../postgres/migrations/20261107000000_create_legal_plan.sql"),
            ),
            (
                "20261107010000_create_plan_fee_schedule.sql",
                include_str!("../postgres/migrations/20261107010000_create_plan_fee_schedule.sql"),
            ),
            (
                "20261107020000_add_project_plan_columns.sql",
                include_str!("../postgres/migrations/20261107020000_add_project_plan_columns.sql"),
            ),
            (
                "20261107030000_add_time_entry_stage.sql",
                include_str!("../postgres/migrations/20261107030000_add_time_entry_stage.sql"),
            ),
            (
                "20261107120000_person_directory_list_indexes.sql",
                include_str!("../postgres/migrations/20261107120000_person_directory_list_indexes.sql"),
            ),
            (
                "20261107130000_add_client_production_subfolder_slots.sql",
                include_str!(
                    "../postgres/migrations/20261107130000_add_client_production_subfolder_slots.sql"
                ),
            ),
            (
                "20261107140000_add_asset_folder.sql",
                include_str!("../postgres/migrations/20261107140000_add_asset_folder.sql"),
            ),
            (
                "20261107150000_enable_postgis.sql",
                include_str!("../postgres/migrations/20261107150000_enable_postgis.sql"),
            ),
            (
                "20261107160000_create_firm_assumption.sql",
                include_str!("../postgres/migrations/20261107160000_create_firm_assumption.sql"),
            ),
            (
                "20261107170000_create_astrocartography_reference_geometry.sql",
                include_str!(
                    "../postgres/migrations/20261107170000_create_astrocartography_reference_geometry.sql"
                ),
            ),
            (
                "20261108000000_add_project_client_dri.sql",
                include_str!("../postgres/migrations/20261108000000_add_project_client_dri.sql"),
            ),
            (
                "20261108120000_seed_sun_lower_meridian.sql",
                include_str!("../postgres/migrations/20261108120000_seed_sun_lower_meridian.sql"),
            ),
            (
                "20261108140000_add_project_plan_case_number.sql",
                include_str!(
                    "../postgres/migrations/20261108140000_add_project_plan_case_number.sql"
                ),
            ),
            ("20261108130000_add_notation_creation_attempt.sql", include_str!("../postgres/migrations/20261108130000_add_notation_creation_attempt.sql")),
            (
                "20261109120000_add_project_status_transition.sql",
                include_str!("../postgres/migrations/20261109120000_add_project_status_transition.sql"),
            ),
            (
                "20261110000000_create_forgejo_push_webhook_journal.sql",
                include_str!(
                    "../postgres/migrations/20261110000000_create_forgejo_push_webhook_journal.sql"
                ),
            ),
            (
                "20261110120000_add_lead_consent_call_opt_in.sql",
                include_str!(
                    "../postgres/migrations/20261110120000_add_lead_consent_call_opt_in.sql"
                ),
            ),
            (
                "20261110130000_inbox_conversation_trashed_at.sql",
                include_str!(
                    "../postgres/migrations/20261110130000_inbox_conversation_trashed_at.sql"
                ),
            ),
            (
                "20261111120000_add_matter_caption_signed_on.sql",
                include_str!(
                    "../postgres/migrations/20261111120000_add_matter_caption_signed_on.sql"
                ),
            ),
            (
                "20261112000000_add_costs_project_folder_slot.sql",
                include_str!("../postgres/migrations/20261112000000_add_costs_project_folder_slot.sql"),
            ),
            (
                "20261114140000_matter_record_line_indexes.sql",
                include_str!("../postgres/migrations/20261114140000_matter_record_line_indexes.sql"),
            ),
            (
                "20261114150000_identity_authority_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261114150000_identity_authority_constraints.sql"
                ),
            ),
            (
                "20261114170000_catalog_authority_jurisdiction_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261114170000_catalog_authority_jurisdiction_constraints.sql"
                ),
            ),
            (
                "20261114180000_create_plan_intake_and_upload_session_folder.sql",
                include_str!(
                    "../postgres/migrations/20261114180000_create_plan_intake_and_upload_session_folder.sql"
                ),
            ),
            (
                "20261114190000_add_firm_portal_theme_brand.sql",
                include_str!(
                    "../postgres/migrations/20261114190000_add_firm_portal_theme_brand.sql"
                ),
            ),
            (
                "20261114210000_scope_manual_relationship_logs.sql",
                include_str!("../postgres/migrations/20261114210000_scope_manual_relationship_logs.sql"),
            ),
            (
                "20261115150000_create_product_catalog_history.sql",
                include_str!("../postgres/migrations/20261115150000_create_product_catalog_history.sql"),
            ),
            (
                "20261115160000_create_person_calendar.sql",
                include_str!("../postgres/migrations/20261115160000_create_person_calendar.sql"),
            ),
            (
                "20261115170000_add_firm_calendar_settings.sql",
                include_str!("../postgres/migrations/20261115170000_add_firm_calendar_settings.sql"),
            ),
            (
                "20261116080000_create_inbox_attachment_filing.sql",
                include_str!("../postgres/migrations/20261116080000_create_inbox_attachment_filing.sql"),
            ),
            (
                "20261116120000_add_clause_heading_and_aliases.sql",
                include_str!("../postgres/migrations/20261116120000_add_clause_heading_and_aliases.sql"),
            ),
            (
                "20261116190000_create_project_calendar_grant.sql",
                include_str!("../postgres/migrations/20261116190000_create_project_calendar_grant.sql"),
            ),
            (
                "20261116200000_add_task_handoff.sql",
                include_str!("../postgres/migrations/20261116200000_add_task_handoff.sql"),
            ),
            (
                "20261116210000_matter_page_document_revision.sql",
                include_str!("../postgres/migrations/20261116210000_matter_page_document_revision.sql"),
            ),
            (
                "20261116220000_create_evidence_share.sql",
                include_str!("../postgres/migrations/20261116220000_create_evidence_share.sql"),
            ),
            (
                "20261116230000_add_matter_page_media.sql",
                include_str!("../postgres/migrations/20261116230000_add_matter_page_media.sql"),
            ),
            (
                "20261117000000_create_docusign_completion.sql",
                include_str!("../postgres/migrations/20261117000000_create_docusign_completion.sql"),
            ),
            (
                "20261117000100_add_stripe_draft_to_billing_batch.sql",
                include_str!("../postgres/migrations/20261117000100_add_stripe_draft_to_billing_batch.sql"),
            ),
            (
                "20261117120000_add_accounting_authority_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261117120000_add_accounting_authority_constraints.sql"
                ),
            ),
            (
                "20261117130000_add_case_authority_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261117130000_add_case_authority_constraints.sql"
                ),
            ),
            (
                "20261117140000_add_review_authority_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261117140000_add_review_authority_constraints.sql"
                ),
            ),
            (
                "20261117150000_add_signature_authority_constraints.sql",
                include_str!(
                    "../postgres/migrations/20261117150000_add_signature_authority_constraints.sql"
                ),
            ),
            (
                "20261117160000_signature_notation_reference.sql",
                include_str!("../postgres/migrations/20261117160000_signature_notation_reference.sql"),
            ),
            (
                "20261117170000_notation_document_authority.sql",
                include_str!("../postgres/migrations/20261117170000_notation_document_authority.sql"),
            ),
            (
                "20261117190000_add_satellite_authority_constraints.sql",
                include_str!("../postgres/migrations/20261117190000_add_satellite_authority_constraints.sql"),
            ),
            ("20261117200000_scope_mirror_backfill_runs.sql", include_str!("../postgres/migrations/20261117200000_scope_mirror_backfill_runs.sql")),
            ("20261118160000_purchase_stripe.sql", include_str!("../postgres/migrations/20261118160000_purchase_stripe.sql")),

            ("20261118210000_preserve_deployment_change_context.sql", include_str!("../postgres/migrations/20261118210000_preserve_deployment_change_context.sql")),
            (
                "20261119010000_create_project_pull_request_notice.sql",
                include_str!("../postgres/migrations/20261119010000_create_project_pull_request_notice.sql"),
            ),
            ("20261119020000_email_summary_recovery.sql", include_str!("../postgres/migrations/20261119020000_email_summary_recovery.sql")),
            ("20261119030000_project_slack_delivery.sql", include_str!("../postgres/migrations/20261119030000_project_slack_delivery.sql")),
            ("20261119040000_project_slack_delivery_reconciliation.sql", include_str!("../postgres/migrations/20261119040000_project_slack_delivery_reconciliation.sql")),
            ("20261119050000_create_database_origin.sql", include_str!("../postgres/migrations/20261119050000_create_database_origin.sql")),
            (
                "20261119180000_add_firm_esign_provider.sql",
                include_str!("../postgres/migrations/20261119180000_add_firm_esign_provider.sql"),
            ),
            (
                "20261121000000_create_database_recovery.sql",
                include_str!("../postgres/migrations/20261121000000_create_database_recovery.sql"),
            ),
            ("20261121100000_create_billing_annotation.sql", include_str!("../postgres/migrations/20261121100000_create_billing_annotation.sql")),
            (
                "20261121220000_admit_government_official_reference.sql",
                include_str!(
                    "../postgres/migrations/20261121220000_admit_government_official_reference.sql"
                ),
            ),
            ("20261122000200_create_legal_plan_claims.sql", include_str!("../postgres/migrations/20261122000200_create_legal_plan_claims.sql")),
            ("20261122000300_close_native_graph_writers.sql", include_str!("../postgres/migrations/20261122000300_close_native_graph_writers.sql")),
            (
                "20261122000400_preserve_plan_intake_case_number.sql",
                include_str!("../postgres/migrations/20261122000400_preserve_plan_intake_case_number.sql"),
            ),
            ("20261122021314_converge_native_person_merge.sql", include_str!("../postgres/migrations/20261122021314_converge_native_person_merge.sql")),
            ("20261122140000_firm_namespace_move.sql", include_str!("../postgres/migrations/20261122140000_firm_namespace_move.sql")),
            ("20261122150000_optional_plan_terms.sql", include_str!("../postgres/migrations/20261122150000_optional_plan_terms.sql")),
            ("20261122180000_personal_project.sql", include_str!("../postgres/migrations/20261122180000_personal_project.sql")),
            ("20261123003000_project_envelope.sql", include_str!("../postgres/migrations/20261123003000_project_envelope.sql")),
            ("20261123010000_firm_secret_mode.sql", include_str!("../postgres/migrations/20261123010000_firm_secret_mode.sql")),
            ("20261123054600_add_mail_project_folder_slot.sql", include_str!("../postgres/migrations/20261123054600_add_mail_project_folder_slot.sql")),
            ("20261123131000_create_project_list.sql", include_str!("../postgres/migrations/20261123131000_create_project_list.sql")),
            ("20261123190000_create_project_calendar_event_backup.sql", include_str!("../postgres/migrations/20261123190000_create_project_calendar_event_backup.sql")),
            ("20261123200000_widen_plan_matter_type.sql", include_str!("../postgres/migrations/20261123200000_widen_plan_matter_type.sql")),
            ("20261124030000_create_lead_intake.sql", include_str!("../postgres/migrations/20261124030000_create_lead_intake.sql")),
            ("20261124150000_deadline_civil_date.sql", include_str!("../postgres/migrations/20261124150000_deadline_civil_date.sql")),
            ("20261124150100_project_calendar_timezone.sql", include_str!("../postgres/migrations/20261124150100_project_calendar_timezone.sql")),
            ("20261124150200_deadline_proposals.sql", include_str!("../postgres/migrations/20261124150200_deadline_proposals.sql")),
            ("20261124150300_deadline_proposal_source_receipts.sql", include_str!("../postgres/migrations/20261124150300_deadline_proposal_source_receipts.sql")),
            ("20261124180000_firm_namespace_provider_source.sql", include_str!("../postgres/migrations/20261124180000_firm_namespace_provider_source.sql")),
            ("20261124210000_create_project_labels.sql", include_str!("../postgres/migrations/20261124210000_create_project_labels.sql")),
            ("20261125010000_make_attorney_rates_insert_only.sql", include_str!("../postgres/migrations/20261125010000_make_attorney_rates_insert_only.sql")),
            ("20261125020000_index_upcoming_project_deadlines.sql", include_str!("../postgres/migrations/20261125020000_index_upcoming_project_deadlines.sql")),
            ("20261125120000_public_contact_lead_intake.sql", include_str!("../postgres/migrations/20261125120000_public_contact_lead_intake.sql")),
            ("20261125200000_attorney_booking_calendar.sql", include_str!("../postgres/migrations/20261125200000_attorney_booking_calendar.sql")),
            ("20261126033000_project_plan_benefit_terms.sql", include_str!("../postgres/migrations/20261126033000_project_plan_benefit_terms.sql")),
            ("20261126230000_allow_durable_object_cutover_without_capture.sql", include_str!("../postgres/migrations/20261126230000_allow_durable_object_cutover_without_capture.sql")),
            ("20261127010000_add_firm_drive_settings.sql", include_str!("../postgres/migrations/20261127010000_add_firm_drive_settings.sql")),
            ("20261127193400_deadline_scheduling.sql", include_str!("../postgres/migrations/20261127193400_deadline_scheduling.sql")),
        ])
        .expect("timestamped migration assembles");
        assert_eq!(canonical_schema().trim(), assembled.trim());
    }

    #[test]
    fn canonical_schema_admits_contact_lead_intake() {
        let schema = parse_schema(canonical_schema()).expect("canonical SQL parses");
        assert_eq!(schema["lead_intake"].columns["contact_name"], "TEXT");
    }

    #[test]
    fn inbox_conversation_persists_the_sender_person() {
        let schema = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let conversation = schema
            .get("inbox_conversation")
            .expect("the inbox conversation table");
        let kind = conversation
            .columns
            .get("person_id")
            .expect("the sender-person link column");
        assert!(kind.contains("UUID"), "person_id is a UUID: {kind}");
        assert!(
            conversation
                .foreign_keys
                .contains(&("person_id".to_string(), "person".to_string())),
            "person_id references person"
        );
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
            "person_id",
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
        let ledger = schema
            .get("sent_email_export")
            .expect("Sent Email Export ledger");
        for column in [
            "day",
            "row_count",
            "content_sha256",
            "data_file",
            "purged_at",
        ] {
            assert!(
                ledger.columns.contains_key(column),
                "sent_email_export.{column}"
            );
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
    fn a_dropped_constraint_parses_without_changing_parity_shape() {
        let ddl = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand DROP CONSTRAINT brand_font_licence_check;";
        let tables = parse_schema(ddl).expect("dropped constraint parses");
        assert_eq!(tables["brand"].columns.len(), 1);
        for revoke in [
            "REVOKE ALL ON brand FROM PUBLIC;",
            "REVOKE ALL ON FUNCTION protect_brand() FROM PUBLIC;",
            "REVOKE ALL ON FUNCTION protect_brand(REGROLE) FROM PUBLIC;",
        ] {
            assert_eq!(parse_schema(&format!("{ddl}\n{revoke}")).unwrap(), tables);
        }
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE absent DROP CONSTRAINT x;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand DROP CONSTRAINT has space;"
        )
        .is_err());
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
            "REVOKE ALL ON brand FROM PUBLIC",
            "REVOKE ALL ON  FROM PUBLIC;",
            "REVOKE ALL ON brand; SELECT 1 FROM PUBLIC;",
            "REVOKE ALL ON brand FROM navigator_app;",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY,\n REVOKE ALL ON brand FROM PUBLIC;\n);",
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
