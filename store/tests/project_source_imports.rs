//! Durable, idempotent source initialization from a validated headless archive
//! against real PostgreSQL, the embedded `SurrealDB`, and a fake forge.
use std::io::{Cursor, Write as _};

use chrono::{TimeZone, Utc};
use cloud::forge::{ForgeError, ForgeService};
use cloud::FakeForge;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::project_source_imports::{self as imports, ImportState, SourceImportError};
use store::projects::{self, NewProject, Project};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("source_import_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn source() -> Vec<u8> {
    archive(&[
        ("matter/navigator.yaml", "version: 1\n"),
        ("matter/drafts/onboarding__letter.md", "# Draft\n"),
    ])
}

struct World {
    pg: PostgresDb,
    surreal: SurrealDb,
    project: Project,
    lawyer: (Uuid, Role),
}

async fn world() -> Option<World> {
    let pg = database().await?;
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = projects::create(
        &surreal,
        &NewProject {
            code: format!("import-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Import Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = persons::create(
        &surreal,
        &NewPerson::with_role("Import Lawyer", "import-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id: project.id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    Some(World {
        pg,
        surreal,
        project,
        lawyer: (lawyer.id, Role::Lawyer),
    })
}

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

async fn rows(pg: &PostgresDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM project_source_import")
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn invalid_or_unauthorized_requests_write_nothing() {
    let Some(world) = world().await else {
        return;
    };
    let hostile = archive(&[("../escape.md", "x")]);
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            world.lawyer,
            "k1",
            &hostile,
            at(1)
        )
        .await,
        Err(SourceImportError::Archive(_))
    ));
    let client = persons::create(
        &world.surreal,
        &NewPerson::new("Import Client", "import-client@example.com"),
    )
    .await
    .unwrap();
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            (client.id, Role::Client),
            "k2",
            &source(),
            at(2)
        )
        .await,
        Err(SourceImportError::ProjectNotFound)
    ));
    assert_eq!(rows(&world.pg).await, 0);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_lost_forge_reply_converges_on_one_commit_and_truthful_timestamps() {
    let Some(world) = world().await else {
        return;
    };
    let requested = imports::request(
        &world.surreal,
        &world.pg,
        world.project.id,
        world.lawyer,
        "init-1",
        &source(),
        at(10),
    )
    .await
    .unwrap();
    assert_eq!(requested.state, ImportState::Pending);
    assert_eq!(requested.file_count, 2);
    // A replay returns the same import; a second pending import is refused.
    let replay = imports::request(
        &world.surreal,
        &world.pg,
        world.project.id,
        world.lawyer,
        "init-1",
        &source(),
        at(11),
    )
    .await
    .unwrap();
    assert_eq!(replay.id, requested.id);
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            world.lawyer,
            "init-2",
            &source(),
            at(12)
        )
        .await,
        Err(SourceImportError::ImportPending)
    ));
    let other_tree = archive(&[("navigator.yaml", "version: 2\n")]);
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            world.lawyer,
            "init-1",
            &other_tree,
            at(13)
        )
        .await,
        Err(SourceImportError::OperationKeyConflict)
    ));

    let forge = FakeForge::new();
    forge.lose_next_write_reply();
    let stalled = imports::process(&world.surreal, &world.pg, &forge, requested.id, at(20)).await;
    assert!(stalled.is_err());
    let project = projects::find_by_id(&world.surreal, world.project.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        project.git_initialized_at.is_none(),
        "nothing claims source landed before it is verified"
    );
    let pending = imports::find(&world.pg, world.project.id, requested.id)
        .await
        .unwrap();
    assert_eq!(pending.state, ImportState::Pending);
    assert!(pending.last_error.is_some());
    // The durable worker reads the same row by id alone, and finds it in the
    // pending queue.
    assert_eq!(
        imports::get(&world.pg, requested.id).await.unwrap(),
        pending
    );
    assert!(imports::pending(&world.pg, 25)
        .await
        .unwrap()
        .iter()
        .any(|queued| queued.id == requested.id));
    assert!(matches!(
        imports::get(&world.pg, Uuid::new_v4()).await,
        Err(SourceImportError::NotFound)
    ));

    let committed = imports::process(&world.surreal, &world.pg, &forge, requested.id, at(30))
        .await
        .unwrap();
    assert_eq!(committed.state, ImportState::Committed);
    assert_eq!(
        forge.initialize_writes(),
        1,
        "the lost reply did not cause a second commit"
    );
    assert_eq!(
        forge.head_commit_sha(&world.project.code).await.unwrap(),
        committed.commit_sha
    );
    assert_eq!(
        forge
            .read_file(&world.project.code, "navigator.yaml")
            .await
            .unwrap()
            .as_deref(),
        Some("version: 1\n")
    );
    let project = projects::find_by_id(&world.surreal, world.project.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project.repository_url, committed.repository_url);
    assert_eq!(
        project.git_initialized_at.as_deref(),
        Some(at(30).to_rfc3339().as_str())
    );
    assert!(project.forge_provisioned_at.is_some());
    let cleared: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT canonical_zip FROM project_source_import WHERE id = $1")
            .bind(requested.id)
            .fetch_one(world.pg.pool())
            .await
            .unwrap();
    assert!(
        cleared.is_none(),
        "the uploaded archive is discarded once applied"
    );

    // Processing again is a no-op, and a new import is refused.
    let again = imports::process(&world.surreal, &world.pg, &forge, requested.id, at(40))
        .await
        .unwrap();
    assert_eq!(again, committed);
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            world.lawyer,
            "init-3",
            &other_tree,
            at(41)
        )
        .await,
        Err(SourceImportError::AlreadyInitialized)
    ));
}

