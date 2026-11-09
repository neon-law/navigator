//! The Firm's assumptions: the stated premises the CEO digest checks and
//! a Kit resolves at run time (ENG-1264).
//!
//! An assumption is one Firm's answer under one dotted key — for example
//! `plan.target_minutes.document_review` — stored as versioned history:
//! a change is a new version row, never an edit, so the history is the
//! table (the `plan_fee_schedule` rule). The keys come from the
//! repository's public catalog, `assumptions/schema.yaml`, which carries
//! the dotted path, the value's type, and its null default and never a
//! Firm's value; `set` refuses any key or value shape the catalog does not
//! describe. A fresh deployment holds no rows, so every lookup renders
//! blank — never another Firm's economics. See
//! `docs/glossary/firm-assumption.md`.
//!
//! Reads and writes here are Firm-keyed data operations: the durable
//! workflows call them directly as the system actor, and an actor-facing
//! caller (the `navigator assumptions` commands through their API routes)
//! authorizes with [`crate::firm_capability::FirmCapability::
//! ManageFirmAssumptions`] first, exactly the `firm_journal_api` split.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// The repository's public key catalog, `assumptions/schema.yaml`: keys,
/// types, and null defaults, never a Firm's value. Pinned at compile time
/// so the accessor and the catalog cannot drift apart unnoticed. The embed
/// lives once, in `rules::assumption_keys` — `rules` needs the key list for
/// the Project Skill gate (`N128`) and carries no `store` dependency — and
/// this static re-exports it rather than including the file a second time.
pub const SCHEMA_YAML: &str = rules::assumption_keys::SCHEMA_YAML;

/// The value shape a key's catalog entry declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssumptionType {
    Integer,
    Number,
    Text,
    Boolean,
}

impl AssumptionType {
    /// The word `assumptions/schema.yaml` spells the type with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Text => "string",
            Self::Boolean => "boolean",
        }
    }

    fn parse(raw: &str) -> Result<Self, FirmAssumptionError> {
        match raw {
            "integer" => Ok(Self::Integer),
            "number" => Ok(Self::Number),
            "string" => Ok(Self::Text),
            "boolean" => Ok(Self::Boolean),
            _ => Err(FirmAssumptionError::Catalog),
        }
    }

    /// Whether a JSON value belongs to this type. JSON has one number
    /// kind, so an integer is also a number — the same rule PostgreSQL's
    /// `jsonb` applies.
    #[must_use]
    pub fn matches(self, value: &serde_json::Value) -> bool {
        match self {
            Self::Integer => value.is_i64(),
            Self::Number => value.is_number(),
            Self::Text => value.is_string(),
            Self::Boolean => value.is_boolean(),
        }
    }
}

/// One key's entry in the repository's `assumptions/schema.yaml` catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct AssumptionKeySpec {
    pub kind: AssumptionType,
    /// The value a fresh deployment renders. Every key ships null: blank
    /// until the Firm sets its own, never someone else's.
    pub default: serde_json::Value,
    pub description: String,
}

/// The catalog, parsed once. A broken committed catalog is a programmer
/// error `the_catalog_parses_and_carries_only_known_shapes` catches in
/// tests, so the parse failure panics instead of leaking a typed error
/// through every later read.
#[must_use]
pub fn catalog() -> &'static BTreeMap<String, AssumptionKeySpec> {
    static CATALOG: LazyLock<BTreeMap<String, AssumptionKeySpec>> = LazyLock::new(|| {
        let raw: BTreeMap<String, YamlValue> =
            serde_yaml::from_str(SCHEMA_YAML).expect("assumptions/schema.yaml parses");
        raw.into_iter()
            .map(|(key, entry)| {
                let spec = AssumptionKeySpec {
                    kind: AssumptionType::parse(
                        entry
                            .get("type")
                            .and_then(YamlValue::as_str)
                            .expect("every catalog entry carries a type"),
                    )
                    .expect("every catalog type is one of integer, number, string, boolean"),
                    default: serde_json::to_value(
                        entry.get("default").cloned().unwrap_or(YamlValue::Null),
                    )
                    .unwrap_or(serde_json::Value::Null),
                    description: entry
                        .get("description")
                        .and_then(YamlValue::as_str)
                        .expect("every catalog entry carries a description")
                        .to_string(),
                };
                (key, spec)
            })
            .collect()
    });
    &CATALOG
}

/// What the actor asked to record under one key.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFirmAssumption {
    pub key: String,
    pub value: serde_json::Value,
    pub reason: Option<String>,
}

