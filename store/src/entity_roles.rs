//! The `entity_role` relation — a Person's structural tie to an Entity
//! (manages, owns, member-of), and one of the two edge tables the
//! pre-matter conflict check walks.
//!
//! PostgreSQL holds the relational tie and its AGE `HAS_ROLE` edge. Its
//! identity is the Person, Entity and role; Source reads remain available
//! for captured historical import.
//!
//! # Idempotence is the contract
//!
//! `store::seed` and `import::apply` both re-run over a live database
//! and must not duplicate a tie. A hand-rolled find-then-insert is a race
//! with no constraint behind it, so
//! the index closes it and [`grant`] re-reads the winner's row when it
//! loses — the pattern `store::persons::find_or_create` established.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph::{ensure_vertices, graph_authority, load_edge};

const PERSON_TABLE: &str = "person";
const ENTITY_TABLE: &str = "entity";

/// One structural tie between a Person and an Entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityRole {
    pub person_id: Uuid,
    pub entity_id: Uuid,
    /// Role token (`manages`, `member`, `beneficiary`, …). Open by
    /// design: the vocabulary grows without a migration, and the
    /// conflict check treats every structural tie the same way.
    pub role: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum EntityRoleError {
    /// A write reported success but returned no row, or one this module
    /// could not read back.
    #[error("writing an entity role returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL is configured but unreachable. A grant fails rather
    /// than guessing a store.
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

/// Record one structural tie: in PostgreSQL (the relational row, then
/// the graph's `HAS_ROLE` edge) wherever it is configured, else in
/// SurrealDB ([`graph_authority`]).
///
/// Idempotent and race-safe: the `entity_role_tie` index makes the
/// second writer lose, and the loser re-reads rather than failing. The
/// seed runs this on every boot.
///
/// # Errors
///
/// [`EntityRoleError::Postgres`] or [`EntityRoleError::Sqlx`] if the
/// PostgreSQL write fails, or [`EntityRoleError::Engine`] if graph authority
/// or an endpoint lookup is unavailable.
pub async fn grant(
    db: &ApplicationStore,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
) -> Result<EntityRole, EntityRoleError> {
    let role = role.trim();
    let postgres = authority().await?;
    grant_in_postgres(db, &postgres, person_id, entity_id, role).await
}

async fn authority() -> Result<PostgresDb, EntityRoleError> {
    graph_authority().await.map_err(EntityRoleError::Engine)
}

/// The authoritative write: the relational row first (the store of
/// record), then the graph's `HAS_ROLE` edge, whose vertices the write
/// ensures before loading — the projection may not have rebuilt since
/// the Person or Entity was minted.
///
/// The vertex-ensure function uses the authoritative Person and Entity names.
async fn grant_in_postgres(
    db: &ApplicationStore,
    postgres: &PostgresDb,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
) -> Result<EntityRole, EntityRoleError> {
    if let Some(existing) = find_in_postgres(postgres, person_id, entity_id, role).await? {
        return Ok(existing);
    }

    let person = crate::persons::find_by_id(db, person_id)
        .await
        .map_err(|error| EntityRoleError::Engine(format!("read the tie's person: {error}")))?;
    let entity = crate::entities::find_by_id(db, entity_id)
        .await
        .map_err(|error| EntityRoleError::Engine(format!("read the tie's entity: {error}")))?;
    let mut transaction = postgres.pool().begin().await?;
    let written = sqlx::query(
        "INSERT INTO entity_role (person_id, entity_id, role) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (person_id, entity_id, role) DO NOTHING \
         RETURNING inserted_at, updated_at",
    )
    .bind(person_id)
    .bind(entity_id)
    .bind(role)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some(row) = written else {
        // Another writer minted the same tie between the read above and
        // this write. Its row is the answer.
        transaction.rollback().await?;
        return find_in_postgres(postgres, person_id, entity_id, role)
            .await?
            .ok_or(EntityRoleError::WriteReturnedNothing);
    };
    let tie = EntityRole {
        person_id,
        entity_id,
        role: role.to_string(),
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    };

    write_edge(
        &mut transaction,
        person_id,
        entity_id,
        role,
        person.map(|p| p.name),
        entity.map(|e| e.name),
    )
    .await?;
    transaction.commit().await?;
    Ok(tie)
}

/// Write the resolved endpoint names and graph edge on the same connection
/// as the uncommitted relational tie; no additional pool connection is needed.
async fn write_edge(
    connection: &mut sqlx::PgConnection,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
    person_name: Option<String>,
    entity_name: Option<String>,
) -> Result<(), EntityRoleError> {
    ensure_vertices(&mut *connection, PERSON_TABLE, &[(person_id, person_name)]).await?;
    ensure_vertices(&mut *connection, ENTITY_TABLE, &[(entity_id, entity_name)]).await?;
    load_edge(
        &mut *connection,
        "has_role",
        serde_json::json!({
            "from_id": person_id.to_string(),
            "to_id": entity_id.to_string(),
            "role": role,
        }),
    )
    .await?;
    Ok(())
}

/// The one tie matching all three parts of the natural key, read from
/// PostgreSQL — the authoritative copy under the `age` engine.
async fn find_in_postgres(
    postgres: &PostgresDb,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
) -> Result<Option<EntityRole>, EntityRoleError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {PG_SELECT} FROM entity_role \
         WHERE person_id = $1 AND entity_id = $2 AND role = $3"
    )))
    .bind(person_id)
    .bind(entity_id)
    .bind(role.trim())
    .fetch_optional(&postgres.0)
    .await?;
    Ok(row.as_ref().map(from_pg).transpose()?)
}