#[tokio::test]
async fn an_existing_repository_with_other_source_is_never_replaced() {
    let Some(world) = world().await else {
        return;
    };
    let forge = FakeForge::new();
    forge.ensure_repository(&world.project.code).await.unwrap();
    forge.set_head_commit_sha(&world.project.code, "f".repeat(40));
    let requested = imports::request(
        &world.surreal,
        &world.pg,
        world.project.id,
        world.lawyer,
        "replace-1",
        &source(),
        at(1),
    )
    .await
    .unwrap();
    assert!(matches!(
        imports::process(&world.surreal, &world.pg, &forge, requested.id, at(2)).await,
        Err(SourceImportError::Forge(
            ForgeError::RepositoryNotEmpty { .. }
        ))
    ));
    let refused = imports::find(&world.pg, world.project.id, requested.id)
        .await
        .unwrap();
    assert_eq!(refused.state, ImportState::Refused);
    assert_eq!(
        forge.head_commit_sha(&world.project.code).await.unwrap(),
        Some("f".repeat(40))
    );
    assert!(projects::find_by_id(&world.surreal, world.project.id)
        .await
        .unwrap()
        .unwrap()
        .git_initialized_at
        .is_none());
}

#[tokio::test]
async fn keys_are_validated_and_reused_only_for_the_same_tree() {
    let Some(world) = world().await else {
        return;
    };
    for key in ["", "two words", &"k".repeat(400)] {
        assert!(
            matches!(
                imports::request(
                    &world.surreal,
                    &world.pg,
                    world.project.id,
                    world.lawyer,
                    key,
                    &source(),
                    at(1)
                )
                .await,
                Err(SourceImportError::InvalidOperationKey)
            ),
            "{key:?}"
        );
    }
    let first = imports::request(
        &world.surreal,
        &world.pg,
        world.project.id,
        world.lawyer,
        "reuse-1",
        &source(),
        at(2),
    )
    .await
    .unwrap();
    // The same key and tree returns the original request.
    let again = imports::request(
        &world.surreal,
        &world.pg,
        world.project.id,
        world.lawyer,
        "reuse-1",
        &source(),
        at(3),
    )
    .await
    .unwrap();
    assert_eq!(again.id, first.id);
    // The same key with a different tree is a conflict.
    let other = archive(&[("matter/navigator.yaml", "version: 2\n")]);
    assert!(matches!(
        imports::request(
            &world.surreal,
            &world.pg,
            world.project.id,
            world.lawyer,
            "reuse-1",
            &other,
            at(4)
        )
        .await,
        Err(SourceImportError::OperationKeyConflict)
    ));
    assert_eq!(
        imports::get(&world.pg, first.id).await.unwrap().id,
        first.id
    );
    assert!(matches!(
        imports::get(&world.pg, Uuid::now_v7()).await,
        Err(SourceImportError::NotFound)
    ));
    let pending = imports::pending(&world.pg, 10).await.unwrap();
    assert_eq!(
        pending.iter().map(|import| import.id).collect::<Vec<_>>(),
        vec![first.id]
    );
    assert_eq!(rows(&world.pg).await, 1);
}