/// The typed failures an assumption read or write can meet.
#[derive(Debug, thiserror::Error)]
pub enum FirmAssumptionError {
    #[error("invalid firm assumption request: {0}")]
    Invalid(&'static str),
    #[error("unknown assumption key: {0}")]
    UnknownKey(String),
    #[error("the assumption value does not match the key's catalog type")]
    TypeMismatch,
    #[error("the repository's assumptions/schema.yaml catalog is invalid")]
    Catalog,
    #[error("the Firm assumption is unreadable or the write failed")]
    Database(#[from] sqlx::Error),
}

/// Validates a key and its value against the catalog: the pure half of
/// [`set`], unit-testable without a database.
fn validate(key: &str, value: &serde_json::Value) -> Result<AssumptionType, FirmAssumptionError> {
    let spec = catalog()
        .get(key)
        .ok_or_else(|| FirmAssumptionError::UnknownKey(key.to_string()))?;
    if value.is_null() || !spec.kind.matches(value) {
        return Err(FirmAssumptionError::TypeMismatch);
    }
    Ok(spec.kind)
}

/// One versioned assumption row: the Firm's value under one key at one
/// point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirmAssumption {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub key: String,
    pub value: serde_json::Value,
    pub version: i32,
    pub set_by: Uuid,
    pub set_at: DateTime<Utc>,
    pub reason: Option<String>,
}

fn from_row(row: &sqlx::postgres::PgRow) -> FirmAssumption {
    FirmAssumption {
        id: row.get("id"),
        firm_id: row.get("firm_id"),
        key: row.get("key"),
        value: row.get("value"),
        version: row.get("version"),
        set_by: row.get("set_by"),
        set_at: row.get("set_at"),
        reason: row.get("reason"),
    }
}

/// The Firm's current assumptions — one row per key, the newest version of
/// each, ordered by key for stable display. Another Firm's rows never
/// appear: every read is filtered to the one `firm_id`.
///
/// # Errors
///
/// [`FirmAssumptionError::Database`] on a database failure.
pub async fn list(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<FirmAssumption>, FirmAssumptionError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (key) id, firm_id, key, value, version, set_by, set_at, reason \
         FROM firm_assumption WHERE firm_id = $1 ORDER BY key, version DESC",
    )
    .bind(firm_id)
    .fetch_all(pg.pool())
    .await?;
    Ok(rows.iter().map(from_row).collect())
}

/// Every version of one key, newest first — the append-only history. The
/// trigger refuses edits and removals, so no other shape is possible.
///
/// # Errors
///
/// [`FirmAssumptionError::Database`] on a database failure.
pub async fn history(
    pg: &PostgresDb,
    firm_id: Uuid,
    key: &str,
) -> Result<Vec<FirmAssumption>, FirmAssumptionError> {
    let rows = sqlx::query(
        "SELECT id, firm_id, key, value, version, set_by, set_at, reason \
         FROM firm_assumption WHERE firm_id = $1 AND key = $2 ORDER BY version DESC",
    )
    .bind(firm_id)
    .bind(key)
    .fetch_all(pg.pool())
    .await?;
    Ok(rows.iter().map(from_row).collect())
}

/// The newest version of one key, or `None` when the Firm has never set
/// it — the blank a fresh deployment renders.
///
/// # Errors
///
/// [`FirmAssumptionError::Database`] on a database failure.
pub async fn current(
    pg: &PostgresDb,
    firm_id: Uuid,
    key: &str,
) -> Result<Option<FirmAssumption>, FirmAssumptionError> {
    let row = sqlx::query(
        "SELECT id, firm_id, key, value, version, set_by, set_at, reason \
         FROM firm_assumption WHERE firm_id = $1 AND key = $2 ORDER BY version DESC LIMIT 1",
    )
    .bind(firm_id)
    .bind(key)
    .fetch_optional(pg.pool())
    .await?;
    Ok(row.as_ref().map(from_row))
}

/// The value under one key, for a Kit or digest resolving at run time.
/// `None` when the Firm never set the key: blank, never a default from
/// another Firm.
///
/// # Errors
///
/// [`FirmAssumptionError::Database`] on a database failure.
pub async fn lookup(
    pg: &PostgresDb,
    firm_id: Uuid,
    key: &str,
) -> Result<Option<serde_json::Value>, FirmAssumptionError> {
    Ok(current(pg, firm_id, key).await?.map(|row| row.value))
}

