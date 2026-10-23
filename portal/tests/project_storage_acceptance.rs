//! The synthetic Project storage lifecycle end to end, at the store and
//! service seams, against real PostgreSQL and the embedded `SurrealDB`:
//! initialize source, file and ingest documents, soft-delete and restore,
//! hold, hand off and acknowledge, close, archive a complete generation,
//! gate cleanup, restore in isolation, and purge — with one injected failure
//! at each external boundary (forge, Drive, archive storage, worker restart
//! by re-running every step).
//!
//! Forgejo, Drive, and object storage are synthetic doubles here; the same
//! scenario against a deployed Forgejo and the KIND tier is the remaining
//! operational proof.
use std::collections::BTreeMap;
use std::io::{Cursor, Write as _};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use cloud::forge::ForgeService;
use cloud::{DriveService, FakeDrive, FakeForge, FsStorage, StorageService};
use portal::expunge::purge::{purge_expired_documents_with, PurgeAuthority, RetentionLanes};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::assets::{self, Filed};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::project_closeout::{self as closeout, Closeout};
use store::projects::{self, ClosureReason, NewProject, Transition};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity, MemoryStorage};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
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
    let name = format!("acceptance_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
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

fn source_archive() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, text) in [
        ("navigator.yaml", "version: 1\n"),
        ("drafts/onboarding__letter.md", "# Engagement\n"),
        ("blobs/engagement.pdf.yml", "slug: engagement\n"),
    ] {
        writer
            .start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

async fn file(
    surreal: &SurrealDb,
    documents: &Arc<dyn StorageService>,
    project_id: Uuid,
    slug: &str,
    kind: &str,
    bytes: &[u8],
    visible: &str,
) -> Uuid {
    let args = IngestArgs {
        project_id,
        source: "upload",
        filename: &format!("{slug}.pdf"),
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
    match assets::file_revision(surreal, documents, &args, &identity, bytes)
        .await
        .unwrap()
    {
        Filed::Revision(document) => document.asset_id,
        Filed::Unchanged { asset_id } => asset_id,
    }
}

/// A malware screen that finds nothing.
struct Clean;

#[async_trait::async_trait]
impl store::drive_import::IngestScreen for Clean {
    async fn is_clean(&self, _: &[u8]) -> Result<bool, ()> {
        Ok(true)
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_synthetic_project_survives_its_whole_storage_lifecycle() {
    let Some(pg) = database().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let admin = persons::create(
        &surreal,
        &NewPerson::with_role(
            "Acceptance Admin",
            "acceptance.admin@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Acceptance Synthetic Firm".into(),
            status: "active".into(),
            entity_id: seed_entity(&surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let project = projects::create(
        &surreal,
        &NewProject {
            code: format!("acceptance-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Acceptance Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = participant(&surreal, project.id, "Acceptance Lawyer", Role::Lawyer).await;
    let client = participant(&surreal, project.id, "Acceptance Client", Role::Client).await;
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    let archive_store = Arc::new(MemoryStorage::default());
    let archive: Arc<dyn StorageService> = archive_store.clone();

    // 1. Initialize source on the forge; the first write's reply is lost.
    let forge = FakeForge::new();
    let import = store::project_source_imports::request(
        &surreal,
        &pg,
        project.id,
        (lawyer, Role::Lawyer),
        "acceptance-source",
        &source_archive(),
        at(1),
    )
    .await
    .unwrap();
    forge.lose_next_write_reply();
    assert!(
        store::project_source_imports::process(&surreal, &pg, &forge, import.id, at(2))
            .await
            .is_err()
    );
    let import = store::project_source_imports::process(&surreal, &pg, &forge, import.id, at(3))
        .await
        .unwrap();
    assert_eq!(forge.initialize_writes(), 1);
    let head = import.commit_sha.clone().unwrap();
    let committed = forge.committed_paths(&project.code);
    assert!(
        committed.iter().all(|path| !std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))),
        "no legal bytes enter Git: {committed:?}"
    );

    // 2. File documents and revisions.
    file(
        &surreal,
        &documents,
        project.id,
        "engagement",
        "onboarding",
        b"engagement v1",
        visibility::CLIENT,
    )
    .await;
    let engagement = file(
        &surreal,
        &documents,
        project.id,
        "engagement",
        "onboarding",
        b"engagement v2",
        visibility::CLIENT,
    )
    .await;
    let memo = file(
        &surreal,
        &documents,
        project.id,
        "memo",
        "unclassified",
        b"internal memo",
        visibility::INTERNAL,
    )
    .await;
    let draft = file(
        &surreal,
        &documents,
        project.id,
        "draft",
        "unclassified",
        b"withdrawn draft",
        visibility::INTERNAL,
    )
    .await;

    // 3. Optional Drive ingest; the first download fails.
    let drive = FakeDrive::default();
    let folder = drive
        .create_folder("acceptance display name")
        .await
        .unwrap();
    projects::set_drive_folder_id(&surreal, project.id, Some(&folder.id))
        .await
        .unwrap();
    drive
        .add_file(
            &folder.id,
            "intake.pdf",
            "application/pdf",
            b"intake form".to_vec(),
            None,
        )
        .unwrap();
    let drive_args = store::drive_import::DriveImportArgs {
        kind: "unclassified",
        visibility: visibility::CLIENT,
        description: None,
    };
    drive.fail_downloads(1);
    assert!(store::drive_import::import_project_files(
        &surreal,
        &documents,
        &drive,
        &Clean,
        project.id,
        lawyer,
        &drive_args
    )
    .await
    .is_err());
    let ingested = store::drive_import::import_project_files(
        &surreal,
        &documents,
        &drive,
        &Clean,
        project.id,
        lawyer,
        &drive_args,
    )
    .await
    .unwrap();
    assert_eq!(ingested.imported.len(), 1);

    // 4. Soft-delete, archive (one storage failure), and restore a chain.
    assets::soft_delete(&surreal, project.id, memo, lawyer, None, at(10))
        .await
        .unwrap();
    let retention = store::document_retention::RetentionStores {
        surreal: &surreal,
        pg: &pg,
        documents: &documents,
        archive: &archive,
    };
    archive_store.fail_before_write.store(1, Ordering::SeqCst);
    let stalled = store::document_retention::reconcile_soft_deleted(&retention, at(11), 100)
        .await
        .unwrap();
    assert_eq!(stalled.stalled.len(), 1);
    let archived = store::document_retention::reconcile_soft_deleted(&retention, at(12), 100)
        .await
        .unwrap();
    assert_eq!(archived.active_removed, 1);
    store::document_retention::restore_chain(
        &surreal,
        Some(&pg),
        &documents,
        Some(&archive),
        project.id,
        memo,
        at(13),
    )
    .await
    .unwrap();
    let memo_row = assets::find_by_id(&surreal, memo).await.unwrap().unwrap();
    assert_eq!(
        documents.get(&memo_row.storage_key).await.unwrap().bytes,
        b"internal memo"
    );

    // 5. Soft-delete and hold another chain.
    assets::soft_delete(&surreal, project.id, draft, lawyer, None, at(20))
        .await
        .unwrap();
    assets::set_legal_hold(&surreal, project.id, draft, true, at(21))
        .await
        .unwrap();

    // 6. Required legal-files-only handoff, acknowledged by the client.
    let handoff = store::project_handoffs::prepare(
        &surreal,
        &pg,
        &documents,
        Some(&archive),
        project.id,
        (lawyer, Role::Lawyer),
        client,
        true,
        at(30),
    )
    .await
    .unwrap();
    let shipped: Vec<_> = handoff
        .manifest
        .files
        .iter()
        .map(|file| file.asset_id)
        .collect();
    assert!(shipped.contains(&engagement));
    assert!(
        !shipped.contains(&memo),
        "internal work product never ships to the client"
    );
    closeout::acknowledge(
        &pg,
        project.id,
        handoff.id,
        client,
        &handoff.manifest_sha256,
        at(31),
    )
    .await
    .unwrap();

    // 7. Close, then archive a complete generation (one unknown outcome).
    projects::transition_project_with_reason(
        &surreal,
        project.id,
        Transition::Close,
        Some(ClosureReason::EngagementCompleted),
        None,
    )
    .await
    .unwrap();
    let authority = store::project_archives::authorize(&surreal, Role::Admin, admin.id, firm.id)
        .await
        .unwrap();
    let bundles: Arc<dyn StorageService> = Arc::new(MemoryStorage::default());
    let bundle = bundles
        .put_immutable(
            "repositories/acceptance.bundle",
            b"synthetic bundle",
            "application/x-git-bundle",
        )
        .await
        .unwrap();
    let stores = store::project_archives::ArchiveStores {
        surreal: &surreal,
        pg: &pg,
        documents: &documents,
        archive: &archive,
        source_backups: Some(&bundles),
    };
    let receipt = store::project_archives::SourceBackupReference {
        format: 1,
        object: "repositories/acceptance.bundle".into(),
        generation: bundle.generation,
        size_bytes: bundle.size_bytes,
        sha256: assets::sha256_hex(b"synthetic bundle"),
        head: "refs/heads/main".into(),
        refs: format!("refs/heads/main {head}"),
    };
    let now = Utc::now();
    let generation = store::project_archives::request(
        &stores,
        &authority,
        project.id,
        "acceptance-close",
        Some(receipt),
        now,
    )
    .await
    .unwrap();
    archive_store.fail_after_write.store(1, Ordering::SeqCst);
    let generation = store::project_archives::run(&stores, &authority, generation.id, now)
        .await
        .unwrap();
    assert_eq!(
        generation.state,
        store::project_archives::GenerationState::Committed
    );

    // 8. Cleanup gates: archival is ready; deletion waits on the hold.
    assert!(
        closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
            .await
            .unwrap()
            .is_ready()
    );
    let delete = closeout::readiness(
        &surreal,
        Some(&pg),
        project.id,
        Closeout::DeleteRepository { live_head: &head },
    )
    .await
    .unwrap();
    assert_eq!(
        delete.blockers,
        vec!["a Legal Hold stands on the Project".to_string()]
    );
    assert_eq!(
        forge.head_commit_sha(&project.code).await.unwrap(),
        Some(head.clone())
    );

    // 9. Restore the generation into an isolated store and compare.
    let target = mem_surreal().await;
    let stored_firm = store::firms::find_by_id(&surreal, firm.id)
        .await
        .unwrap()
        .unwrap();
    store::test_support::seed_restore_prerequisites(&target, &stored_firm, project.entity_id).await;
    let target_dir = tempfile::tempdir().unwrap();
    let target_documents: Arc<dyn StorageService> =
        Arc::new(FsStorage::new(target_dir.path()).await.unwrap());
    let restored = store::project_archives::restore(
        &pg,
        &archive,
        &authority,
        generation.id,
        &store::project_archives::RestoreTarget {
            surreal: &target,
            documents: &target_documents,
        },
    )
    .await
    .unwrap();
    assert_eq!(restored.source_backup.unwrap().head, "refs/heads/main");
    let source_rows = assets::for_project_including_deleted(&surreal, project.id)
        .await
        .unwrap();
    let target_rows = assets::for_project_including_deleted(&target, project.id)
        .await
        .unwrap();
    assert_eq!(
        source_rows, target_rows,
        "identities, checksums, provenance, visibility, stamps"
    );
    for row in &target_rows {
        assert_eq!(
            assets::sha256_hex(&target_documents.get(&row.storage_key).await.unwrap().bytes),
            row.sha256_hex
        );
    }

    // 10. The purge: the held chain stays; release it and the window passes.
    let other = persons::create(
        &surreal,
        &NewPerson::with_role("Other Admin", "other.admin@example.com", Role::Admin),
    )
    .await
    .unwrap();
    let lanes = RetentionLanes {
        pg: Some(&pg),
        archive: Some(&archive),
    };
    let after_window = at(20) + Duration::days(assets::RETENTION_DAYS);
    let firm_authority = PurgeAuthority::ByFirm(BTreeMap::from([(firm.id, admin.id)]));
    let while_held =
        purge_expired_documents_with(&surreal, &documents, lanes, &firm_authority, after_window)
            .await
            .unwrap();
    assert_eq!(while_held.held, 1);
    let foreign = PurgeAuthority::ByFirm(BTreeMap::from([(Uuid::now_v7(), other.id)]));
    assets::set_legal_hold(&surreal, project.id, draft, false, at(40))
        .await
        .unwrap();
    let refused = purge_expired_documents_with(&surreal, &documents, lanes, &foreign, after_window)
        .await
        .unwrap();
    assert_eq!(
        refused.unauthorized, 1,
        "another Firm's authority never purges this matter"
    );
    let purged =
        purge_expired_documents_with(&surreal, &documents, lanes, &firm_authority, after_window)
            .await
            .unwrap();
    assert_eq!(purged.purged.len(), 1);
    let report =
        store::project_archives::verify(&pg, &archive, Some(&bundles), &authority, generation.id)
            .await
            .unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.purged, vec![format!("documents/{draft}")]);
    assert!(report.source_backup_verified);

    // Cross-Firm access to the archive is denied.
    let other_firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Unrelated Synthetic Firm".into(),
            status: "active".into(),
            entity_id: seed_entity(&surreal).await,
            admin_dri_person_id: other.id,
        },
    )
    .await
    .unwrap();
    let foreign_authority =
        store::project_archives::authorize(&surreal, Role::Admin, other.id, other_firm.id)
            .await
            .unwrap();
    assert!(
        store::project_archives::find(&pg, &foreign_authority, generation.id)
            .await
            .is_err()
    );
}
