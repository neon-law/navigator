use super::*;
use crate::postgres::fixtures::{connect, fresh_schema};

/// A PostgreSQL-only fixture for matter-record accessor tests.
pub(crate) async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// Create a Project and the minimum relational parents it needs.
pub(crate) async fn postgres_project(pg: &PostgresDb, code: &str) -> Uuid {
    let entity_id = postgres_entity(pg).await;
    let project_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id) VALUES ($1,$2,$2,'open',$3)",
    )
    .bind(project_id)
    .bind(code)
    .bind(entity_id)
    .execute(pg.pool())
    .await
    .expect("synthetic Project inserts");
    project_id
}

/// Create a synthetic Entity with its required catalog parents.
pub(crate) async fn postgres_entity(pg: &PostgresDb) -> Uuid {
    let entity_type = Uuid::now_v7();
    let jurisdiction = Uuid::now_v7();
    let entity = Uuid::now_v7();
    sqlx::query("INSERT INTO entity_type (id, name) VALUES ($1, $2)")
        .bind(entity_type)
        .bind(format!("Synthetic Entity Type {}", Uuid::now_v7()))
        .execute(pg.pool())
        .await
        .expect("synthetic Entity Type inserts");
    sqlx::query("INSERT INTO jurisdiction (id, name) VALUES ($1, $2)")
        .bind(jurisdiction)
        .bind(format!("Synthetic Jurisdiction {}", Uuid::now_v7()))
        .execute(pg.pool())
        .await
        .expect("synthetic Jurisdiction inserts");
    sqlx::query("INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) VALUES ($1, 'Synthetic Entity', $2, $3)")
        .bind(entity)
        .bind(entity_type)
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .expect("synthetic Entity inserts");
    entity
}

/// Create a synthetic Person for a typed matter-record link.
pub(crate) async fn postgres_person(pg: &PostgresDb, email: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO person (id, name, email) VALUES ($1, 'Synthetic Person', $2)")
        .bind(id)
        .bind(email)
        .execute(pg.pool())
        .await
        .expect("synthetic Person inserts");
    id
}

#[test]
fn every_authoritative_table_is_a_registered_record_table() {
    for table in TABLES {
        assert!(
            crate::postgres::record_metadata::columns(table).is_some(),
            "{table}"
        );
    }
}

#[test]
fn a_tagged_link_round_trips() {
    let id = Uuid::now_v7();
    assert_eq!(untagged(&tagged("entity", id)), Some(("entity", id)));
    assert_eq!(untagged(&serde_json::json!({"tb": "entity"})), None);
}

#[tokio::test]
async fn the_accessor_refuses_unregistered_tables_and_columns() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    assert!(matches!(
        select::<Value>(&pg, "person", serde_json::json!({}), &[]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        select::<Value>(&pg, "git_repository", serde_json::json!({"nope": 1}), &[]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        select::<Value>(&pg, "git_repository", serde_json::json!({}), &["nope DESC"]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        delete(&pg, "git_repository", serde_json::json!({"id": null})).await,
        Ok(0)
    ));
}
