//! Project Recovery Archives against real PostgreSQL, the embedded `SurrealDB`,
//! and synthetic object stores with injected faults.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use cloud::{FsStorage, StorageService};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::assets::{self, Filed};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::project_archives::{
    self as archives, ArchiveAuthority, ArchiveError, ArchiveStores, GenerationState,
    RestoreTarget, SourceBackupReference,
};
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
    let name = format!("archive_test_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

use store::test_support::MemoryStorage;

struct World {
    pg: PostgresDb,
    surreal: SurrealDb,
    documents: Arc<dyn StorageService>,
    archive_store: Arc<MemoryStorage>,
    archive: Arc<dyn StorageService>,
    firm_id: Uuid,
    owner: ArchiveAuthority,
    project: Project,
    _documents_dir: tempfile::TempDir,
}

impl World {
    fn stores(&self) -> ArchiveStores<'_> {
        ArchiveStores {
            surreal: &self.surreal,
            pg: &self.pg,
            documents: &self.documents,
            archive: &self.archive,
            source_backups: None,
        }
    }

    async fn file(&self, slug: Option<&str>, filename: &str, bytes: &[u8], visible: &str) -> Uuid {
        let args = IngestArgs {
            project_id: self.project.id,
            source: "upload",
            filename,
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            visibility: visible,
            secondary_storage_key: None,
        };
        let identity = DocumentIdentity {
            slug,
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

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

async fn world() -> Option<World> {
    let pg = database().await?;
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let owner = persons::create(
        &surreal,
        &NewPerson::with_role("Archive Owner", "archive-owner@example.com", Role::Owner),
    )
    .await
    .unwrap();
    let owner = archives::authorize(&surreal, Role::Owner, owner.id, firm_id)
        .await
        .unwrap();
    let project = projects::create(
        &surreal,
        &NewProject {
            code: format!(
                "archive-matter-{}",
                &Uuid::now_v7().simple().to_string()[..8]
            ),
            name: "Synthetic Archive Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    let archive_store = Arc::new(MemoryStorage::default());
    Some(World {
        pg,
        surreal,
        documents,
        archive: archive_store.clone(),
        archive_store,
        firm_id,
        owner,
        project,
        _documents_dir: dir,
    })
}

/// A synthetic record: a two-revision client chain, an internal one-off, a
/// soft-deleted chain, a held chain, a purged tombstone, two chains sharing
/// bytes, and a Notation with a rendered output.
#[allow(clippy::too_many_lines)]
async fn seed_record(world: &World) -> Uuid {
    world
        .file(
            Some("engagement"),
            "engagement.pdf",
            b"engagement v1",
            visibility::CLIENT,
        )
        .await;
    world
        .file(
            Some("engagement"),
            "engagement.pdf",
            b"engagement v2",
            visibility::CLIENT,
        )
        .await;
    world
        .file(None, "memo.pdf", b"internal memo", visibility::INTERNAL)
        .await;
    let deleted = world
        .file(
            Some("draft"),
            "draft.pdf",
            b"withdrawn draft",
            visibility::INTERNAL,
        )
        .await;
    assets::soft_delete(
        &world.surreal,
        world.project.id,
        deleted,
        world.owner.person_id(),
        None,
        at(10),
    )
    .await
    .unwrap();
    let held = world
        .file(
            Some("evidence"),
            "evidence.pdf",
            b"held evidence",
            visibility::INTERNAL,
        )
        .await;
    assets::set_legal_hold(&world.surreal, world.project.id, held, true, at(11))
        .await
        .unwrap();
    let purged = world
        .file(
            Some("expired"),
            "expired.pdf",
            b"expired bytes",
            visibility::INTERNAL,
        )
        .await;
    assets::soft_delete(
        &world.surreal,
        world.project.id,
        purged,
        world.owner.person_id(),
        None,
        at(12),
    )
    .await
    .unwrap();
    assets::mark_purged(&world.surreal, &[purged], at(13))
        .await
        .unwrap();
    world
        .file(
            Some("copy-a"),
            "copy-a.pdf",
            b"shared exhibit",
            visibility::CLIENT,
        )
        .await;
    world
        .file(
            Some("copy-b"),
            "copy-b.pdf",
            b"shared exhibit",
            visibility::INTERNAL,
        )
        .await;
    let template = store::test_support::seed_unpinned_project_template(
        &world.surreal,
        world.project.id,
        "synthetic-letter",
        "Synthetic Letter",
        "person",
        None,
    )
    .await;
    let notation = store::notations::create(
        &world.surreal,
        &store::notations::NewNotation::new(
            template.id,
            world.owner.person_id(),
            world.project.id,
            "BEGIN",
        ),
    )
    .await
    .unwrap();
    world
        .documents
        .put(
            &store::notations::document_pdf_storage_key(notation.id),
            b"rendered notation pdf",
            "application/pdf",
        )
        .await
        .unwrap();
    notation.id
}

/// Copy the Firm and client Entity rows into an empty target under their
/// original ids: the only prerequisites a restore expects.
async fn prepare_target(world: &World) -> (SurrealDb, Arc<dyn StorageService>, tempfile::TempDir) {
    let target = mem_surreal().await;
    let firm = store::firms::find_by_id(&world.surreal, world.firm_id)
        .await
        .unwrap()
        .unwrap();
    store::test_support::seed_restore_prerequisites(&target, &firm, world.project.entity_id).await;
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    (target, documents, dir)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_committed_generation_restores_the_complete_project_in_isolation() {
    let Some(world) = world().await else {
        return;
    };
    let notation_id = seed_record(&world).await;
    let requested = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "close-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    assert_eq!(requested.state, GenerationState::Pending);
    let generation = archives::run(&world.stores(), &world.owner, requested.id, at(101))
        .await
        .unwrap();
    assert_eq!(generation.state, GenerationState::Committed);
    assert_eq!(generation.requested_at, at(100));
    assert_eq!(generation.cutoff_at, Some(at(101)));

    let items = archives::items(&world.pg, generation.id).await.unwrap();
    // Seven live or soft-deleted revisions, one output, one restore metadata;
    // the purged revision keeps only its row.
    assert_eq!(items.len(), 9);
    assert!(items.iter().all(|item| item.verified_at.is_some()));
    let objects = world
        .archive_store
        .keys(&archives::project_prefix(world.firm_id, world.project.id));
    // Shared bytes are one object; the manifest is one more.
    let distinct: std::collections::BTreeSet<_> =
        items.iter().map(|item| item.object_key.clone()).collect();
    assert_eq!(objects.len(), distinct.len() + 1);

    let report = archives::verify(&world.pg, &world.archive, None, &world.owner, generation.id)
        .await
        .unwrap();
    assert_eq!(report.verified, 9);
    assert!(report.failures.is_empty());

    let (target, target_documents, _dir) = prepare_target(&world).await;
    let restore_target = RestoreTarget {
        surreal: &target,
        documents: &target_documents,
    };
    let restored = archives::restore(
        &world.pg,
        &world.archive,
        &world.owner,
        generation.id,
        &restore_target,
    )
    .await
    .unwrap();
    assert!(restored.rows_created > 0);
    assert_eq!(restored.rows_identical, 0);

    assert_eq!(
        projects::find_by_id(&target, world.project.id)
            .await
            .unwrap(),
        Some(world.project.clone())
    );
    let source_assets = assets::for_project_including_deleted(&world.surreal, world.project.id)
        .await
        .unwrap();
    let target_assets = assets::for_project_including_deleted(&target, world.project.id)
        .await
        .unwrap();
    assert_eq!(source_assets, target_assets);
    for asset in source_assets
        .iter()
        .filter(|asset| asset.purged_at.is_none())
    {
        assert_eq!(
            target_documents
                .get(&asset.storage_key)
                .await
                .unwrap()
                .bytes,
            world.documents.get(&asset.storage_key).await.unwrap().bytes,
        );
    }
    let purged = source_assets
        .iter()
        .find(|asset| asset.purged_at.is_some())
        .unwrap();
    assert!(target_documents.get(&purged.storage_key).await.is_err());
    // Visibility is unchanged: the client lens sees exactly what it saw.
    let client = |rows: &[assets::Asset]| {
        rows.iter()
            .filter(|asset| asset.visibility == visibility::CLIENT && asset.deleted_at.is_none())
            .map(|asset| asset.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(client(&source_assets), client(&target_assets));
    assert_eq!(
        store::notations::find_by_id(&target, notation_id)
            .await
            .unwrap(),
        store::notations::find_by_id(&world.surreal, notation_id)
            .await
            .unwrap(),
    );
    assert_eq!(
        target_documents
            .get(&store::notations::document_pdf_storage_key(notation_id))
            .await
            .unwrap()
            .bytes,
        b"rendered notation pdf"
    );

    // A rerun converges without writing.
    let again = archives::restore(
        &world.pg,
        &world.archive,
        &world.owner,
        generation.id,
        &restore_target,
    )
    .await
    .unwrap();
    assert_eq!(again.rows_created, 0);
    assert_eq!(again.objects_written, 0);
    // So does a rerun of the archive itself.
    let rerun = archives::run(&world.stores(), &world.owner, generation.id, at(500))
        .await
        .unwrap();
    assert_eq!(rerun, generation);
}

/// A Notation's imported Markdown-source chain (its Word original, a root
/// version, and an edit on top) travels with the archive and comes back under
/// its original ids; a rerun leaves it alone and a diverged row is refused.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_notation_source_chain_restores_under_its_original_ids() {
    use store::notation_documents::{self as sources, AppendEdit, ImportRoot, Saved};
    let Some(world) = world().await else {
        return;
    };
    let template = store::test_support::seed_unpinned_project_template(
        &world.surreal,
        world.project.id,
        "synthetic-source-letter",
        "Synthetic Source Letter",
        "person",
        None,
    )
    .await;
    let notation = store::notations::create(
        &world.surreal,
        &store::notations::NewNotation::new(
            template.id,
            world.owner.person_id(),
            world.project.id,
            "BEGIN",
        ),
    )
    .await
    .unwrap();
    let original = assets::ingest_content(
        &world.surreal,
        &world.documents,
        b"synthetic word package",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .await
    .unwrap();
    let Saved::Written(root) = sources::import_root(
        &world.surreal,
        &world.documents,
        ImportRoot {
            project_id: world.project.id,
            notation_id: notation.id,
            original_asset_id: original,
            markdown: b"# Letter\n\nDear client,\n",
            anchor_manifest: b"{}",
            parser_version: "test-1",
            schema_version: 1,
            authored_by_person_id: world.owner.person_id(),
            recorded_at: "2026-10-18T00:00:00Z",
        },
    )
    .await
    .unwrap() else {
        panic!("a fresh import writes a root version");
    };
    sources::append_edit(
        &world.surreal,
        &world.documents,
        AppendEdit {
            project_id: world.project.id,
            notation_id: notation.id,
            expected_parent_version_id: root.id,
            markdown: b"# Letter\n\nDear synthetic client,\n",
            anchor_manifest: b"{}",
            parser_version: "test-1",
            schema_version: 1,
            authored_by_person_id: world.owner.person_id(),
            recorded_at: "2026-10-18T00:01:00Z",
        },
    )
    .await
    .unwrap();
    let document = sources::document_for_notation(&world.surreal, notation.id)
        .await
        .unwrap()
        .unwrap();
    let chain = sources::versions_of(&world.surreal, document.id)
        .await
        .unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].id, root.id);

    let requested = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "sources-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    // The request is the durable queue the unattended schedule resumes.
    assert!(archives::unfinished(&world.pg, 25)
        .await
        .unwrap()
        .iter()
        .any(|pending| pending.id == requested.id));
    let generation = archives::resume_unattended(&world.stores(), requested.id, at(101))
        .await
        .unwrap();
    assert_eq!(generation.state, GenerationState::Committed);
    assert!(!archives::unfinished(&world.pg, 25)
        .await
        .unwrap()
        .iter()
        .any(|pending| pending.id == requested.id));
    archives::probe(&world.archive).await.unwrap();
    let items = archives::items(&world.pg, generation.id).await.unwrap();
    assert!(
        archives::digest_is_archived(&world.pg, world.project.id, &items[0].sha256)
            .await
            .unwrap()
    );
    assert!(
        !archives::digest_is_archived(&world.pg, world.project.id, &"0".repeat(64))
            .await
            .unwrap()
    );
    for asset_id in [
        original,
        chain[0].markdown_asset_id,
        chain[1].markdown_asset_id,
    ] {
        assert!(
            items.iter().any(|item| item.asset_id == Some(asset_id)),
            "source asset {asset_id} is archived"
        );
    }

    let (target, target_documents, _dir) = prepare_target(&world).await;
    let restore_target = RestoreTarget {
        surreal: &target,
        documents: &target_documents,
    };
    let restored = archives::restore(
        &world.pg,
        &world.archive,
        &world.owner,
        generation.id,
        &restore_target,
    )
    .await
    .unwrap();
    assert!(restored.rows_created > 0);
    assert_eq!(
        sources::document_for_notation(&target, notation.id)
            .await
            .unwrap(),
        Some(document.clone())
    );
    assert_eq!(
        sources::versions_of(&target, document.id).await.unwrap(),
        chain
    );
    for version in &chain {
        let source = world
            .documents
            .get(
                &assets::find_by_id(&world.surreal, version.markdown_asset_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .storage_key,
            )
            .await
            .unwrap();
        let restored_asset = assets::find_by_id(&target, version.markdown_asset_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            target_documents
                .get(&restored_asset.storage_key)
                .await
                .unwrap()
                .bytes,
            source.bytes
        );
    }

    // A rerun finds every row identical.
    let again = archives::restore(
        &world.pg,
        &world.archive,
        &world.owner,
        generation.id,
        &restore_target,
    )
    .await
    .unwrap();
    assert_eq!(again.rows_created, 0);
    assert!(again.rows_identical > 0);
    assert_eq!(
        sources::restore_archived_document(&target, &document)
            .await
            .unwrap(),
        assets::RowRestore::Identical
    );
    assert_eq!(
        sources::restore_archived_version(&target, &chain[1])
            .await
            .unwrap(),
        assets::RowRestore::Identical
    );
    // A row that diverged under the same id is a conflict, never overwritten.
    let diverged_document = sources::NotationDocument {
        current_version_id: None,
        ..document.clone()
    };
    assert_eq!(
        sources::restore_archived_document(&target, &diverged_document)
            .await
            .unwrap(),
        assets::RowRestore::Conflict
    );
    let diverged_version = sources::NotationDocumentVersion {
        status: "diverged".into(),
        ..chain[1].clone()
    };
    assert_eq!(
        sources::restore_archived_version(&target, &diverged_version)
            .await
            .unwrap(),
        assets::RowRestore::Conflict
    );
}

#[tokio::test]
async fn interrupted_copies_unknown_outcomes_and_lost_checkpoints_converge() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "retry-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    world
        .archive_store
        .fail_after_write
        .store(2, Ordering::SeqCst);
    world
        .archive_store
        .fail_before_write
        .store(1, Ordering::SeqCst);
    // The provider reports failure after storing: read-back settles it, and
    // the injected pre-write failure stops this attempt.
    let error = archives::run(&world.stores(), &world.owner, generation.id, at(101))
        .await
        .unwrap_err();
    assert!(matches!(error, ArchiveError::ArchiveMissing(_)), "{error}");
    let stalled = archives::find(&world.pg, &world.owner, generation.id)
        .await
        .unwrap();
    assert_eq!(stalled.state, GenerationState::Copying);
    assert!(stalled.last_error.is_some());
    assert_eq!(stalled.cutoff_at, Some(at(101)));

    // Lose every checkpoint, as a crash between copy and checkpoint would.
    sqlx::query("UPDATE project_archive_item SET verified_at = NULL WHERE generation_id = $1")
        .bind(generation.id)
        .execute(world.pg.pool())
        .await
        .unwrap();
    let committed = archives::run(&world.stores(), &world.owner, generation.id, at(102))
        .await
        .unwrap();
    assert_eq!(committed.state, GenerationState::Committed);
    // Retries neither moved the cutoff nor reset the request time.
    assert_eq!(committed.cutoff_at, Some(at(101)));
    assert_eq!(committed.requested_at, at(100));
    assert!(committed.attempts >= 2);
    assert!(committed.last_error.is_none());
    let items = archives::items(&world.pg, generation.id).await.unwrap();
    let distinct: std::collections::BTreeSet<_> =
        items.iter().map(|item| item.object_key.clone()).collect();
    assert_eq!(
        world
            .archive_store
            .keys(&archives::project_prefix(world.firm_id, world.project.id))
            .len(),
        distinct.len() + 1,
        "retries leave no duplicate objects"
    );

    // A committed generation is immutable in the database.
    let refused =
        sqlx::query("UPDATE project_archive_item SET verified_at = NULL WHERE generation_id = $1")
            .bind(generation.id)
            .execute(world.pg.pool())
            .await;
    assert!(refused.is_err());
    let refused = sqlx::query("DELETE FROM project_archive_generation WHERE id = $1")
        .bind(generation.id)
        .execute(world.pg.pool())
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_project_changed_during_copy_is_replanned_before_it_commits() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "moving-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    world
        .archive_store
        .fail_before_write
        .store(1, Ordering::SeqCst);
    archives::run(&world.stores(), &world.owner, generation.id, at(101))
        .await
        .unwrap_err();
    let before = archives::items(&world.pg, generation.id).await.unwrap();
    // A new revision lands while the archive is incomplete.
    let added = world
        .file(
            Some("engagement"),
            "engagement.pdf",
            b"engagement v3",
            visibility::CLIENT,
        )
        .await;
    let committed = archives::run(&world.stores(), &world.owner, generation.id, at(102))
        .await
        .unwrap();
    assert_eq!(committed.state, GenerationState::Committed);
    assert_eq!(
        committed.cutoff_at,
        Some(at(102)),
        "a changed inventory has a new cutoff"
    );
    let after = archives::items(&world.pg, generation.id).await.unwrap();
    assert_eq!(after.len(), before.len() + 1);
    assert!(after.iter().any(|item| item.asset_id == Some(added)));
}

#[tokio::test]
async fn missing_or_corrupt_objects_never_commit_or_restore() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    let memo = assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|asset| asset.filename.as_deref() == Some("memo.pdf"))
        .unwrap();
    let saved = world.documents.get(&memo.storage_key).await.unwrap().bytes;
    world.documents.delete(&memo.storage_key).await.unwrap();
    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "missing-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    let error = archives::run(&world.stores(), &world.owner, generation.id, at(101))
        .await
        .unwrap_err();
    assert!(matches!(error, ArchiveError::SourceMissing(_)), "{error}");
    world
        .documents
        .put(&memo.storage_key, b"tampered", "application/pdf")
        .await
        .unwrap();
    let error = archives::run(&world.stores(), &world.owner, generation.id, at(102))
        .await
        .unwrap_err();
    assert!(matches!(error, ArchiveError::SourceCorrupt(_)), "{error}");
    assert_ne!(
        archives::find(&world.pg, &world.owner, generation.id)
            .await
            .unwrap()
            .state,
        GenerationState::Committed
    );
    world
        .documents
        .put(&memo.storage_key, &saved, "application/pdf")
        .await
        .unwrap();
    let committed = archives::run(&world.stores(), &world.owner, generation.id, at(103))
        .await
        .unwrap();
    assert_eq!(committed.state, GenerationState::Committed);

    let damaged = archives::object_key(world.firm_id, world.project.id, &memo.sha256_hex);
    world.archive_store.corrupt(&damaged);
    let report = archives::verify(&world.pg, &world.archive, None, &world.owner, committed.id)
        .await
        .unwrap();
    assert_eq!(report.failures, vec![format!("documents/{}", memo.id)]);
    let (target, target_documents, _dir) = prepare_target(&world).await;
    let error = archives::restore(
        &world.pg,
        &world.archive,
        &world.owner,
        committed.id,
        &RestoreTarget {
            surreal: &target,
            documents: &target_documents,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ArchiveError::ArchiveCorrupt(_)), "{error}");
    assert!(
        projects::find_by_id(&target, world.project.id)
            .await
            .unwrap()
            .is_none(),
        "nothing is written before every object verifies"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn another_firm_can_neither_request_read_nor_restore() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "firm-a",
        None,
        at(100),
    )
    .await
    .unwrap();
    archives::run(&world.stores(), &world.owner, generation.id, at(101))
        .await
        .unwrap();

    let admin = persons::create(
        &world.surreal,
        &NewPerson::with_role("Other Admin", "other-admin@example.com", Role::Admin),
    )
    .await
    .unwrap();
    // The new Firm's Admin DRI holds an Admin membership there and nowhere else.
    let other_firm = store::firms::create(
        &world.surreal,
        &store::firms::NewFirm {
            name: "Other Synthetic Firm".into(),
            status: "active".into(),
            entity_id: seed_entity(&world.surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap()
    .id;
    assert!(matches!(
        archives::authorize(&world.surreal, Role::Admin, admin.id, world.firm_id).await,
        Err(ArchiveError::Forbidden)
    ));
    let lawyer = persons::create(
        &world.surreal,
        &NewPerson::with_role("Firm Lawyer", "firm-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    assert!(matches!(
        archives::authorize(&world.surreal, Role::Lawyer, lawyer.id, world.firm_id).await,
        Err(ArchiveError::Forbidden)
    ));
    let foreign = archives::authorize(&world.surreal, Role::Admin, admin.id, other_firm)
        .await
        .unwrap();
    assert!(matches!(
        archives::request(
            &world.stores(),
            &foreign,
            world.project.id,
            "firm-b",
            None,
            at(102)
        )
        .await,
        Err(ArchiveError::ProjectNotFound)
    ));
    assert!(matches!(
        archives::find(&world.pg, &foreign, generation.id).await,
        Err(ArchiveError::GenerationNotFound)
    ));
    assert!(matches!(
        archives::run(&world.stores(), &foreign, generation.id, at(103)).await,
        Err(ArchiveError::GenerationNotFound)
    ));
    assert!(archives::for_project(&world.pg, &foreign, world.project.id)
        .await
        .unwrap()
        .is_empty());
    let (target, target_documents, _dir) = prepare_target(&world).await;
    assert!(matches!(
        archives::restore(
            &world.pg,
            &world.archive,
            &foreign,
            generation.id,
            &RestoreTarget {
                surreal: &target,
                documents: &target_documents,
            },
        )
        .await,
        Err(ArchiveError::GenerationNotFound)
    ));
    // The same operation key cannot be pointed at another Project.
    let second = projects::create(
        &world.surreal,
        &NewProject {
            code: format!(
                "archive-other-{}",
                &Uuid::now_v7().simple().to_string()[..8]
            ),
            name: "Second Synthetic Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&world.surreal).await,
            firm_id: Some(world.firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        archives::request(
            &world.stores(),
            &world.owner,
            second.id,
            "firm-a",
            None,
            at(104)
        )
        .await,
        Err(ArchiveError::OperationKeyConflict)
    ));
}

#[tokio::test]
async fn concurrent_runners_converge_on_one_committed_generation() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "race-1",
        None,
        at(100),
    )
    .await
    .unwrap();
    let stores = world.stores();
    let (left, right) = tokio::join!(
        archives::run(&stores, &world.owner, generation.id, at(101)),
        archives::run(&stores, &world.owner, generation.id, at(101)),
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.state, GenerationState::Committed);
    assert_eq!(left.manifest_key, right.manifest_key);
    assert_eq!(left.manifest_sha256, right.manifest_sha256);
    let manifests = world.archive_store.keys(&format!(
        "{}generations/{}/",
        archives::project_prefix(world.firm_id, world.project.id),
        generation.id
    ));
    assert_eq!(manifests.len(), 1);
}

#[tokio::test]
async fn a_repository_project_needs_a_verified_source_backup() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    projects::set_repository_url(
        &world.surreal,
        world.project.id,
        Some("https://forge.example.test/synthetic/archive-matter"),
    )
    .await
    .unwrap();
    assert!(matches!(
        archives::request(
            &world.stores(),
            &world.owner,
            world.project.id,
            "source-0",
            None,
            at(100)
        )
        .await,
        Err(ArchiveError::SourceBackupRequired)
    ));
    let bundles = Arc::new(MemoryStorage::default());
    let bundle = b"synthetic git bundle bytes";
    let created = bundles
        .put_immutable(
            "repositories/archive-matter.bundle",
            bundle,
            "application/x-git-bundle",
        )
        .await
        .unwrap();
    let receipt = |sha256: String| SourceBackupReference {
        format: 1,
        object: "repositories/archive-matter.bundle".into(),
        generation: created.generation,
        size_bytes: created.size_bytes,
        sha256,
        head: "refs/heads/main".into(),
        refs: format!("refs/heads/main {}", "a".repeat(40)),
    };
    let digest = store::assets::sha256_hex(bundle);
    let bundle_storage: Arc<dyn StorageService> = bundles.clone();
    let stores = ArchiveStores {
        source_backups: Some(&bundle_storage),
        ..world.stores()
    };
    let wrong = archives::request(
        &stores,
        &world.owner,
        world.project.id,
        "source-1",
        Some(receipt("b".repeat(64))),
        at(101),
    )
    .await
    .unwrap();
    assert!(matches!(
        archives::run(&stores, &world.owner, wrong.id, at(102)).await,
        Err(ArchiveError::SourceCorrupt(_))
    ));
    let generation = archives::request(
        &stores,
        &world.owner,
        world.project.id,
        "source-2",
        Some(receipt(digest)),
        at(103),
    )
    .await
    .unwrap();
    let committed = archives::run(&stores, &world.owner, generation.id, at(104))
        .await
        .unwrap();
    assert_eq!(committed.state, GenerationState::Committed);
    assert!(archives::items(&world.pg, committed.id)
        .await
        .unwrap()
        .iter()
        .any(|item| item.kind == archives::kind::SOURCE_BACKUP));
    let report = archives::verify(
        &world.pg,
        &world.archive,
        Some(&bundle_storage),
        &world.owner,
        committed.id,
    )
    .await
    .unwrap();
    assert!(report.source_backup_verified);
    // The bundle object is never copied into the archived-projects lane.
    assert!(world.archive_store.keys("repositories/").is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn requests_refuse_bad_keys_unknown_projects_and_malformed_receipts() {
    let Some(world) = world().await else {
        return;
    };
    for key in ["", "has space", &"k".repeat(400)] {
        assert!(
            matches!(
                archives::request(
                    &world.stores(),
                    &world.owner,
                    world.project.id,
                    key,
                    None,
                    at(1)
                )
                .await,
                Err(ArchiveError::InvalidOperationKey)
            ),
            "{key:?}"
        );
    }
    assert!(matches!(
        archives::request(
            &world.stores(),
            &world.owner,
            Uuid::now_v7(),
            "unknown-project",
            None,
            at(2)
        )
        .await,
        Err(ArchiveError::ProjectNotFound)
    ));
    let good = SourceBackupReference {
        format: 1,
        object: "repositories/archive-matter.bundle".into(),
        generation: 7,
        size_bytes: 42,
        sha256: "c".repeat(64),
        head: "refs/heads/main".into(),
        refs: format!("refs/heads/main {}", "a".repeat(40)),
    };
    let malformed = [
        SourceBackupReference {
            format: 2,
            ..good.clone()
        },
        SourceBackupReference {
            object: " ".into(),
            ..good.clone()
        },
        SourceBackupReference {
            generation: 0,
            ..good.clone()
        },
        SourceBackupReference {
            sha256: "not-a-digest".into(),
            ..good.clone()
        },
        SourceBackupReference {
            head: "main".into(),
            ..good.clone()
        },
        SourceBackupReference {
            refs: "  ".into(),
            ..good.clone()
        },
    ];
    for (index, receipt) in malformed.into_iter().enumerate() {
        assert!(
            matches!(
                archives::request(
                    &world.stores(),
                    &world.owner,
                    world.project.id,
                    &format!("bad-receipt-{index}"),
                    Some(receipt),
                    at(3)
                )
                .await,
                Err(ArchiveError::InvalidSourceBackup(_))
            ),
            "receipt {index}"
        );
    }
    // Nothing was recorded for any refusal.
    assert!(archives::unfinished(&world.pg, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn unattended_runs_verification_failures_and_restore_refusals() {
    let Some(world) = world().await else {
        return;
    };
    seed_record(&world).await;
    archives::probe(&world.archive).await.unwrap();
    assert_eq!(world.archive_store.keys("health/").len(), 1);

    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "unattended-1",
        None,
        at(10),
    )
    .await
    .unwrap();
    let unfinished = archives::unfinished(&world.pg, 10).await.unwrap();
    assert_eq!(
        unfinished.iter().map(|g| g.id).collect::<Vec<_>>(),
        vec![generation.id]
    );
    // An uncommitted generation can be neither verified nor restored.
    assert!(matches!(
        archives::verify(&world.pg, &world.archive, None, &world.owner, generation.id).await,
        Err(ArchiveError::NotCommitted)
    ));
    let committed = archives::resume_unattended(&world.stores(), generation.id, at(11))
        .await
        .unwrap();
    assert_eq!(committed.state, GenerationState::Committed);
    assert!(archives::unfinished(&world.pg, 10)
        .await
        .unwrap()
        .is_empty());
    // Resuming a committed generation is a no-op that returns it unchanged.
    let again = archives::resume_unattended(&world.stores(), generation.id, at(12))
        .await
        .unwrap();
    assert_eq!(again, committed);

    let items = archives::items(&world.pg, committed.id).await.unwrap();
    let document = items
        .iter()
        .find(|item| item.kind != archives::kind::SOURCE_BACKUP)
        .unwrap();
    assert!(
        archives::digest_is_archived(&world.pg, world.project.id, &document.sha256)
            .await
            .unwrap()
    );
    assert!(
        !archives::digest_is_archived(&world.pg, world.project.id, &"0".repeat(64))
            .await
            .unwrap()
    );

    // A target without the Project's Firm and client Entity is refused.
    let empty = mem_surreal().await;
    let dir = tempfile::tempdir().unwrap();
    let empty_documents: Arc<dyn StorageService> =
        Arc::new(FsStorage::new(dir.path()).await.unwrap());
    assert!(matches!(
        archives::restore(
            &world.pg,
            &world.archive,
            &world.owner,
            committed.id,
            &RestoreTarget {
                surreal: &empty,
                documents: &empty_documents,
            },
        )
        .await,
        Err(ArchiveError::RestorePrerequisite(_))
    ));

    // A different object already at an original key stops the restore.
    let (target, target_documents, _target_dir) = prepare_target(&world).await;
    let asset = assets::for_project_including_deleted(&world.surreal, world.project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|asset| asset.purged_at.is_none())
        .unwrap();
    target_documents
        .put(&asset.storage_key, b"different bytes", "application/pdf")
        .await
        .unwrap();
    assert!(matches!(
        archives::restore(
            &world.pg,
            &world.archive,
            &world.owner,
            committed.id,
            &RestoreTarget {
                surreal: &target,
                documents: &target_documents,
            },
        )
        .await,
        Err(ArchiveError::RestoreConflict(_))
    ));

    // A damaged or missing manifest fails verification.
    let manifest_key = committed.manifest_key.clone().unwrap();
    world.archive_store.corrupt(&manifest_key);
    assert!(matches!(
        archives::verify(&world.pg, &world.archive, None, &world.owner, committed.id).await,
        Err(ArchiveError::ArchiveCorrupt(_))
    ));
    world.archive_store.remove(&manifest_key);
    assert!(matches!(
        archives::verify(&world.pg, &world.archive, None, &world.owner, committed.id).await,
        Err(ArchiveError::ArchiveMissing(_))
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn coverage_enumerates_every_project_and_flags_missing_and_stale_generations() {
    use archives::coverage::{self, Coverage, BASELINE};
    let Some(world) = world().await else {
        return;
    };
    let new_project = |code: &'static str| {
        let surreal = world.surreal.clone();
        let firm_id = world.firm_id;
        async move {
            projects::create(
                &surreal,
                &NewProject {
                    code: format!("{code}-{}", &Uuid::now_v7().simple().to_string()[..8]),
                    name: "Synthetic Coverage Matter".into(),
                    status: "open".into(),
                    entity_id: seed_entity(&surreal).await,
                    firm_id: Some(firm_id),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        }
    };
    let unarchived = new_project("coverage-unarchived").await;
    let repository = new_project("coverage-repository").await;
    projects::set_repository_url(
        &world.surreal,
        repository.id,
        Some("https://forge.example.com/synthetic/coverage.git"),
    )
    .await
    .unwrap();

    let entry = |report: &coverage::CoverageReport, id: Uuid| {
        report
            .entries
            .iter()
            .find(|entry| entry.project_id == id)
            .cloned()
            .expect("every Project in the inventory is expected")
    };
    let empty = coverage::report(&world.surreal, &world.pg, at(0))
        .await
        .unwrap();
    assert_eq!(empty.entries.len(), 3);
    for id in [world.project.id, unarchived.id, repository.id] {
        assert_eq!(entry(&empty, id).coverage, Coverage::Missing);
    }
    assert!(entry(&empty, repository.id).has_repository);

    let generation = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "coverage-1",
        None,
        at(1),
    )
    .await
    .unwrap();
    let requested = coverage::report(&world.surreal, &world.pg, at(1))
        .await
        .unwrap();
    assert_eq!(
        entry(&requested, world.project.id).coverage,
        Coverage::Missing
    );
    assert!(entry(&requested, world.project.id).pending);

    let committed = archives::run(&world.stores(), &world.owner, generation.id, at(2))
        .await
        .unwrap();
    let cutoff = committed
        .cutoff_at
        .expect("a committed generation has a cutoff");
    // A retried request under the same key joins the committed generation.
    let retried = archives::request(
        &world.stores(),
        &world.owner,
        world.project.id,
        "coverage-1",
        None,
        at(3),
    )
    .await
    .unwrap();
    assert_eq!(retried.id, committed.id);

    let fresh = coverage::report(&world.surreal, &world.pg, cutoff + BASELINE)
        .await
        .unwrap();
    let covered = entry(&fresh, world.project.id);
    assert_eq!(covered.coverage, Coverage::Current);
    assert_eq!(covered.generation_id, Some(committed.id));
    assert_eq!(covered.cutoff_at, Some(cutoff));
    assert!(!covered.pending);
    assert_eq!(entry(&fresh, unarchived.id).coverage, Coverage::Missing);
    let counts = fresh.counts();
    assert_eq!((counts.expected, counts.current, counts.missing), (3, 1, 2));

    let stale = coverage::report(
        &world.surreal,
        &world.pg,
        cutoff + BASELINE + chrono::TimeDelta::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(entry(&stale, world.project.id).coverage, Coverage::Stale);
    let summary = stale.summary();
    assert_eq!(summary.uncovered.len(), 3);
    assert!(
        !serde_json::to_string(&summary)
            .unwrap()
            .contains("forge.example.com"),
        "the summary names ids, never repository coordinates"
    );
}