const PG_SELECT: &str = "person_id, entity_id, role, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<EntityRole, sqlx::Error> {
    Ok(EntityRole {
        person_id: row.try_get("person_id")?,
        entity_id: row.try_get("entity_id")?,
        role: row.try_get("role")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The ties a PostgreSQL filter selects, oldest first.
async fn select_in_postgres(
    postgres: &PostgresDb,
    filter: &str,
    id: Option<Uuid>,
) -> Result<Vec<EntityRole>, EntityRoleError> {
    // `filter` is one of this module's literals, never input.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {PG_SELECT} FROM entity_role {filter} ORDER BY inserted_at ASC, id ASC"
    )))
    .bind(id)
    .fetch_all(&postgres.0)
    .await?
    .iter()
    .map(|row| from_pg(row).map_err(EntityRoleError::from))
    .collect()
}

/// The one tie matching all three parts of the natural key, if any, from
/// the store the graph engine names.
///
/// # Errors
///
/// [`EntityRoleError::Db`] or [`EntityRoleError::Sqlx`] if the lookup
/// fails.
pub async fn find(
    _db: &ApplicationStore,
    person_id: Uuid,
    entity_id: Uuid,
    role: &str,
) -> Result<Option<EntityRole>, EntityRoleError> {
    {
        let postgres = authority().await?;
        find_in_postgres(&postgres, person_id, entity_id, role).await
    }
}

/// Every structural tie, ordered so a listing is stable.
///
/// # Errors
///
/// [`EntityRoleError::Db`] or [`EntityRoleError::Sqlx`] if the lookup
/// fails.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<EntityRole>, EntityRoleError> {
    {
        let postgres = authority().await?;
        select_in_postgres(&postgres, "WHERE $1::uuid IS NULL", None).await
    }
}

