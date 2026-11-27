use super::*;
use crate::application_store::ApplicationStore;
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::history::Family;
use crate::postgres::{
    fixtures::{activate_family, workflow_fixture},
    matter::with_test_db,
};
use crate::{assets, notation_documents as documents, notations};
use std::{sync::Arc, time::Duration};

async fn metadata(
    pg: &PostgresDb,
    project: Uuid,
    notes: Vec<notations::Notation>,
    assets: Vec<assets::Asset>,
    documents: Vec<documents::NotationDocument>,
    versions: Vec<documents::NotationDocumentVersion>,
) -> RestoreMetadata {
    sqlx::query("UPDATE project SET brand='neon' WHERE id=$1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    RestoreMetadata {
        format: super::super::FORMAT,
        project: crate::postgres::projects::find_by_id_on(pg, project)
            .await
            .unwrap()
            .unwrap(),
        assets,
        notations: notes,
        notation_documents: documents,
        notation_document_versions: versions,
    }
}

async fn bare_asset(pg: &PostgresDb) -> assets::Asset {
    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let db = ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let id = assets::ingest_content(
            &db,
            &storage,
            b"synthetic archived bytes",
            "application/octet-stream",
        )
        .await
        .unwrap();
        assets::find_by_id(&db, id).await.unwrap().unwrap()
    })
    .await
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one verified historical graph restored and replayed under strict ordinary writes"
)]
async fn verified_history_restores_orphans_without_widening_normal_writes() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, note, _) = workflow_fixture(&pg).await;
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(note)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    activate_family(&pg, Family::Notation).await;
    let mut asset = bare_asset(&pg).await;
    asset.id = Uuid::now_v7();
    let missing_note = Uuid::new_v4();
    asset.storage_key = format!("notations/{missing_note}/document.pdf");
    let now = chrono::Utc::now();
    let document = documents::NotationDocument {
        id: Uuid::now_v7(),
        notation_id: missing_note,
        original_asset_id: asset.id,
        current_version_id: None,
        inserted_at: now,
        updated_at: now,
    };
    let version = documents::NotationDocumentVersion {
        id: Uuid::now_v7(),
        document_id: Uuid::new_v4(),
        parent_version_id: None,
        markdown_asset_id: asset.id,
        anchor_manifest_asset_id: asset.id,
        parser_version: "synthetic".into(),
        schema_version: 1,
        authored_by_person_id: person,
        status: documents::STATUS_IMPORTED.into(),
        inserted_at: now,
        updated_at: now,
    };
    let snapshot = metadata(
        &pg,
        project,
        vec![],
        vec![asset.clone()],
        vec![document.clone()],
        vec![version.clone()],
    )
    .await;
    let db = ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        assert!(assets::restore_archived(&db, &asset).await.is_err());
        assert!(documents::restore_archived_document(&db, &document)
            .await
            .is_err());
        assert!(documents::restore_archived_version(&db, &version)
            .await
            .is_err());
    })
    .await;
    let parents = acquire(&pg, &snapshot).await.unwrap();
    assert_eq!(
        crate::postgres::assets::restore_history(&parents, &asset)
            .await
            .unwrap(),
        assets::RowRestore::Created
    );
    assert_eq!(
        crate::postgres::notation_documents::restore_document_history(&parents, &document)
            .await
            .unwrap(),
        assets::RowRestore::Created
    );
    assert_eq!(
        crate::postgres::notation_documents::restore_version_history(&parents, &version)
            .await
            .unwrap(),
        assets::RowRestore::Created
    );
    assert_eq!(
        crate::postgres::assets::restore_history(&parents, &asset)
            .await
            .unwrap(),
        assets::RowRestore::Identical
    );
    assert_eq!(
        crate::postgres::notation_documents::restore_document_history(&parents, &document)
            .await
            .unwrap(),
        assets::RowRestore::Identical
    );
    assert_eq!(
        crate::postgres::notation_documents::restore_version_history(&parents, &version)
            .await
            .unwrap(),
        assets::RowRestore::Identical
    );
    let mut unverified = asset.clone();
    unverified.id = Uuid::new_v4();
    assert!(
        crate::postgres::assets::restore_history(&parents, &unverified)
            .await
            .is_err()
    );
    parents.finish().await.unwrap();
    let mut conflict = snapshot.clone();
    conflict.assets[0].visibility = crate::documents::visibility::CLIENT.into();
    let parents = acquire(&pg, &conflict).await.unwrap();
    assert_eq!(
        crate::postgres::assets::restore_history(&parents, &conflict.assets[0])
            .await
            .unwrap(),
        assets::RowRestore::Conflict
    );
    parents.finish().await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation WHERE id=$1)")
        .bind(missing_note)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(!exists);
    pg.pool().close().await;
}