/// Records a new version under one key: catalog-checked, type-checked,
/// versioned past the current maximum, and only ever an insert. The
/// actor-facing caller authorizes with
/// [`crate::firm_capability::FirmCapability::ManageFirmAssumptions`] first.
///
/// # Errors
///
/// [`FirmAssumptionError::UnknownKey`] when the catalog does not describe
/// the key, [`FirmAssumptionError::TypeMismatch`] when the value does not
/// match the key's type, [`FirmAssumptionError::Invalid`] when the reason
/// is blank, and [`FirmAssumptionError::Database`] on a database failure —
/// including the rare concurrent write that loses the version race.
pub async fn set(
    pg: &PostgresDb,
    firm_id: Uuid,
    set_by: Uuid,
    new_assumption: &NewFirmAssumption,
) -> Result<FirmAssumption, FirmAssumptionError> {
    validate(&new_assumption.key, &new_assumption.value)?;
    if new_assumption
        .reason
        .as_ref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err(FirmAssumptionError::Invalid("reason"));
    }
    let row = sqlx::query(
        "INSERT INTO firm_assumption (firm_id, key, value, version, set_by, reason) \
         SELECT $1, $2, $3, COALESCE(MAX(version), 0) + 1, $4, $5 \
         FROM firm_assumption WHERE firm_id = $1 AND key = $2 \
         RETURNING id, firm_id, key, value, version, set_by, set_at, reason",
    )
    .bind(firm_id)
    .bind(&new_assumption.key)
    .bind(&new_assumption.value)
    .bind(set_by)
    .bind(new_assumption.reason.as_deref())
    .fetch_one(pg.pool())
    .await?;
    Ok(from_row(&row))
}

#[cfg(test)]
mod tests {
    use super::{catalog, validate, AssumptionType, FirmAssumptionError};
    use crate::postgres::PostgresDb;
    use crate::postgres_mirror::tests::{connect, fresh_schema};
    use serde_json::json;
    use sqlx::Row;
    use uuid::Uuid;

    #[test]
    fn the_catalog_parses_and_carries_only_known_shapes() {
        let known: &[(&str, AssumptionType)] = &[
            (
                "plan.target_minutes.document_review",
                AssumptionType::Integer,
            ),
            ("plan.target_minutes.letter", AssumptionType::Integer),
            (
                "plan.target_minutes.uncontested_divorce",
                AssumptionType::Integer,
            ),
            (
                "plan.target_minutes.court_appearance",
                AssumptionType::Integer,
            ),
        ];
        for (key, kind) in known {
            let spec = catalog()
                .get(*key)
                .unwrap_or_else(|| panic!("the catalog carries {key}"));
            assert_eq!(spec.kind, *kind, "{key} is typed");
            assert!(
                spec.default.is_null(),
                "{key} defaults to null — blank, not someone else's economics"
            );
            assert!(
                !spec.description.trim().is_empty()
                    && spec.description.ends_with('.')
                    && !spec.description.contains(". "),
                "{key} carries a one-sentence description"
            );
        }
        assert_eq!(catalog().len(), known.len(), "no stray catalog entries");
    }

    #[test]
    fn set_validation_refuses_unknown_keys_and_type_mismatches() {
        assert!(matches!(
            validate("plan.target_minutes.document_review", &json!(45)),
            Ok(AssumptionType::Integer)
        ));
        for (key, value) in [
            ("plan.target_minutes.nope", json!(45)),
            ("plan.target_minutes", json!(45)),
            ("Plan.Target_Minutes.Document_Review", json!(45)),
            (
                "plan.target_minutes.document_review",
                serde_json::Value::Null,
            ),
            ("plan.target_minutes.document_review", json!(45.5)),
            ("plan.target_minutes.document_review", json!("45")),
            ("plan.target_minutes.document_review", json!(true)),
        ] {
            let error = validate(key, &value).expect_err("the write is refused");
            assert!(
                matches!(
                    error,
                    FirmAssumptionError::UnknownKey(_) | FirmAssumptionError::TypeMismatch
                ),
                "{key} refuses with a typed error, not a panic"
            );
        }
    }

