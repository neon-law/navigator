//! The AGE graph projection against a real AGE 1.6.0 database, beside
//! SurrealDB. Fixtures live in both stores under the same record UUIDs, so
//! the parity comparison compares the same graph twice: the projection loads
//! from the PostgreSQL mirrors, the Surreal side walks the records the
//! relationship seam wrote, and [`compare_traversal`] must see one graph.

use super::{advance, compare_traversal, project_graph, validate_graph, GRAPH_MIRROR};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::relationships::{Endpoint, NewRelationship, KIND_ADVERSE_TO, SOURCE_MANUAL};
use crate::test_support::{
    mem_surreal, seed_entity, SEED_ENTITY_JURISDICTION_ID, SEED_ENTITY_TYPE_ID,
};
use crate::{postgres::PostgresDb, surreal::SurrealDb};
use sqlx::Row;

/// A synthetic firm graph seeded into both stores: two people, an entity, a
/// project, a firm, and every edge family the projection carries. The ids
/// are the same records in both stores, so traversal parity compares like
/// with like.
async fn seed(surreal: &SurrealDb, pg: &PostgresDb) {
    let (entity, people) = seed_surreal(surreal).await;
    seed_postgres_reference(pg, entity, &people).await;
    seed_postgres_edges(pg, entity, &people).await;
}

/// The fixed insert timestamp every fixture row carries.
fn fixture_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The Surreal side: reference rows, the entity, two people, one entity
/// role, and one adverse relationship. Returns the ids the PostgreSQL
/// mirror mirrors under the same UUIDs.
async fn seed_surreal(surreal: &SurrealDb) -> (uuid::Uuid, Vec<uuid::Uuid>) {
    // The schema's seeded Brands name Firms this synthetic source lacks.
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    // The reference rows `seed_entity` names, in both stores.
    let inserted_at = fixture_time();
    surreal
        .query("CREATE $id SET name = 'Synthetic Entity Type', inserted_at = $at, updated_at = $at")
        .bind((
            "id",
            crate::surreal::record_id("entity_type", SEED_ENTITY_TYPE_ID),
        ))
        .bind(("at", surrealdb::types::Datetime::from(inserted_at)))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query(
            "CREATE $id SET name = 'Synthetic Jurisdiction', code = 'ZZ', \
             jurisdiction_type = 'state', inserted_at = $at, updated_at = $at",
        )
        .bind((
            "id",
            crate::surreal::record_id("jurisdiction", SEED_ENTITY_JURISDICTION_ID),
        ))
        .bind(("at", surrealdb::types::Datetime::from(inserted_at)))
        .await
        .unwrap()
        .check()
        .unwrap();

    let entity = seed_entity(surreal).await;
    let mut people = Vec::new();
    for index in 0..2 {
        people.push(
            crate::persons::create(
                surreal,
                &crate::persons::NewPerson::with_role(
                    "Synthetic Person",
                    format!("graph-{index}-{}@example.com", uuid::Uuid::now_v7()),
                    crate::persons::Role::Client,
                ),
            )
            .await
            .unwrap()
            .id,
        );
    }
    crate::entity_roles::grant(surreal, people[0], entity, "manages")
        .await
        .unwrap();
    crate::relationships::record(
        surreal,
        &NewRelationship {
            from: Endpoint::Person,
            from_id: people[0],
            to: Endpoint::Person,
            to_id: people[1],
            kind: KIND_ADVERSE_TO.to_string(),
            confidence_pct: 80,
            source_kind: SOURCE_MANUAL.to_string(),
            source_id: None,
            detail: None,
        },
    )
    .await
    .unwrap();
    (entity, people)
}

