//! Mirror harness evidence for the Template table: write, idempotence,
//! parent cascade, drift repair, and deletion — against a fresh PostgreSQL
//! database per test and in-memory SurrealDB as the source of truth.
use super::*;
use crate::{
    postgres_mirror::tests::{connect, fresh_schema, synthetic_entity},
    templates,
    test_support::mem_surreal,
};

fn version(title: &str) -> templates::Version {
    templates::Version {
        title: title.into(),
        respondent_type: "person".into(),
        asset_id: None,
        form_code: None,
        kind: Some("letter".into()),
        source_commit_sha: Some("9d3f1c2".into()),
    }
}

async fn harness() -> Option<(crate::surreal::SurrealDb, crate::postgres::PostgresDb)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    Some((surreal, pg))
}

#[tokio::test]
async fn mirror_writes_a_shared_template_then_reports_unchanged() {
    let Some((surreal, pg)) = harness().await else {
        return;
    };
    let saved = templates::save_version(&surreal, None, "llc_formation", version("Articles"))
        .await
        .unwrap();
    let template = saved.into_model();

    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Unchanged
    );

    let row: (String, String, bool, Option<String>) = sqlx::query_as(
        "SELECT code, title, is_current, source_commit_sha FROM template WHERE id = $1",
    )
    .bind(template.id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(row.0, "llc_formation");
    assert_eq!(row.1, "Articles");
    assert!(row.2);
    assert_eq!(row.3.as_deref(), Some("9d3f1c2"));

    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn mirror_deletes_the_postgres_row_once_the_source_row_is_gone() {
    let Some((surreal, pg)) = harness().await else {
        return;
    };
    let saved = templates::save_version(&surreal, None, "will_questionnaire", version("Will"))
        .await
        .unwrap();
    let template = saved.into_model();
    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Written
    );

    surreal
        .query("DELETE $id")
        .bind((
            "id",
            crate::surreal::record_id(templates::TABLE, template.id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Deleted
    );
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM template WHERE id = $1")
        .bind(template.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(stored, 0);
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn reconcile_repairs_a_drifted_template_row() {
    let Some((surreal, pg)) = harness().await else {
        return;
    };
    let saved = templates::save_version(&surreal, None, "tenant_answer", version("Answer"))
        .await
        .unwrap();
    let template = saved.into_model();
    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Written
    );

    sqlx::query("UPDATE template SET title = 'Drifted' WHERE id = $1")
        .bind(template.id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());

    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean());
    let title: String = sqlx::query_scalar("SELECT title FROM template WHERE id = $1")
        .bind(template.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(title, "Answer");
    pg.0.close().await;
}

#[tokio::test]
async fn mirror_follows_a_project_parent_on_demand() {
    let Some((surreal, pg)) = harness().await else {
        return;
    };
    // A Project-scoped template names a parent the mirror has never seen:
    // the mirror must materialize the Project row before the template's.
    // The project needs a real entity behind it, and the version needs a
    // body asset for a project scope, so seed both the usual way.
    let entity = synthetic_entity(&surreal).await;
    let project = crate::projects::create(
        &surreal,
        &crate::projects::NewProject {
            code: format!("mirror-cascade-{}", Uuid::now_v7()),
            name: "Mirror cascade".into(),
            status: "open".into(),
            entity_id: entity,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
        cloud::FsStorage::new(tmp.path().to_path_buf())
            .await
            .unwrap(),
    );
    let asset = crate::assets::ingest_content(&surreal, &storage, b"# Engagement", "text/markdown")
        .await
        .unwrap();

    let saved = templates::save_version(
        &surreal,
        Some(project.id),
        "engagement_letter",
        crate::templates::Version {
            asset_id: Some(asset),
            ..version("Engagement")
        },
    )
    .await
    .unwrap();
    let template = saved.into_model();

    assert_eq!(
        mirror(&surreal, &pg, template.id).await.unwrap(),
        Mirrored::Written
    );
    let parents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project WHERE id = $1")
        .bind(project.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(parents, 1);
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}
