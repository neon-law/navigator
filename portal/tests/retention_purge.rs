//! The governed retention purge: Firm-scoped authority, Legal Holds, the
//! deadline boundary, shared objects, crash recovery, and archived copies.
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use cloud::{FsStorage, StorageService};
use portal::expunge::purge::{
    due_chains, purge_expired_documents, purge_expired_documents_with, purge_one, DueChain,
    PurgeAuthority, RetentionLanes, ScheduledPurge,
};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::assets::{self, Filed, RETENTION_DAYS};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::projects::{self, ClosureReason, NewProject, Project};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity, MemoryStorage};
use uuid::Uuid;
use workflows::dbos_engine::retention_purge::RetentionSource;

struct World {
    surreal: SurrealDb,
    storage: Arc<dyn StorageService>,
    firm_id: Uuid,
    admin: Uuid,
    project: Project,
    _dir: tempfile::TempDir,
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

async fn world() -> World {
    let surreal = mem_surreal().await;
    let admin = persons::create(
        &surreal,
        &NewPerson::with_role("Purge Admin", "purge-admin@example.com", Role::Admin),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Purge Synthetic Firm".into(),
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
            code: format!("purge-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Purge Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    World {
        surreal,
        storage,
        firm_id: firm.id,
        admin: admin.id,
        project,
        _dir: dir,
    }
}

impl World {
    async fn file(&self, slug: &str, bytes: &[u8]) -> Uuid {
        self.file_kind(slug, "unclassified", bytes).await
    }

    async fn file_kind(&self, slug: &str, kind: &str, bytes: &[u8]) -> Uuid {
        let args = IngestArgs {
            project_id: self.project.id,
            source: "upload",
            filename: "doc.pdf",
            kind,
            content_type: "application/pdf",
            description: None,
            visibility: visibility::INTERNAL,
            secondary_storage_key: None,
        };
        let identity = DocumentIdentity {
            slug: Some(slug),
            ..DocumentIdentity::default()
        };
        match assets::file_revision(&self.surreal, &self.storage, &args, &identity, bytes)
            .await
            .unwrap()
        {
            Filed::Revision(document) => document.asset_id,
            Filed::Unchanged { asset_id } => asset_id,
        }
    }

    async fn delete(&self, id: Uuid, when: DateTime<Utc>) {
        assets::soft_delete(&self.surreal, self.project.id, id, self.admin, None, when)
            .await
            .unwrap();
    }

    fn by_firm(&self) -> PurgeAuthority {
        PurgeAuthority::ByFirm(BTreeMap::from([(self.firm_id, self.admin)]))
    }

    fn scheduled(&self, pg: Option<PostgresDb>, authority: PurgeAuthority) -> ScheduledPurge {
        ScheduledPurge {
            surreal: self.surreal.clone(),
            storage: Arc::clone(&self.storage),
            pg,
            archive: None,
            authority,
        }
    }

    /// Stamp the matter closed now, as the lifecycle transition would; the
    /// closing ceremony itself is not what this suite is about.
    async fn close(&self) {
        self.surreal
            .query("UPDATE $id SET status = 'closed', closed_at = $at, closure_reason = $reason")
            .bind(("id", store::surreal::record_id("project", self.project.id)))
            .bind(("at", Utc::now().to_rfc3339()))
            .bind(("reason", ClosureReason::EngagementCompleted.as_str()))
            .await
            .unwrap()
            .check()
            .unwrap();
        let project = projects::find_by_id(&self.surreal, self.project.id)
            .await
            .unwrap()
            .unwrap();
        assert!(project.closed_at.is_some());
    }
}

fn window_closes(deleted: DateTime<Utc>) -> DateTime<Utc> {
    deleted + Duration::days(RETENTION_DAYS)
}

#[tokio::test]
async fn authority_holds_and_the_deadline_decide_what_is_purged() {
    let world = world().await;
    let id = world.file("filing", b"filing bytes").await;
    world.delete(id, at(0)).await;
    let lanes = RetentionLanes::default();

    // One second early: nothing is due.
    let early = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &world.by_firm(),
        window_closes(at(0)) - Duration::seconds(1),
    )
    .await
    .unwrap();
    assert!(early.purged.is_empty());

    // No authority for this Firm: reported, untouched.
    let missing = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &PurgeAuthority::ByFirm(BTreeMap::new()),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(missing.unauthorized, 1);
    // A configured person without the Firm's capability is no authority.
    let lawyer = persons::create(
        &world.surreal,
        &NewPerson::with_role("Not Admin", "not-admin@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let wrong = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &PurgeAuthority::ByFirm(BTreeMap::from([(world.firm_id, lawyer.id)])),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(wrong.unauthorized, 1);

    // A hold placed before the run protects the chain.
    assets::set_legal_hold(&world.surreal, world.project.id, id, true, at(10))
        .await
        .unwrap();
    let held = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &world.by_firm(),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(held.held, 1);
    let row = assets::find_by_id(&world.surreal, id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.purged_at.is_none());
    assert!(world.storage.get(&row.storage_key).await.is_ok());

    // Released, exactly at the deadline, with authority: purged.
    assets::set_legal_hold(&world.surreal, world.project.id, id, false, at(11))
        .await
        .unwrap();
    let purged = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &world.by_firm(),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(purged.purged.len(), 1);
    let row = assets::find_by_id(&world.surreal, id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.purged_at.is_some());
    assert!(world.storage.get(&row.storage_key).await.is_err());
}

#[tokio::test]
async fn a_purge_keeps_bytes_a_live_sibling_chain_shares() {
    let world = world().await;
    let doomed = world.file("draft", b"identical exhibit").await;
    let live = world.file("exhibit", b"identical exhibit").await;
    world.delete(doomed, at(0)).await;
    let report = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        RetentionLanes::default(),
        &PurgeAuthority::Admin(world.admin),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(report.purged.len(), 1);
    let live = assets::find_by_id(&world.surreal, live)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        world.storage.get(&live.storage_key).await.unwrap().bytes,
        b"identical exhibit"
    );
}

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
    let name = format!("purge_test_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_durable_purge_joins_its_run_and_removes_archived_copies_it_owns() {
    let Some(pg) = database().await else {
        return;
    };
    let world = world().await;
    let archive_store = Arc::new(MemoryStorage::default());
    let archive: Arc<dyn StorageService> = archive_store.clone();
    let doomed = world.file("old-brief", b"old brief").await;
    let shared = world.file("old-brief", b"shared annex").await;
    let live = world.file("annex", b"shared annex").await;
    world.delete(doomed, at(0)).await;

    // A committed recovery generation holds every byte, and the chain is
    // archived and its active copies cleaned up.
    let authority =
        store::project_archives::authorize(&world.surreal, Role::Admin, world.admin, world.firm_id)
            .await
            .unwrap();
    let stores = store::project_archives::ArchiveStores {
        surreal: &world.surreal,
        pg: &pg,
        documents: &world.storage,
        archive: &archive,
        source_backups: None,
    };
    let generation = store::project_archives::request(
        &stores,
        &authority,
        world.project.id,
        "pre-purge",
        None,
        at(1),
    )
    .await
    .unwrap();
    store::project_archives::run(&stores, &authority, generation.id, at(2))
        .await
        .unwrap();
    let retention_stores = store::document_retention::RetentionStores {
        surreal: &world.surreal,
        pg: &pg,
        documents: &world.storage,
        archive: &archive,
    };
    store::document_retention::reconcile_soft_deleted(&retention_stores, at(3), 100)
        .await
        .unwrap();

    // A crash after the expunge but before its run record: the rerun joins.
    let doomed_row = assets::find_by_id(&world.surreal, doomed)
        .await
        .unwrap()
        .unwrap();
    let purge_after = doomed_row.purge_after.unwrap();
    store::document_retention::start_purge_run(
        &pg,
        world.project.id,
        "old-brief",
        purge_after,
        world.firm_id,
        world.admin,
        at(4),
    )
    .await
    .unwrap();
    let prior = Uuid::now_v7();
    store::document_retention::record_purge_expunge(
        &pg,
        world.project.id,
        "old-brief",
        purge_after,
        prior,
    )
    .await
    .unwrap();
    let lanes = RetentionLanes {
        pg: Some(&pg),
        archive: Some(&archive),
    };
    let report = purge_expired_documents_with(
        &world.surreal,
        &world.storage,
        lanes,
        &world.by_firm(),
        window_closes(at(0)),
    )
    .await
    .unwrap();
    assert_eq!(report.purged.len(), 1);
    assert_eq!(
        report.purged[0].expunge_record_id, prior,
        "the recorded run is joined, not expunged again"
    );
    let audits = store::expunge_records::for_project(&world.surreal, world.project.id)
        .await
        .unwrap();
    assert!(audits.is_empty(), "no second audit row for a joined run");

    // The doomed revision's archived bytes are gone; the shared annex stays.
    let doomed_object = store::project_archives::object_key(
        world.firm_id,
        world.project.id,
        &doomed_row.sha256_hex,
    );
    assert!(archive_store.keys(&doomed_object).is_empty());
    let shared_row = assets::find_by_id(&world.surreal, shared)
        .await
        .unwrap()
        .unwrap();
    let shared_object = store::project_archives::object_key(
        world.firm_id,
        world.project.id,
        &shared_row.sha256_hex,
    );
    assert_eq!(archive_store.keys(&shared_object).len(), 1);
    let live_row = assets::find_by_id(&world.surreal, live)
        .await
        .unwrap()
        .unwrap();
    assert!(world.storage.get(&live_row.storage_key).await.is_ok());

    // The earlier generation accounts for the purge rather than failing.
    let verified = store::project_archives::verify(&pg, &archive, None, &authority, generation.id)
        .await
        .unwrap();
    assert!(verified.failures.is_empty(), "{:?}", verified.failures);
    assert_eq!(verified.purged, vec![format!("documents/{doomed}")]);
}

#[tokio::test]
async fn the_schedule_reports_one_outcome_per_chain() {
    let world = world().await;
    let doomed = world.file("doomed", b"doomed bytes").await;
    let held = world.file("held", b"held bytes").await;
    let _live = world.file("live", b"live bytes").await;
    world.delete(doomed, at(0)).await;
    world.delete(held, at(0)).await;
    assets::set_legal_hold(&world.surreal, world.project.id, held, true, at(5))
        .await
        .unwrap();
    let now = window_closes(at(0));

    let scheduled = world.scheduled(None, world.by_firm());
    let mut due = scheduled.due(now).await.unwrap();
    due.sort();
    assert_eq!(
        due,
        vec![
            (world.project.id, "doomed".to_string()),
            (world.project.id, "held".to_string()),
        ]
    );
    assert!(scheduled
        .due(now - Duration::seconds(1))
        .await
        .unwrap()
        .is_empty());

    // A Firm with no configured person is reported, never purged.
    let nobody = world.scheduled(None, PurgeAuthority::ByFirm(BTreeMap::new()));
    assert_eq!(
        nobody.purge(world.project.id, "doomed", now).await,
        "no Firm-scoped authority configured"
    );
    // A configured person who does not exist is no authority either.
    let ghost = world.scheduled(
        None,
        PurgeAuthority::ByFirm(BTreeMap::from([(world.firm_id, Uuid::now_v7())])),
    );
    assert_eq!(
        ghost.purge(world.project.id, "doomed", now).await,
        "no Firm-scoped authority configured"
    );

    assert_eq!(scheduled.purge(world.project.id, "held", now).await, "held");
    assert_eq!(
        scheduled.purge(world.project.id, "doomed", now).await,
        "purged"
    );
    // A purged chain, a live chain, and an unknown chain are no longer due.
    for chain in ["doomed", "live", "never-filed"] {
        assert_eq!(
            scheduled.purge(world.project.id, chain, now).await,
            "no longer eligible",
            "{chain}"
        );
    }
    let after = due_chains(&world.surreal, now).await.unwrap();
    assert_eq!(
        after,
        vec![DueChain {
            project_id: world.project.id,
            chain_key: "held".into()
        }]
    );
}

#[tokio::test]
async fn a_closed_project_waits_for_its_closed_repository_archive() {
    let world = world().await;
    let doomed = world.file("closing-memo", b"closing memo").await;
    world.delete(doomed, at(0)).await;
    world.close().await;
    let now = window_closes(at(0));

    let waiting = purge_expired_documents(&world.surreal, &world.storage, world.admin, now)
        .await
        .unwrap();
    assert_eq!(waiting.awaiting_archive, 1);
    assert!(waiting.purged.is_empty());
    assert_eq!(
        world
            .scheduled(None, world.by_firm())
            .purge(world.project.id, "closing-memo", now)
            .await,
        "awaiting closed-repository archive"
    );

    // The repository's closed_repository record releases the chain.
    world
        .file_kind("closed-repository", "closed_repository", b"bundle")
        .await;
    let chain = DueChain {
        project_id: world.project.id,
        chain_key: "closing-memo".into(),
    };
    let report = purge_one(
        &world.surreal,
        &world.storage,
        RetentionLanes::default(),
        &PurgeAuthority::Admin(world.admin),
        &chain,
        now,
    )
    .await
    .unwrap();
    assert_eq!(report.purged.len(), 1);
    assert_eq!(report.purged[0].path, "doc.pdf");
    assert_eq!(report.purged[0].revisions, 1);
    let audits = store::expunge_records::for_project(&world.surreal, world.project.id)
        .await
        .unwrap();
    assert_eq!(audits.len(), 1, "the purge leaves one audit row");
}

#[tokio::test]
async fn a_recovery_archive_cut_after_the_close_releases_a_durable_purge() {
    let Some(pg) = database().await else {
        return;
    };
    let world = world().await;
    let archive_store = Arc::new(MemoryStorage::default());
    let archive: Arc<dyn StorageService> = archive_store.clone();
    let doomed = world.file("final-letter", b"final letter").await;
    world.delete(doomed, at(0)).await;
    world.close().await;
    let now = window_closes(at(0));
    let lanes = RetentionLanes {
        pg: Some(&pg),
        archive: Some(&archive),
    };

    // Closed with no generation yet: the durable lane still waits.
    let waiting =
        purge_expired_documents_with(&world.surreal, &world.storage, lanes, &world.by_firm(), now)
            .await
            .unwrap();
    assert_eq!(waiting.awaiting_archive, 1);

    // A committed generation cut after the close stands in for the record.
    let authority =
        store::project_archives::authorize(&world.surreal, Role::Admin, world.admin, world.firm_id)
            .await
            .unwrap();
    let stores = store::project_archives::ArchiveStores {
        surreal: &world.surreal,
        pg: &pg,
        documents: &world.storage,
        archive: &archive,
        source_backups: None,
    };
    let cut = Utc::now() + Duration::minutes(1);
    let generation = store::project_archives::request(
        &stores,
        &authority,
        world.project.id,
        "closed",
        None,
        cut,
    )
    .await
    .unwrap();
    store::project_archives::run(&stores, &authority, generation.id, cut)
        .await
        .unwrap();

    let report =
        purge_expired_documents_with(&world.surreal, &world.storage, lanes, &world.by_firm(), now)
            .await
            .unwrap();
    assert_eq!(report.purged.len(), 1, "{report:?}");
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The run is recorded under its chain and window and marked finished.
    let (expunge_record_id, finished): (Option<Uuid>, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT expunge_record_id, finished_at FROM retention_purge \
         WHERE project_id = $1 AND chain_key = $2",
    )
    .bind(world.project.id)
    .bind("final-letter")
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(expunge_record_id, Some(report.purged[0].expunge_record_id));
    assert_eq!(finished, Some(now));
}

#[tokio::test]
async fn an_unreachable_run_record_leaves_the_chain_for_the_next_run() {
    let Some(pg) = database().await else {
        return;
    };
    let world = world().await;
    let doomed = world.file("stale-note", b"stale note").await;
    world.delete(doomed, at(0)).await;
    pg.pool().close().await;
    let now = window_closes(at(0));

    let outcome = world
        .scheduled(Some(pg), world.by_firm())
        .purge(world.project.id, "stale-note", now)
        .await;
    assert!(outcome.starts_with("failed: doc.pdf: "), "{outcome}");
    let row = assets::find_by_id(&world.surreal, doomed)
        .await
        .unwrap()
        .unwrap();
    assert!(row.purged_at.is_none(), "a failed run purges nothing");
    assert!(world.storage.get(&row.storage_key).await.is_ok());
}