#[tokio::test]
async fn reset_winning_before_archive_lease_refuses_completion_and_retry_converges() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (_, note, _) = workflow_fixture(&pg).await;
    activate_family(&pg, Family::Notation).await;
    let db = ApplicationStore::uninitialized();
    let archived = with_test_db(pg.clone(), notations::find_by_id(&db, note))
        .await
        .unwrap()
        .unwrap();
    let snapshot = metadata(
        &pg,
        archived.project_id,
        vec![archived.clone()],
        vec![],
        vec![],
        vec![],
    )
    .await;
    with_test_db(pg.clone(), async {
        assert_eq!(
            crate::notation_reset::delete_draft(&db, note)
                .await
                .unwrap(),
            crate::notation_reset::ResetOutcome::Deleted
        );
        assert!(matches!(
            acquire(&pg, &snapshot).await,
            Err(ArchiveError::RestorePrerequisite(_))
        ));
        assert_eq!(
            notations::restore_archived(&db, &archived).await.unwrap(),
            assets::RowRestore::Created
        );
    })
    .await;
    let parents = acquire(&pg, &snapshot).await.unwrap();
    parents.finish().await.unwrap();
    assert!(with_test_db(pg.clone(), notations::find_by_id(&db, note))
        .await
        .unwrap()
        .is_some());
    pg.pool().close().await;
}

#[tokio::test]
async fn queued_reset_cannot_deadlock_inner_archive_writes_or_remove_restored_facts() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (_, note, _) = workflow_fixture(&pg).await;
    activate_family(&pg, Family::Notation).await;
    let db = ApplicationStore::uninitialized();
    let archived = with_test_db(pg.clone(), notations::find_by_id(&db, note))
        .await
        .unwrap()
        .unwrap();
    let mut asset = bare_asset(&pg).await;
    asset.id = Uuid::now_v7();
    asset.secondary_storage_key = Some(format!("notations/{note}/document.pdf"));
    let snapshot = metadata(
        &pg,
        archived.project_id,
        vec![archived],
        vec![asset.clone()],
        vec![],
        vec![],
    )
    .await;
    let mut parents = acquire(&pg, &snapshot).await.unwrap();
    let lease_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *parents.transaction)
        .await
        .unwrap();
    let reset_pg = pg.clone();
    let reset = tokio::spawn(async move {
        let db = ApplicationStore::uninitialized();
        with_test_db(reset_pg, crate::notation_reset::delete_draft(&db, note)).await
    });
    tokio::time::timeout(Duration::from_secs(5),async{
        loop{
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock')").bind(lease_pid).fetch_one(pg.pool()).await.unwrap();
            if waiting{break;}
            tokio::task::yield_now().await;
        }
    }).await.expect("reset is queued behind the actual parent lease");
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(3),
            crate::postgres::assets::restore_history(&parents, &asset)
        )
        .await
        .expect("inner restore must not queue behind reset")
        .unwrap(),
        assets::RowRestore::Created
    );
    parents.finish().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), reset)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        crate::notation_reset::ResetOutcome::Kept
    );
    assert!(with_test_db(pg.clone(), notations::find_by_id(&db, note))
        .await
        .unwrap()
        .is_some());
    pg.pool().close().await;
}