    async fn firm(pg: &PostgresDb) -> Uuid {
        let id: Uuid = sqlx::query(
            "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic Assumption Firm',$2,'active') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("fa-{}", Uuid::new_v4().simple()))
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        id
    }

    async fn person(pg: &PostgresDb) -> Uuid {
        let id: Uuid = sqlx::query(
            "INSERT INTO person (id, name, email) VALUES ($1, 'Synthetic Assumption Admin', $2) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("assumption-{}@example.com", Uuid::now_v7().simple()))
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        id
    }

    fn setting(value: i64) -> super::NewFirmAssumption {
        super::NewFirmAssumption {
            key: "plan.target_minutes.document_review".into(),
            value: json!(value),
            reason: Some("The panel keeps returning reviews faster than planned".into()),
        }
    }

    #[tokio::test]
    async fn assumptions_are_versioned_append_only_and_firm_scoped() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firm_id = firm(&pg).await;
        let admin = person(&pg).await;

        let first = super::set(&pg, firm_id, admin, &setting(45)).await.unwrap();
        assert_eq!(first.version, 1, "the first version is one");
        let second = super::set(&pg, firm_id, admin, &setting(30)).await.unwrap();
        assert_eq!(second.version, 2, "a change adds a version");

        let rows = super::history(&pg, firm_id, "plan.target_minutes.document_review")
            .await
            .unwrap();
        assert_eq!(rows, vec![second.clone(), first.clone()]);
        assert_eq!(
            super::current(&pg, firm_id, "plan.target_minutes.document_review")
                .await
                .unwrap(),
            Some(second.clone())
        );
        assert_eq!(
            super::lookup(&pg, firm_id, "plan.target_minutes.document_review")
                .await
                .unwrap(),
            Some(json!(30))
        );

        // History: the trigger refuses an edit, a removal, and a wipe.
        for statement in [
            "UPDATE firm_assumption SET value = '45' WHERE firm_id = $1",
            "DELETE FROM firm_assumption WHERE firm_id = $1",
            "TRUNCATE firm_assumption",
        ] {
            let result = if statement.starts_with("TRUNCATE") {
                sqlx::query(statement).execute(pg.pool()).await
            } else {
                sqlx::query(statement)
                    .bind(firm_id)
                    .execute(pg.pool())
                    .await
            };
            assert!(result.is_err(), "{statement} must be refused");
        }
        assert_eq!(
            super::current(&pg, firm_id, "plan.target_minutes.document_review")
                .await
                .unwrap(),
            Some(second),
            "the refused statements left the history standing"
        );
    }

    #[tokio::test]
    async fn an_assumption_is_readable_only_inside_its_firm() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firm_id = firm(&pg).await;
        let other_firm = firm(&pg).await;
        let admin = person(&pg).await;
        super::set(&pg, firm_id, admin, &setting(45)).await.unwrap();

        // A second Firm holds no copy and cannot read the row, under any
        // accessor, at any version.
        assert!(super::list(&pg, other_firm).await.unwrap().is_empty());
        assert!(
            super::history(&pg, other_firm, "plan.target_minutes.document_review")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            super::current(&pg, other_firm, "plan.target_minutes.document_review")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            super::lookup(&pg, other_firm, "plan.target_minutes.document_review")
                .await
                .unwrap()
                .is_none()
        );

        // The same key is one Firm's row each: the second Firm sets its own.
        super::set(&pg, other_firm, admin, &setting(90))
            .await
            .unwrap();
        assert_eq!(
            super::lookup(&pg, other_firm, "plan.target_minutes.document_review")
                .await
                .unwrap(),
            Some(json!(90))
        );
        assert_eq!(
            super::lookup(&pg, firm_id, "plan.target_minutes.document_review")
                .await
                .unwrap(),
            Some(json!(45)),
            "one Firm's set never rewrites another's"
        );
    }

    #[tokio::test]
    async fn a_missing_assumption_renders_blank() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firm_id = firm(&pg).await;
        assert!(super::list(&pg, firm_id).await.unwrap().is_empty());
        assert!(
            super::lookup(&pg, firm_id, "plan.target_minutes.court_appearance")
                .await
                .unwrap()
                .is_none()
        );

        // A known key with no row is blank; an unknown key is refused at
        // write time, so no row can exist under it to read.
        let admin = person(&pg).await;
        let mut unknown = setting(20);
        unknown.key = "plan.target_minutes.somewhere.else".into();
        assert!(matches!(
            super::set(&pg, firm_id, admin, &unknown).await,
            Err(FirmAssumptionError::UnknownKey(_))
        ));
        let mut wrong_shape = setting(20);
        wrong_shape.value = json!("half an hour");
        assert!(matches!(
            super::set(&pg, firm_id, admin, &wrong_shape).await,
            Err(FirmAssumptionError::TypeMismatch)
        ));
        let mut blank_reason = setting(20);
        blank_reason.reason = Some("   ".into());
        assert!(matches!(
            super::set(&pg, firm_id, admin, &blank_reason).await,
            Err(FirmAssumptionError::Invalid("reason"))
        ));
        assert!(super::list(&pg, firm_id).await.unwrap().is_empty());
    }
}
