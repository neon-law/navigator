//! Soft-deleted chains in the archived-projects lane: interrupted archiving,
//! verified active-copy cleanup that keeps shared objects, archive-backed
//! reads, and restoring every revision.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use cloud::{FsStorage, StorageError, StorageService};
use store::assets::{self, Filed};
use store::document_retention::{self as retention, RetentionError, RetentionStores};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::projects::Project;
use store::surreal::SurrealDb;
#[path = "support/native_project.rs"]
mod native_project;
use uuid::Uuid;

use store::test_support::MemoryStorage;

struct World {
    pg: PostgresDb,
    surreal: SurrealDb,
    documents: Arc<dyn StorageService>,
    archive_store: Arc<MemoryStorage>,
    archive: Arc<dyn StorageService>,
    project: Project,
    deleter: Uuid,
    _dir: tempfile::TempDir,
}

impl World {
    fn stores(&self) -> RetentionStores<'_> {
        RetentionStores {
            surreal: &self.surreal,
            pg: &self.pg,
            documents: &self.documents,
            archive: &self.archive,
        }
    }

    async fn file(&self, slug: &str, bytes: &[u8]) -> Uuid {
        let args = IngestArgs {
            project_id: self.project.id,
            source: "upload",
            filename: "doc.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            visibility: visibility::CLIENT,
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

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

async fn world() -> World {
    let pg = PostgresDb::application().await.unwrap();
    let surreal = SurrealDb::uninitialized();
    let project = native_project::create(&format!(
        "retention-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ))
    .await;
    let deleter = persons::create(
        &surreal,
        &NewPerson::with_role(
            "Retention Lawyer",
            "retention-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap()
    .id;
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id: project.id,
            person_id: deleter,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    let archive_store = Arc::new(MemoryStorage::default());
    World {
        pg,
        surreal,
        documents,
        archive: archive_store.clone(),
        archive_store,
        project,
        deleter,
        _dir: dir,
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_soft_deleted_chain_is_archived_cleaned_read_and_restored_whole() {
    store::test_support::with_postgres(async {
        let world = world().await;
        let v1 = world.file("pleading", b"pleading v1").await;
        let v2 = world.file("pleading", b"shared exhibit bytes").await;
        // A live sibling chain shares v2's bytes, and so its storage key.
        let sibling = world.file("exhibit", b"shared exhibit bytes").await;
        let before = assets::revisions(&world.surreal, world.project.id, "pleading")
            .await
            .unwrap();
        assets::soft_delete(
            &world.surreal,
            world.project.id,
            v1,
            world.deleter,
            None,
            at(1),
        )
        .await
        .unwrap();

        // The archive lane fails mid-chain: nothing is cleaned up.
        world
            .archive_store
            .fail_before_write
            .store(1, Ordering::SeqCst);
        let report = retention::reconcile_soft_deleted(&world.stores(), at(2), 100)
            .await
            .unwrap();
        assert_eq!(report.archived, 0);
        assert_eq!(report.stalled.len(), 1);
        for asset in &before {
            assert!(world.documents.get(&asset.storage_key).await.is_ok());
        }

        // The rerun converges, then removes only what no live row needs.
        let report = retention::reconcile_soft_deleted(&world.stores(), at(3), 100)
            .await
            .unwrap();
        assert_eq!(report.archived, 1);
        assert_eq!(report.active_removed, 1);
        let v1_row = assets::find_by_id(&world.surreal, v1)
            .await
            .unwrap()
            .unwrap();
        let v2_row = assets::find_by_id(&world.surreal, v2)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            world.documents.get(&v1_row.storage_key).await,
            Err(StorageError::NotFound(_))
        ));
        assert_eq!(
            world
                .documents
                .get(&v2_row.storage_key)
                .await
                .unwrap()
                .bytes,
            b"shared exhibit bytes",
            "the live sibling's shared object is kept"
        );
        let sibling_row = assets::find_by_id(&world.surreal, sibling)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sibling_row.storage_key, v2_row.storage_key);

        // A lawyer's read of the deleted revision comes from the archive.
        assert_eq!(
            retention::read_archived(&world.pg, &world.archive, &v1_row)
                .await
                .unwrap()
                .unwrap(),
            b"pleading v1"
        );
        // A rerun is a no-op.
        let again = retention::reconcile_soft_deleted(&world.stores(), at(4), 100)
            .await
            .unwrap();
        assert_eq!((again.archived, again.active_removed), (0, 0));

        // Restoring without the archive lane changes nothing.
        assert!(matches!(
            retention::restore_chain(
                &world.surreal,
                Some(&world.pg),
                &world.documents,
                None,
                world.project.id,
                v1,
                at(5)
            )
            .await,
            Err(RetentionError::ArchiveUnavailable)
        ));
        assert!(assets::find_by_id(&world.surreal, v1)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some());

        let restored = retention::restore_chain(
            &world.surreal,
            Some(&world.pg),
            &world.documents,
            Some(&world.archive),
            world.project.id,
            v1,
            at(6),
        )
        .await
        .unwrap();
        assert!(restored.changed);
        assert_eq!(restored.revisions, 2);
        let after = assets::revisions(&world.surreal, world.project.id, "pleading")
            .await
            .unwrap();
        assert_eq!(after.len(), before.len());
        for (old, new) in before.iter().zip(&after) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.sha256_hex, new.sha256_hex);
            assert_eq!(old.visibility, new.visibility);
            assert!(new.deleted_at.is_none());
            assert_eq!(
                store::assets::sha256_hex(
                    &world.documents.get(&new.storage_key).await.unwrap().bytes
                ),
                new.sha256_hex
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_damaged_archive_never_becomes_the_only_copy() {
    store::test_support::with_postgres(async {
        let world = world().await;
        let id = world.file("memo", b"memo bytes").await;
        assets::soft_delete(
            &world.surreal,
            world.project.id,
            id,
            world.deleter,
            None,
            at(1),
        )
        .await
        .unwrap();
        // Archive the chain but break the archived copy before cleanup runs.
        world
            .archive_store
            .corrupt_after_write
            .store(1, Ordering::SeqCst);
        let report = retention::reconcile_soft_deleted(&world.stores(), at(2), 100)
            .await
            .unwrap();
        assert_eq!(report.active_removed, 0);
        let row = assets::find_by_id(&world.surreal, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            world.documents.get(&row.storage_key).await.unwrap().bytes,
            b"memo bytes"
        );
    })
    .await;
}
