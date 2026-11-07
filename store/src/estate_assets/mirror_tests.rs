use std::process::Command;

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::*;
use crate::postgres::PostgresDb;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::{mem_surreal, seed_entity};

#[test]
fn estate_asset_write_hooks_use_isolated_postgres() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Some((_, database)) = runtime.block_on(fresh_schema()) else {
        return;
    };
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "estate_assets::mirror_tests::estate_asset_write_hook_probe",
            "--nocapture",
        ])
        .env("NAVIGATOR_ESTATE_ASSET_MIRROR_PROBE", "1")
        .env("NAVIGATOR_POSTGRES_MIRROR", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env(
            "NAVIGATOR_POSTGRES_PORT",
            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
        )
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Estate Asset write-hook probe: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn project(surreal: &SurrealDb, code: &str) -> Uuid {
    crate::projects::create(
        surreal,
        &crate::projects::NewProject {
            code: code.into(),
            name: "Synthetic schedule".into(),
            status: "open".into(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

fn fields(description: &str) -> EstateAssetFields {
    EstateAssetFields {
        category: "account".into(),
        description: description.into(),
        passes_by: "trust".into(),
        funding_state: "in_progress".into(),
        note: Some("Synthetic schedule note".into()),
    }
}

async fn assert_image(pg: &PostgresDb, expected: &EstateAsset) {
    let row = sqlx::query("SELECT * FROM estate_asset WHERE id = $1")
        .bind(expected.id)
        .fetch_optional(&pg.0)
        .await
        .unwrap()
        .expect("the actual accessor must mirror its successful write");
    assert_eq!(row.get::<Uuid, _>("project_id"), expected.project_id);
    assert_eq!(row.get::<String, _>("category"), expected.category);
    assert_eq!(row.get::<String, _>("description"), expected.description);
    assert_eq!(row.get::<String, _>("passes_by"), expected.passes_by);
    assert_eq!(
        row.get::<String, _>("funding_state"),
        expected.funding_state
    );
    assert_eq!(row.get::<Option<String>, _>("note"), expected.note);
    for (column, value) in [
        ("inserted_at", &expected.inserted_at),
        ("updated_at", &expected.updated_at),
    ] {
        assert_eq!(
            row.get::<DateTime<Utc>, _>(column).timestamp_micros(),
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .timestamp_micros()
        );
    }
}

async fn stored_ids(pg: &PostgresDb) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM estate_asset ORDER BY id")
        .fetch_all(&pg.0)
        .await
        .unwrap()
}

async fn assert_note(pg: &PostgresDb, id: Uuid, expected: &str) {
    let note: Option<String> = sqlx::query_scalar("SELECT note FROM estate_asset WHERE id = $1")
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(note.as_deref(), Some(expected));
}

async fn insert_row_without_funding_state(surreal: &SurrealDb, pg: &PostgresDb, project: Uuid) {
    let id = Uuid::new_v4();
    surreal
        .query(
            "DEFINE FIELD OVERWRITE funding_state ON estate_asset TYPE option<string>; \
             CREATE $id SET project_id = $project, category = 'account', \
             description = 'Synthetic incomplete row', passes_by = 'trust', \
             inserted_at = $now, updated_at = $now",
        )
        .bind(("id", record_id(TABLE, id)))
        .bind(("project", record_id("project", project)))
        .bind(("now", Utc::now().to_rfc3339()))
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query(
        "INSERT INTO estate_asset \
         (id, project_id, category, description, passes_by, funding_state, inserted_at, updated_at) \
         VALUES ($1, $2, 'account', 'Synthetic incomplete row', 'trust', 'funded', now(), now())",
    )
    .bind(id)
    .bind(project)
    .execute(&pg.0)
    .await
    .unwrap();
}

#[tokio::test]
async fn estate_asset_write_hook_probe() {
    if std::env::var("NAVIGATOR_ESTATE_ASSET_MIRROR_PROBE").as_deref() != Ok("1") {
        return;
    }
    let (options, _) = crate::postgres_mirror::tests::test_postgres().unwrap();
    let pg = connect(
        &options,
        &std::env::var("NAVIGATOR_POSTGRES_DATABASE").unwrap(),
    )
    .await;
    let surreal = mem_surreal().await;
    let first = project(&surreal, "synthetic-mirror-schedule").await;
    let second = project(&surreal, "synthetic-other-schedule").await;

    let asset = insert(&surreal, first, &fields("Synthetic account"))
        .await
        .unwrap();
    assert_image(&pg, &asset).await;
    let other = insert(&surreal, second, &fields("Synthetic account"))
        .await
        .unwrap();
    assert_image(&pg, &other).await;
    sqlx::query("UPDATE estate_asset SET note = 'Synthetic target sentinel' WHERE id = $1")
        .bind(asset.id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(matches!(
        update(&surreal, second, asset.id, &fields("Denied change")).await,
        Err(EstateAssetError::NotFound)
    ));
    assert_note(&pg, asset.id, "Synthetic target sentinel").await;
    assert!(matches!(
        delete(&surreal, second, asset.id).await,
        Err(EstateAssetError::NotFound)
    ));
    assert_note(&pg, asset.id, "Synthetic target sentinel").await;
    assert_eq!(
        find(&surreal, first, asset.id).await.unwrap(),
        Some(asset.clone())
    );
    assert_image(&pg, &other).await;

    let updated = update(
        &surreal,
        first,
        asset.id,
        &EstateAssetFields {
            funding_state: "funded".into(),
            note: None,
            ..fields("Synthetic funded account")
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.inserted_at, asset.inserted_at);
    assert_image(&pg, &updated).await;
    let key = crate::postgres_mirror::records::key(TABLE, asset.id).unwrap();
    assert_eq!(
        crate::postgres_mirror::mirror(&surreal, &pg, &key)
            .await
            .unwrap(),
        crate::postgres_mirror::Mirrored::Unchanged,
        "a replay after the accessor's write changes no target row"
    );

    delete(&surreal, first, asset.id).await.unwrap();
    assert_eq!(stored_ids(&pg).await, vec![other.id]);
    assert!(matches!(
        delete(&surreal, first, asset.id).await,
        Err(EstateAssetError::NotFound)
    ));
    for description in ["Synthetic first line", "Synthetic second line"] {
        let asset = insert(&surreal, first, &fields(description)).await.unwrap();
        assert_image(&pg, &asset).await;
    }
    insert_row_without_funding_state(&surreal, &pg, first).await;
    delete_for_project(&surreal, first).await.unwrap();
    assert!(list(&surreal, first).await.unwrap().is_empty());
    assert_eq!(stored_ids(&pg).await, vec![other.id]);
    assert_image(&pg, &other).await;
    delete_for_project(&surreal, first).await.unwrap();
    assert_eq!(stored_ids(&pg).await, vec![other.id]);
    pg.0.close().await;
}
