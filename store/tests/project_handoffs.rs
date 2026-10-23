//! Legal-files-only handoffs: the recipient's lens, current revisions only,
//! verified bytes from active or archived storage, and immutable manifests.
use std::io::{Cursor, Read};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use cloud::{FsStorage, StorageService};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::assets::{self, Filed};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::project_handoffs::{self as handoffs, HandoffError, MANIFEST_PATH};
use store::projects::{self, NewProject, Project};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand, MemoryStorage};
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
    let name = format!("handoff_test_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

struct World {
    pg: PostgresDb,
    surreal: SurrealDb,
    documents: Arc<dyn StorageService>,
    project: Project,
    lawyer: (Uuid, Role),
    client: Uuid,
    _dir: tempfile::TempDir,
}

impl World {
    async fn file(&self, slug: &str, name: &str, bytes: &[u8], visible: &str, kind: &str) -> Uuid {
        let args = IngestArgs {
            project_id: self.project.id,
            source: "upload",
            filename: name,
            kind,
            content_type: "application/pdf",
            description: None,
            visibility: visible,
            secondary_storage_key: None,
        };
        let identity = DocumentIdentity {
            slug: Some(slug),
            ..DocumentIdentity::default()
        };
        match assets::file_revision(&self.surreal, &self.documents, &args, &identity, bytes)
            .await
            .unwrap()
        {
            Filed::Revision(document) => document.asset_id,
            Filed::Unchanged { asset_id } => asset_id,
        }
    }
}

async fn participant(surreal: &SurrealDb, project_id: Uuid, name: &str, role: Role) -> Uuid {
    let person = persons::create(
        surreal,
        &NewPerson::with_role(
            name,
            format!("{}@example.com", name.replace(' ', ".")),
            role,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: person.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    person.id
}

async fn world() -> Option<World> {
    let pg = database().await?;
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = projects::create(
        &surreal,
        &NewProject {
            code: format!("handoff-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Handoff Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = participant(&surreal, project.id, "Handoff Lawyer", Role::Lawyer).await;
    let client = participant(&surreal, project.id, "Handoff Client", Role::Client).await;
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    Some(World {
        pg,
        surreal,
        documents,
        project,
        lawyer: (lawyer, Role::Lawyer),
        client,
        _dir: dir,
    })
}

fn entries(bundle: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bundle)).unwrap();
    (0..archive.len())
        .map(|index| {
            let mut file = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (file.name().unwrap().to_string(), bytes)
        })
        .collect()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_client_receives_only_current_visible_legal_files_with_a_verified_manifest() {
    let Some(world) = world().await else {
        return;
    };
    world
        .file(
            "engagement",
            "engagement.pdf",
            b"v1",
            visibility::CLIENT,
            "unclassified",
        )
        .await;
    let current = world
        .file(
            "engagement",
            "engagement.pdf",
            b"v2",
            visibility::CLIENT,
            "unclassified",
        )
        .await;
    world
        .file(
            "memo",
            "memo.pdf",
            b"internal",
            visibility::INTERNAL,
            "unclassified",
        )
        .await;
    let deleted = world
        .file(
            "withdrawn",
            "withdrawn.pdf",
            b"gone",
            visibility::CLIENT,
            "unclassified",
        )
        .await;
    assets::soft_delete(
        &world.surreal,
        world.project.id,
        deleted,
        world.lawyer.0,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    world
        .file(
            "closed_repository.zip",
            "repo.zip",
            b"source and history",
            visibility::CLIENT,
            "closed_repository",
        )
        .await;
    let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();

    let handoff = handoffs::prepare(
        &world.surreal,
        &world.pg,
        &world.documents,
        None,
        world.project.id,
        world.lawyer,
        world.client,
        true,
        now,
    )
    .await
    .unwrap();
    assert_eq!(handoff.lens, "client");
    assert!(handoff.required);
    assert_eq!(handoff.file_count, 1);
    assert_eq!(handoff.manifest.files[0].asset_id, current);
    assert_eq!(handoff.manifest.files[0].sha256, assets::sha256_hex(b"v2"));

    let bundle = handoffs::bundle(&world.surreal, &world.documents, None, &handoff)
        .await
        .unwrap();
    let files = entries(&bundle);
    assert_eq!(files[0].0, MANIFEST_PATH);
    assert_eq!(files.len(), 2);
    assert_eq!(files[1], ("engagement.pdf".to_string(), b"v2".to_vec()));
    assert_eq!(
        bundle,
        handoffs::bundle(&world.surreal, &world.documents, None, &handoff)
            .await
            .unwrap(),
        "the same handoff is the same bytes"
    );

    // The firm-side recipient's lens includes internal files but still no
    // source, history, or deleted chains.
    let firm_side = handoffs::prepare(
        &world.surreal,
        &world.pg,
        &world.documents,
        None,
        world.project.id,
        world.lawyer,
        world.lawyer.0,
        false,
        now,
    )
    .await
    .unwrap();
    let names: Vec<_> = firm_side
        .manifest
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(names, ["engagement.pdf", "memo.pdf"]);

    // The manifest is immutable.
    assert!(
        sqlx::query("UPDATE project_handoff SET required = false WHERE id = $1")
            .bind(handoff.id)
            .execute(world.pg.pool())
            .await
            .is_err()
    );
}

/// Two legal files that share a filename both travel, under distinct entry
/// names, and the Project lists every handoff it recorded.
#[tokio::test]
async fn same_named_files_get_distinct_bundle_entries_and_handoffs_are_listed() {
    let Some(world) = world().await else {
        return;
    };
    for (slug, bytes) in [("letter-a", b"first".as_slice()), ("letter-b", b"second")] {
        world
            .file(
                slug,
                "letter.pdf",
                bytes,
                visibility::CLIENT,
                "unclassified",
            )
            .await;
    }
    let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
    let handoff = handoffs::prepare(
        &world.surreal,
        &world.pg,
        &world.documents,
        None,
        world.project.id,
        world.lawyer,
        world.client,
        false,
        now,
    )
    .await
    .unwrap();
    assert_eq!(handoff.file_count, 2);
    let bundle = handoffs::bundle(&world.surreal, &world.documents, None, &handoff)
        .await
        .unwrap();
    let mut names: Vec<String> = entries(&bundle).into_iter().map(|(name, _)| name).collect();
    names.sort();
    assert_eq!(names, [MANIFEST_PATH, "letter (2).pdf", "letter.pdf"]);

    let listed = handoffs::for_project(&world.pg, world.project.id)
        .await
        .unwrap();
    assert_eq!(listed, vec![handoff]);
    assert!(handoffs::for_project(&world.pg, Uuid::new_v4())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn missing_or_damaged_bytes_fail_the_handoff_and_archived_copies_are_equivalent() {
    let Some(world) = world().await else {
        return;
    };
    let id = world
        .file(
            "order",
            "order.pdf",
            b"court order",
            visibility::CLIENT,
            "unclassified",
        )
        .await;
    let row = assets::find_by_id(&world.surreal, id)
        .await
        .unwrap()
        .unwrap();
    let firm_id = world.project.firm_id.unwrap();
    let archive_store = Arc::new(MemoryStorage::default());
    let archive: Arc<dyn StorageService> = archive_store.clone();
    archive
        .put(
            &store::project_archives::object_key(firm_id, world.project.id, &row.sha256_hex),
            b"court order",
            "application/pdf",
        )
        .await
        .unwrap();
    let now = Utc::now();
    let from_active = handoffs::prepare(
        &world.surreal,
        &world.pg,
        &world.documents,
        Some(&archive),
        world.project.id,
        world.lawyer,
        world.client,
        false,
        now,
    )
    .await
    .unwrap();
    world.documents.delete(&row.storage_key).await.unwrap();
    let from_archive = handoffs::prepare(
        &world.surreal,
        &world.pg,
        &world.documents,
        Some(&archive),
        world.project.id,
        world.lawyer,
        world.client,
        false,
        now,
    )
    .await
    .unwrap();
    assert_eq!(from_active.manifest.files, from_archive.manifest.files);
    assert_eq!(
        entries(
            &handoffs::bundle(
                &world.surreal,
                &world.documents,
                Some(&archive),
                &from_archive
            )
            .await
            .unwrap()
        )[1]
        .1,
        b"court order"
    );
    assert!(matches!(
        handoffs::prepare(
            &world.surreal,
            &world.pg,
            &world.documents,
            None,
            world.project.id,
            world.lawyer,
            world.client,
            false,
            now
        )
        .await,
        Err(HandoffError::Missing(_))
    ));
    world
        .documents
        .put(&row.storage_key, b"tampered", "application/pdf")
        .await
        .unwrap();
    assert!(matches!(
        handoffs::prepare(
            &world.surreal,
            &world.pg,
            &world.documents,
            None,
            world.project.id,
            world.lawyer,
            world.client,
            false,
            now
        )
        .await,
        Err(HandoffError::Corrupt(_))
    ));
}

#[tokio::test]
async fn outsiders_and_non_participants_are_refused() {
    let Some(world) = world().await else {
        return;
    };
    let outsider = persons::create(
        &world.surreal,
        &NewPerson::with_role("Outside Lawyer", "outside.lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    assert!(matches!(
        handoffs::prepare(
            &world.surreal,
            &world.pg,
            &world.documents,
            None,
            world.project.id,
            (outsider.id, Role::Lawyer),
            world.client,
            false,
            Utc::now()
        )
        .await,
        Err(HandoffError::ProjectNotFound)
    ));
    assert!(matches!(
        handoffs::prepare(
            &world.surreal,
            &world.pg,
            &world.documents,
            None,
            world.project.id,
            world.lawyer,
            outsider.id,
            false,
            Utc::now()
        )
        .await,
        Err(HandoffError::RecipientNotParticipant)
    ));
}
