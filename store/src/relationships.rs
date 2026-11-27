//! The `relationship` relation — the supplemental typed graph edge the
//! pre-matter conflict check traverses alongside `entity_role`.
//!
//! Adversity, related-party ties, and the edges an LLM later parses out
//! of a Relationship Log's free-form detail all land here. Each carries
//! its own confidence and its own provenance, because a finding must
//! let lawyers judge an edge rather than trust it.
//!
//! Endpoint kinds, confidence, and provenance are validated before the
//! PostgreSQL row and AGE edge are committed together. Captured Source rows
//! remain readable for historical import. Conflict review floors live in
//! `store::conflicts`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph::{ensure_vertices, graph_authority, load_edge};

/// Relationship kind: one node is legally adverse to the other. The
/// strongest conflict signal — a confident `adverse_to` edge between
/// the proposed matter and an existing client blocks the open.
pub const KIND_ADVERSE_TO: &str = "adverse_to";
/// Relationship kind: the two nodes are related parties (family,
/// commonly-controlled entities, insiders) — a softer signal that
/// warrants lawyer review rather than a hard block.
pub const KIND_RELATED_PARTY: &str = "related_party";

/// Provenance: a human asserted this edge directly.
pub const SOURCE_MANUAL: &str = "manual";
/// Provenance: derived from a `disclosures` row.
pub const SOURCE_DISCLOSURE: &str = "disclosure";
/// Provenance: parsed from a Relationship Log entry.
pub const SOURCE_RELATIONSHIP_LOG: &str = "relationship_log";
/// Provenance: extracted from unstructured text by an LLM. These land
/// at lower `confidence_pct` and are always shown as such in findings.
pub const SOURCE_LLM: &str = "llm";

/// Which table an endpoint names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub enum Endpoint {
    Person,
    Entity,
}

impl Endpoint {
    /// The Surreal table this endpoint kind lives in.
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Entity => "entity",
        }
    }

    /// Read an endpoint kind back from a table name, or `None` for a
    /// table the graph does not model.
    #[must_use]
    pub fn from_table(table: &str) -> Option<Self> {
        match table {
            "person" => Some(Self::Person),
            "entity" => Some(Self::Entity),
            _ => None,
        }
    }
}

/// One supplemental typed edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relationship {
    pub from: Endpoint,
    pub from_id: Uuid,
    pub to: Endpoint,
    pub to_id: Uuid,
    pub kind: String,
    /// Confidence this edge is real, 0–100.
    pub confidence_pct: i32,
    pub source_kind: String,
    /// The originating row — a Relationship Log for an LLM-parsed edge,
    /// a Disclosure for a derived one. The schema stores it as a typed
    /// union link while the application-facing shape remains a UUID.
    pub source_id: Option<Uuid>,
    pub detail: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What a write stores.