/// Every tie held by one Person.
///
/// # Errors
///
/// [`EntityRoleError::Db`] if the lookup fails.
pub async fn for_person(
    _db: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<EntityRole>, EntityRoleError> {
    let postgres = authority().await?;
    select_in_postgres(&postgres, "WHERE person_id = $1", Some(person_id)).await
}

/// Every tie into one Entity.
///
/// # Errors
///
/// [`EntityRoleError::Db`] if the lookup fails.
pub async fn for_entity(
    _db: &ApplicationStore,
    entity_id: Uuid,
) -> Result<Vec<EntityRole>, EntityRoleError> {
    let postgres = authority().await?;
    select_in_postgres(&postgres, "WHERE entity_id = $1", Some(entity_id)).await
}

#[cfg(test)]
mod tests {
    use super::{all, find, for_entity, for_person, grant};
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
        let kind = crate::entity_types::find_or_create(db, "Synthetic role entity")
            .await
            .unwrap();
        let jurisdiction = crate::jurisdictions::find_or_create(
            db,
            &crate::jurisdictions::NewJurisdiction::new("Synthetic role state", "SY", "state"),
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

    #[tokio::test]
    async fn a_granted_tie_reads_back_on_both_of_its_ends() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Tie Holder").await;
            let acme = entity(&db, "Acme LLC").await;

            let tie = grant(&db, holder, acme, "manages").await.unwrap();
            assert_eq!(tie.person_id, holder);
            assert_eq!(tie.entity_id, acme);
            assert_eq!(tie.role, "manages");

            assert_eq!(for_person(&db, holder).await.unwrap(), vec![tie.clone()]);
            assert_eq!(for_entity(&db, acme).await.unwrap(), vec![tie.clone()]);
            assert_eq!(find(&db, holder, acme, "manages").await.unwrap(), Some(tie));
            assert_eq!(find(&db, holder, acme, "owns").await.unwrap(), None);
        })
        .await;
    }

    /// The seed and the bulk importer both re-run over a live database.
    #[tokio::test]
    async fn granting_the_same_tie_twice_is_one_edge() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Repeat Holder").await;
            let acme = entity(&db, "Repeat LLC").await;

            let first = grant(&db, holder, acme, "manages").await.unwrap();
            let second = grant(&db, holder, acme, "manages").await.unwrap();
            assert_eq!(first, second);
            assert_eq!(all(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    /// The find-then-insert this replaced had no constraint behind it,
    /// so two concurrent seeds could both write. The index closes that,
    /// and the loser adopts the winner's row rather than failing.
    #[tokio::test]
    async fn concurrent_grants_of_one_tie_settle_on_a_single_edge() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Racing Holder").await;
            let acme = entity(&db, "Racing LLC").await;

            let (first, second) = tokio::join!(
                grant(&db, holder, acme, "manages"),
                grant(&db, holder, acme, "manages"),
            );
            assert_eq!(first.unwrap(), second.unwrap());
            assert_eq!(all(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    /// A person may hold several roles in one entity, so the tie is the
    /// triple rather than the pair — the index must not collapse them.
    #[tokio::test]
    async fn one_person_may_hold_several_roles_in_one_entity() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Many Hats").await;
            let acme = entity(&db, "Many Hats LLC").await;

            grant(&db, holder, acme, "manages").await.unwrap();
            grant(&db, holder, acme, "owns").await.unwrap();

            let mut roles: Vec<String> = for_person(&db, holder)
                .await
                .unwrap()
                .into_iter()
                .map(|tie| tie.role)
                .collect();
            roles.sort();
            assert_eq!(roles, ["manages", "owns"]);
        })
        .await;
    }

    /// The edge has to be a real graph edge, not two columns that
    /// happen to hold ids: the conflict traversal walks it with
    /// `->entity_role->`, which only resolves if `in`/`out` were
    /// written as links to records that exist.
    #[tokio::test]
    async fn the_tie_is_traversable_as_a_graph_edge() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Graph Holder").await;
            let acme = entity(&db, "Graph LLC").await;
            grant(&db, holder, acme, "manages").await.unwrap();

            let pg = crate::postgres_graph::graph_authority().await.unwrap();
            let edges = crate::postgres_graph::conflict_traversal(
                &pg,
                &[crate::conflicts::NodeRef::person(holder)],
            )
            .await
            .unwrap();
            let reached = Some(
                edges
                    .iter()
                    .filter(|edge| {
                        edge.edge_table == "entity_role"
                            && edge.from_id == holder.to_string()
                            && edge.to_id == acme.to_string()
                    })
                    .filter_map(|edge| edge.to_name.clone())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(
                reached,
                Some(vec!["Graph LLC".to_string()]),
                "the edge's `out` link did not resolve to the entity row"
            );
        })
        .await;
    }

    /// A failed AGE write cannot leave a structural tie that later retries
    /// mistake for a complete graph edge.
    #[tokio::test]
    async fn an_age_edge_failure_rolls_back_the_authoritative_entity_role() {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let holder = person(&db, "Rollback Holder").await;
            let acme = entity(&db, "Rollback LLC").await;
            let postgres = crate::postgres_graph::graph_authority()
                .await
                .unwrap();
            crate::postgres_graph::fail_after_loading_edge(&postgres).await;
            let rejected = grant(&db, holder, acme, "manages").await;
            assert!(rejected.is_err(), "the actual AGE insertion must fail");
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM entity_role")
                .fetch_one(postgres.pool())
                .await
                .unwrap();
            assert_eq!(
                rows, 0,
                "a failed AGE edge cannot leave an authoritative tie committed"
            );
            sqlx::raw_sql("DROP FUNCTION graph_load_edges(text, text); ALTER FUNCTION test_original_graph_load_edges(text, text) RENAME TO graph_load_edges;")
                .execute(postgres.pool()).await.unwrap();
            let recovered = grant(&db, holder, acme, "manages").await.unwrap();
            assert_eq!(recovered.person_id, holder);
            let edges = crate::postgres_graph::conflict_traversal(
                &postgres,
                &[crate::conflicts::NodeRef::person(holder)],
            )
            .await
            .unwrap();
            assert_eq!(
                edges.len(),
                1,
                "retry must create the actual missing structural edge"
            );
            assert_eq!(edges[0].to_id, acme.to_string());
        })
        .await;
    }
}