/// The PostgreSQL mirror's canonical rows: the same reference rows, entity,
/// and people the Surreal side holds, under the same UUIDs.
async fn seed_postgres_reference(pg: &PostgresDb, entity: uuid::Uuid, people: &[uuid::Uuid]) {
    let inserted_at = fixture_time();
    sqlx::query(
        "INSERT INTO entity_type (id, name, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity Type', $2, $2)",
    )
    .bind(SEED_ENTITY_TYPE_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jurisdiction (id, name, code, jurisdiction_type, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Jurisdiction', 'ZZ', 'state', $2, $2)",
    )
    .bind(SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity', $2, $3, $4, $4)",
    )
    .bind(entity)
    .bind(SEED_ENTITY_TYPE_ID)
    .bind(SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    for (index, person) in people.iter().enumerate() {
        sqlx::query(
            "INSERT INTO person (id, name, email, inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(person)
        .bind(format!("Synthetic Person {index}"))
        .bind(format!("{person}@example.com"))
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
    }
}

/// The PostgreSQL rows the graph projection reads as edge sources: the
/// project and firm, the entity role, the relationship, and both
/// participation tables.
async fn seed_postgres_edges(pg: &PostgresDb, entity: uuid::Uuid, people: &[uuid::Uuid]) {
    let inserted_at = fixture_time();
    let project = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id, inserted_at, updated_at) \
         VALUES ($1, 'SYN', 'Synthetic Project', 'active', $2, $3, $3)",
    )
    .bind(project)
    .bind(entity)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    let firm = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO firm (id, name, slug, status, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Firm', 'synthetic-firm', 'active', $2, $2)",
    )
    .bind(firm)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO entity_role (person_id, entity_id, role, inserted_at, updated_at) \
         VALUES ($1, $2, 'manages', $3, $3)",
    )
    .bind(people[0])
    .bind(entity)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO \"relationship\" (id, \"in\", \"out\", kind, confidence_pct, source_kind, \
         inserted_at, updated_at) \
         VALUES ($1, $2::jsonb, $3::jsonb, 'adverse_to', 80, 'manual', $4, $4)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(serde_json::json!({"tb": "person", "id": {"Uuid": people[0].to_string()}}).to_string())
    .bind(serde_json::json!({"tb": "person", "id": {"Uuid": people[1].to_string()}}).to_string())
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_project_role (person_id, project_id, participation, inserted_at, \
         updated_at) VALUES ($1, $2, 'lead', $3, $3)",
    )
    .bind(people[0])
    .bind(project)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_firm_role (person_id, firm_id, membership, inserted_at, updated_at) \
         VALUES ($1, $2, 'lawyer', $3, $3)",
    )
    .bind(people[0])
    .bind(firm)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
}

async fn failures(pg: &PostgresDb, run_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_failure WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

#[tokio::test]
async fn projection_loads_every_source_row_and_validates() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    seed(&surreal, &pg).await;

    let projection = project_graph(&pg).await.unwrap();
    assert_eq!(
        projection.vertices.get("Person"),
        Some(&2),
        "both people are vertices"
    );
    assert_eq!(projection.edges.get("HAS_ROLE"), Some(&1));
    assert_eq!(projection.edges.get("RELATIONSHIP"), Some(&1));
    assert_eq!(projection.edges.get("WORKS_ON"), Some(&1));
    assert_eq!(projection.edges.get("MEMBER_OF"), Some(&1));
    assert_eq!(
        projection.edges.get("HANDLES"),
        Some(&1),
        "the firm handles the project its lawyer works on"
    );
    let validation = validate_graph(&pg).await.unwrap();
    assert!(
        validation.is_clean(),
        "counts and endpoints agree: {validation:?}"
    );

    // Removing a source row after the projection leaves the graph holding an
    // edge the sources no longer back: validation reports it, and the next
    // projection rebuilds the graph without it.
    sqlx::query("DELETE FROM \"relationship\"")
        .execute(&pg.0)
        .await
        .unwrap();
    let validation = validate_graph(&pg).await.unwrap();
    assert!(!validation.is_clean(), "a stale edge cannot validate");
    let rebuilt = project_graph(&pg).await.unwrap();
    assert_eq!(rebuilt.edges.get("RELATIONSHIP"), Some(&0));
    let validation = validate_graph(&pg).await.unwrap();
    assert!(validation.is_clean(), "a rebuilt projection validates");
    assert_eq!(
        projection.total_rows() - rebuilt.total_rows(),
        1,
        "the rebuild dropped the one deleted edge"
    );
}

#[tokio::test]
async fn traversal_parity_holds_on_shared_fixtures() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    seed(&surreal, &pg).await;

    project_graph(&pg).await.unwrap();
    let parity = compare_traversal(&surreal, &pg).await.unwrap();
    assert!(
        parity.is_clean(),
        "both engines walk the same graph: {parity:?}"
    );
    assert_eq!(parity.age_edges, 2, "the role tie and the adverse tie");
}

#[tokio::test]
async fn advance_records_a_clean_graph_arm() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    seed(&surreal, &pg).await;

    let run_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO mirror_backfill_run (id, batch_size, status) VALUES ($1, 500, 'running')",
    )
    .bind(run_id)
    .execute(&pg.0)
    .await
    .unwrap();
    let batches = advance(&surreal, &pg, run_id).await.unwrap();
    assert_eq!(
        batches, 3,
        "projection, validation, and parity each ran once"
    );
    let checkpoint = sqlx::query(
        "SELECT batches, failed, completed_at FROM mirror_backfill_checkpoint \
         WHERE run_id = $1 AND mirror = $2",
    )
    .bind(run_id)
    .bind(GRAPH_MIRROR)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(checkpoint.get::<i32, _>("batches"), 3);
    assert_eq!(checkpoint.get::<i32, _>("failed"), 0);
    assert!(
        checkpoint
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("completed_at")
            .is_some(),
        "a clean arm completes"
    );
    assert_eq!(failures(&pg, run_id).await, 0, "a clean run records none");

    // A completed checkpoint is not advanced again.
    let again = advance(&surreal, &pg, run_id).await.unwrap();
    assert_eq!(again, 0, "the arm resumes to nothing");
}