#[derive(Debug, Clone)]
pub struct NewRelationship {
    pub from: Endpoint,
    pub from_id: Uuid,
    pub to: Endpoint,
    pub to_id: Uuid,
    pub kind: String,
    pub confidence_pct: i32,
    pub source_kind: String,
    pub source_id: Option<Uuid>,
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RelationshipError {
    /// The input does not meet the Relationship validation rules.
    #[error("{0}")]
    Invalid(String),
    /// PostgreSQL graph authority or an endpoint lookup is unavailable.
    #[error("{0}")]
    Engine(String),
    /// The authoritative PostgreSQL store refused the write.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] crate::postgres::PostgresError),
    #[error("PostgreSQL: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// The graph half of an authoritative write failed after the
    /// relational row landed — see [`crate::postgres_graph`].
    #[error("the graph projection could not take the write")]
    Graph(#[from] crate::postgres_graph::GraphError),
}

/// Record a typed edge in PostgreSQL and the AGE graph in one transaction.
///
/// # Errors
///
/// Returns an error if validation fails, PostgreSQL graph authority is
/// unavailable, or the relational or graph write fails.
pub async fn record(
    db: &ApplicationStore,
    input: &NewRelationship,
) -> Result<Relationship, RelationshipError> {
    validate_new_relationship(input)?;
    let postgres = authority().await?;
    record_in_postgres(db, &postgres, input).await
}

async fn authority() -> Result<PostgresDb, RelationshipError> {
    graph_authority().await.map_err(RelationshipError::Engine)
}

/// Validate confidence and provenance before either representation is written.
fn validate_new_relationship(input: &NewRelationship) -> Result<(), RelationshipError> {
    if !(0..=100).contains(&input.confidence_pct) {
        return Err(RelationshipError::Invalid(format!(
            "confidence_pct {} is outside 0-100",
            input.confidence_pct
        )));
    }
    match input.source_kind.as_str() {
        SOURCE_MANUAL | SOURCE_DISCLOSURE | SOURCE_RELATIONSHIP_LOG | SOURCE_LLM => {}
        other => {
            return Err(RelationshipError::Invalid(format!(
                "source_kind `{other}` is not one of {SOURCE_MANUAL}, {SOURCE_DISCLOSURE}, \
                 {SOURCE_RELATIONSHIP_LOG}, {SOURCE_LLM}"
            )))
        }
    }
    Ok(())
}

/// Commit the authoritative relational row and its AGE edge together.
/// Names resolve through the endpoint accessors; a missing name keeps the
/// existing vertex fallback, while a lookup or graph error leaves no committed row.
async fn record_in_postgres(
    db: &ApplicationStore,
    postgres: &PostgresDb,
    input: &NewRelationship,
) -> Result<Relationship, RelationshipError> {
    // Resolve names before reserving the write connection so a saturated
    // pool cannot deadlock on a second connection for an endpoint lookup.
    let from_name = endpoint_name(db, input.from, input.from_id).await?;
    let to_name = endpoint_name(db, input.to, input.to_id).await?;
    let mut transaction = postgres.pool().begin().await?;
    let relationship = record_on(&mut transaction, input, from_name, to_name).await?;
    transaction.commit().await?;
    Ok(relationship)
}

/// Persist a validated relationship and its AGE edge on the caller's connection.
pub(crate) async fn record_on(
    connection: &mut sqlx::PgConnection,
    input: &NewRelationship,
    from_name: Option<String>,
    to_name: Option<String>,
) -> Result<Relationship, RelationshipError> {
    validate_new_relationship(input)?;
    let inserted = sqlx::query(
        "INSERT INTO \"relationship\" (id, \"in\", \"out\", kind, confidence_pct, \
         source_kind, source_id, detail, inserted_at, updated_at) \
         VALUES ($1, $2::jsonb, $3::jsonb, $4, $5, $6, $7::jsonb, $8, now(), now()) \
         RETURNING inserted_at, updated_at",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(record_link_json(input.from.table(), input.from_id).to_string())
    .bind(record_link_json(input.to.table(), input.to_id).to_string())
    .bind(&input.kind)
    .bind(i64::from(input.confidence_pct))
    .bind(&input.source_kind)
    .bind(
        input
            .source_id
            .map(|id| serde_json::json!(id.to_string()).to_string()),
    )
    .bind(&input.detail)
    .fetch_one(&mut *connection)
    .await?;

    let relationship = Relationship {
        from: input.from,
        from_id: input.from_id,
        to: input.to,
        to_id: input.to_id,
        kind: input.kind.clone(),
        confidence_pct: input.confidence_pct,
        source_kind: input.source_kind.clone(),
        source_id: input.source_id,
        detail: input.detail.clone(),
        inserted_at: inserted.try_get("inserted_at")?,
        updated_at: inserted.try_get("updated_at")?,
    };

    write_edge(connection, input, from_name, to_name).await?;
    Ok(relationship)
}

/// The JSONB record link the mirror normalized — `{"tb": ..., "id":
/// {"Uuid": ...}}` — the shape the graph projection's reader parses back
/// into endpoint table and UUID.
fn record_link_json(table: &str, id: Uuid) -> serde_json::Value {
    serde_json::json!({"tb": table, "id": {"Uuid": id.to_string()}})
}

/// Load resolved vertices and the edge in the caller's transaction.
/// Any error leaves no committed relational row.
async fn write_edge(
    connection: &mut sqlx::PgConnection,
    input: &NewRelationship,
    from_name: Option<String>,
    to_name: Option<String>,
) -> Result<(), RelationshipError> {
    ensure_vertices(
        &mut *connection,
        input.from.table(),
        &[(input.from_id, from_name)],
    )
    .await?;
    ensure_vertices(
        &mut *connection,
        input.to.table(),
        &[(input.to_id, to_name)],
    )
    .await?;
    load_edge(
        &mut *connection,
        "relationship",
        serde_json::json!({
            "from_id": input.from_id.to_string(),
            "to_id": input.to_id.to_string(),
            "from_kind": input.from.table(),
            "to_kind": input.to.table(),
            "kind": input.kind,
            "confidence_pct": input.confidence_pct,
            "source_kind": input.source_kind,
            "source_id": input.source_id.map(|id| id.to_string()),
            "detail": input.detail,
        }),
    )
    .await?;
    Ok(())
}

/// One endpoint's display name, read through its admitted accessor. A row that is
/// gone reads as `None`, and the vertex projects with no name — the
/// explanation falls back to the id until the next rebuild.
pub(crate) async fn endpoint_name(
    db: &ApplicationStore,
    endpoint: Endpoint,
    id: Uuid,
) -> Result<Option<String>, RelationshipError> {
    let name = match endpoint {
        Endpoint::Person => crate::persons::find_by_id(db, id)
            .await
            .map_err(|error| RelationshipError::Engine(format!("read the edge's person: {error}")))?
            .map(|person| person.name),
        Endpoint::Entity => crate::entities::find_by_id(db, id)
            .await
            .map_err(|error| RelationshipError::Engine(format!("read the edge's entity: {error}")))?
            .map(|entity| entity.name),
    };
    Ok(name)
}

/// Every typed edge, oldest first, from the store the graph engine names.
///
/// # Errors
///
/// [`RelationshipError::Db`] or [`RelationshipError::Sqlx`] if the lookup
/// fails.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<Relationship>, RelationshipError> {
    let postgres = authority().await?;
    all_in_postgres(&postgres).await
}

/// Every typed edge PostgreSQL holds, oldest first. The endpoint columns
/// are the JSONB record links the write path and the mirror both store;
/// `source_id` is a bare UUID string from the write path or a record link
/// from the mirror. A row whose link this module cannot read is skipped,
/// as the Surreal read skips one.
async fn all_in_postgres(postgres: &PostgresDb) -> Result<Vec<Relationship>, RelationshipError> {
    let rows = sqlx::query(
        "SELECT \"in\"->>'tb' AS from_table, \"in\"->'id'->>'Uuid' AS from_id, \
         \"out\"->>'tb' AS to_table, \"out\"->'id'->>'Uuid' AS to_id, kind, confidence_pct, \
         source_kind, COALESCE(source_id->'id'->>'Uuid', source_id #>> '{}') AS source_id, \
         detail, inserted_at, updated_at \
         FROM \"relationship\" ORDER BY inserted_at ASC, id ASC",
    )
    .fetch_all(&postgres.0)
    .await?;
    let mut edges = Vec::with_capacity(rows.len());
    for row in rows {
        let endpoint = |table: &str, id: &str| -> Result<Option<(Endpoint, Uuid)>, sqlx::Error> {
            let table: Option<String> = row.try_get(table)?;
            let id: Option<String> = row.try_get(id)?;
            Ok(table
                .as_deref()
                .and_then(Endpoint::from_table)
                .zip(id.as_deref().and_then(|id| Uuid::parse_str(id).ok())))
        };
        let (Some((from, from_id)), Some((to, to_id))) = (
            endpoint("from_table", "from_id")?,
            endpoint("to_table", "to_id")?,
        ) else {
            continue;
        };
        let source_id: Option<String> = row.try_get("source_id")?;
        edges.push(Relationship {
            from,
            from_id,
            to,
            to_id,
            kind: row.try_get("kind")?,
            confidence_pct: i32::try_from(row.try_get::<i64, _>("confidence_pct")?).map_err(
                |_| RelationshipError::Invalid("confidence_pct is outside 0-100".into()),
            )?,
            source_kind: row.try_get("source_kind")?,
            source_id: source_id.as_deref().and_then(|id| Uuid::parse_str(id).ok()),
            detail: row.try_get("detail")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::{
        all, record, Endpoint, NewRelationship, KIND_ADVERSE_TO, SOURCE_LLM, SOURCE_MANUAL,
    };
    use crate::application_store::ApplicationStore;
    use uuid::Uuid;

    async fn person(db: &ApplicationStore, name: &str) -> Uuid {
        crate::persons::create(
            db,
            &crate::persons::NewPerson::new(name, format!("{}@example.com", Uuid::now_v7())),
        )
        .await
        .unwrap()
        .id
    }

    async fn entity(db: &ApplicationStore, name: &str) -> Uuid {
        let kind = crate::entity_types::find_or_create(db, "Synthetic relationship entity")
            .await
            .unwrap();
        let jurisdiction = crate::jurisdictions::find_or_create(
            db,
            &crate::jurisdictions::NewJurisdiction::new(
                "Synthetic relationship state",
                "SY",
                "state",
            ),
        )
        .await
        .unwrap();
        crate::entities::create(
            db,
            &crate::entities::NewEntity {
                name: name.into(),
                entity_type_id: kind.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn edge(from: Endpoint, from_id: Uuid, to: Endpoint, to_id: Uuid) -> NewRelationship {
        NewRelationship {
            from,
            from_id,
            to,
            to_id,
            kind: KIND_ADVERSE_TO.into(),
            confidence_pct: 100,
            source_kind: SOURCE_MANUAL.into(),
            source_id: None,
            detail: None,
        }
    }

    #[tokio::test]
    async fn a_recorded_edge_reads_back_with_both_endpoints_typed() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Alice").await;
            let acme = entity(&db, "Acme LLC").await;

            let written = record(&db, &edge(Endpoint::Person, alice, Endpoint::Entity, acme))
                .await
                .unwrap();

            assert_eq!(written.from, Endpoint::Person);
            assert_eq!(written.from_id, alice);
            assert_eq!(written.to, Endpoint::Entity);
            assert_eq!(written.to_id, acme);
            assert_eq!(written.kind, KIND_ADVERSE_TO);
            assert_eq!(all(&db).await.unwrap(), vec![written]);
        })
        .await;
    }

    #[tokio::test]
    async fn provenance_and_detail_travel_with_the_edge() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Parsed Alice").await;
            let bob = person(&db, "Parsed Bob").await;
            let source = Uuid::now_v7();

            let written = record(
                &db,
                &NewRelationship {
                    confidence_pct: 40,
                    source_kind: SOURCE_LLM.into(),
                    source_id: Some(source),
                    detail: Some("parsed from a log entry".into()),
                    ..edge(Endpoint::Person, alice, Endpoint::Person, bob)
                },
            )
            .await
            .unwrap();

            assert_eq!(written.confidence_pct, 40);
            assert_eq!(written.source_kind, SOURCE_LLM);
            assert_eq!(written.source_id, Some(source));
            assert_eq!(written.detail.as_deref(), Some("parsed from a log entry"));
        })
        .await;
    }

    /// The canonical relationship validator rejects invalid confidence
    /// before the PostgreSQL row or AGE edge is written.
    #[tokio::test]
    async fn an_out_of_range_confidence_is_refused() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Over Confident").await;
            let acme = entity(&db, "Over LLC").await;

            let rejected = record(
                &db,
                &NewRelationship {
                    confidence_pct: 101,
                    ..edge(Endpoint::Person, alice, Endpoint::Entity, acme)
                },
            )
            .await;
            assert!(rejected.is_err(), "the engine accepted confidence_pct 101");
        })
        .await;
    }

    /// The canonical validator closes provenance before persistence,
    /// so an unexplained source cannot enter the conflict graph.
    #[tokio::test]
    async fn an_unknown_source_kind_is_refused() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Bad Source").await;
            let acme = entity(&db, "Bad Source LLC").await;

            let rejected = record(
                &db,
                &NewRelationship {
                    source_kind: "guesswork".into(),
                    ..edge(Endpoint::Person, alice, Endpoint::Entity, acme)
                },
            )
            .await;
            assert!(
                rejected.is_err(),
                "the engine accepted an unknown source_kind"
            );
        })
        .await;
    }

    /// The native PostgreSQL write persists its relational row and actual
    /// AGE edge with the same endpoint identities.
    #[tokio::test]
    async fn an_age_engine_write_lands_the_row_and_the_graph_edge() {
        crate::test_support::with_postgres(async {
            use sqlx::Row;
            let postgres = crate::postgres_graph::graph_authority().await.unwrap();
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Libra Tidewell").await;
            let acme = entity(&db, "Acme Age LLC").await;

            let written = super::record_in_postgres(
                &db,
                &postgres,
                &edge(Endpoint::Person, alice, Endpoint::Entity, acme),
            )
            .await
            .unwrap();

            assert_eq!(written.from, Endpoint::Person);
            assert_eq!(written.from_id, alice);
            assert_eq!(written.to, Endpoint::Entity);
            assert_eq!(written.to_id, acme);

            // The relational row landed exactly once.
            let landed = sqlx::query("SELECT count(*) AS n FROM \"relationship\"")
                .fetch_one(&postgres.0)
                .await
                .unwrap();
            assert_eq!(landed.get::<i64, _>("n"), 1, "the relational row landed");

            // And the graph took the edge, endpoints included: read it back
            // through the same parity reader the conflict walk uses.
            let rows = sqlx::query("SELECT t.v::text FROM graph_conflict_edges($1) AS t(v)")
                .bind(serde_json::json!([alice.to_string()]).to_string())
                .fetch_all(&postgres.0)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "the graph took one edge");
            let edge = rows[0].get::<String, _>("v");
            assert!(
                edge.contains(&alice.to_string()) && edge.contains(&acme.to_string()),
                "the graph edge lost an endpoint: {edge}"
            );
        })
        .await;
    }

    /// A PostgreSQL row and its conflict edge are one accepted write. A real
    /// AGE-table failure must leave neither representation committed.
    #[tokio::test]
    async fn an_age_edge_failure_rolls_back_the_authoritative_relationship_row() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let alice = person(&db, "Libra Tidewell").await;
            let acme = entity(&db, "Rollback LLC").await;
            let postgres = crate::postgres_graph::graph_authority().await.unwrap();
            crate::postgres_graph::fail_after_loading_edge(&postgres).await;
            let rejected =
                record(&db, &edge(Endpoint::Person, alice, Endpoint::Entity, acme)).await;
            assert!(rejected.is_err(), "the actual AGE insertion must fail");
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM relationship")
                .fetch_one(postgres.pool())
                .await
                .unwrap();
            assert_eq!(
                rows, 0,
                "a failed AGE edge cannot leave an authoritative row committed"
            );
            let edges = crate::postgres_graph::conflict_traversal(
                &postgres,
                &[crate::conflicts::NodeRef::person(alice)],
            )
            .await
            .unwrap();
            assert!(
                edges.is_empty(),
                "the rejected edge must not enter the conflict graph"
            );
        })
        .await;
    }
}
