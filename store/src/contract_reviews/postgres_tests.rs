//! Native Contract Review fields, edits, timestamps, and independent review identities.
use super::*;
use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use chrono::{DateTime, SubsecRound, Utc};
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

async fn assert_stored(pg: &PostgresDb, expected: &ContractReview) {
    let row = sqlx::query("SELECT * FROM contract_review WHERE id=$1")
        .bind(expected.id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<Uuid, _>("notation_id"), expected.notation_id);
    assert_eq!(row.get::<Uuid, _>("playbook_id"), expected.playbook_id);
    assert_eq!(row.get::<Option<Uuid>, _>("asset_id"), expected.asset_id);
    assert_eq!(row.get::<Value, _>("findings"), expected.findings);
    assert_eq!(row.get::<String, _>("status"), expected.status);
    assert_eq!(
        row.get::<Option<String>, _>("risk_summary"),
        expected.risk_summary
    );
    assert_eq!(
        row.get::<DateTime<Utc>, _>("inserted_at"),
        expected.inserted_at.trunc_subsecs(6)
    );
    assert_eq!(
        row.get::<DateTime<Utc>, _>("updated_at"),
        expected.updated_at.trunc_subsecs(6)
    );
}

async fn review_fixture(pg: &PostgresDb, db: &ApplicationStore) -> NewContractReview {
    // Reuse the crate's canonical domain-row fixture, not its enrollment helper.
    let (_, notation_id, _) = crate::postgres::fixtures::workflow_fixture(pg).await;
    let entity_id = sqlx::query_scalar(
        "SELECT p.entity_id FROM notation n JOIN project p ON p.id=n.project_id WHERE n.id=$1",
    )
    .bind(notation_id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let playbook_id = crate::playbooks::create(
        db,
        &crate::playbooks::NewPlaybook {
            entity_id,
            name: "Synthetic review playbook",
            positions: &[],
        },
    )
    .await
    .unwrap();
    NewContractReview {
        notation_id,
        playbook_id,
        asset_id: None,
    }
}

#[tokio::test]
async fn native_lifecycle_preserves_fields_timestamps_and_other_reviews() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = ApplicationStore::uninitialized();
        let directory = tempfile::tempdir().unwrap();
        let storage: Arc<dyn cloud::StorageService> =
            Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
        let first = review_fixture(&pg, &db).await;
        let mut second = review_fixture(&pg, &db).await;
        second.asset_id = Some(
            crate::assets::ingest_content(&db, &storage, b"Synthetic contract", "text/plain")
                .await
                .unwrap(),
        );
        let id = create(&db, &first).await.unwrap();
        let other = create(&db, &second).await.unwrap();
        let original = by_id(&db, id).await.unwrap().unwrap();
        let other_original = by_id(&db, other).await.unwrap().unwrap();
        assert_eq!(original.notation_id, first.notation_id);
        assert_eq!(original.playbook_id, first.playbook_id);
        assert_eq!(original.asset_id, None);
        assert_eq!(original.status, STATUS_PENDING);
        assert!(findings_of(&original).unwrap().is_empty());
        assert_eq!(other_original.asset_id, second.asset_id);
        assert_stored(&pg, &original).await;
        assert_stored(&pg, &other_original).await;
        let approved = edit_review(&pg, &db, &original).await;
        assert_eq!(
            latest_for_notation(&db, first.notation_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            latest_for_notation(&db, second.notation_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            other
        );
        assert_missing(&pg, &db, &approved).await;
        assert_eq!(
            by_id(&db, other).await.unwrap(),
            Some(other_original.clone())
        );
        assert_stored(&pg, &other_original).await;
        assert_stored(&pg, &approved).await;
        clear_and_reject(&pg, &db, &approved).await;
        assert_eq!(by_id(&db, other).await.unwrap(), Some(other_original));
    })
    .await;
}

async fn edit_review(
    pg: &PostgresDb,
    db: &ApplicationStore,
    original: &ContractReview,
) -> ContractReview {
    let id = original.id;
    let mut findings = vec![Finding {
        clause_ref: "Synthetic section".into(),
        deviation: "Synthetic deviation".into(),
        severity: crate::playbooks::SEVERITY_HIGH.into(),
        suggested_redline: Some("Synthetic revision".into()),
        attorney_note: None,
        accepted: false,
    }];
    record_analysis(db, id, "Synthetic analysis", &findings)
        .await
        .unwrap();
    let analyzed = by_id(db, id).await.unwrap().unwrap();
    assert_stored(pg, &analyzed).await;
    assert_eq!(analyzed.status, STATUS_ANALYZED);
    assert_eq!(analyzed.inserted_at, original.inserted_at);
    assert!(analyzed.updated_at >= original.updated_at);
    assert_eq!(analyzed.risk_summary.as_deref(), Some("Synthetic analysis"));
    assert_eq!(findings_of(&analyzed).unwrap(), findings);
    findings[0].attorney_note = Some("Synthetic attorney note".into());
    findings[0].accepted = true;
    update_findings(db, id, &findings).await.unwrap();
    let edited = by_id(db, id).await.unwrap().unwrap();
    assert_eq!(findings_of(&edited).unwrap(), findings);
    assert_stored(pg, &edited).await;
    update_risk_summary(db, id, "Synthetic edited summary")
        .await
        .unwrap();
    let summary = by_id(db, id).await.unwrap().unwrap();
    assert_eq!(
        summary.risk_summary.as_deref(),
        Some("Synthetic edited summary")
    );
    assert_eq!(
        summary.status, STATUS_ANALYZED,
        "risk editing leaves status untouched"
    );
    assert_eq!(findings_of(&summary).unwrap(), findings);
    assert_stored(pg, &summary).await;
    set_status(db, id, STATUS_APPROVED).await.unwrap();
    let approved = by_id(db, id).await.unwrap().unwrap();
    assert_stored(pg, &approved).await;
    assert_eq!(approved.status, STATUS_APPROVED);
    assert_eq!(findings_of(&approved).unwrap(), findings);
    assert!(acted_finding_indices(db, original.notation_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(approved.inserted_at, original.inserted_at);
    assert_eq!(approved.notation_id, original.notation_id);
    assert_eq!(approved.playbook_id, original.playbook_id);
    assert_eq!(approved.asset_id, original.asset_id);
    assert!(approved.updated_at >= summary.updated_at);
    approved
}

async fn assert_missing(pg: &PostgresDb, db: &ApplicationStore, approved: &ContractReview) {
    let missing = Uuid::new_v4();
    assert!(by_id(db, missing).await.unwrap().is_none());
    let findings = findings_of(approved).unwrap();
    assert!(
        matches!(record_analysis(db,missing,"Synthetic",&findings).await,Err(ContractReviewError::NotFound(found)) if found==missing)
    );
    assert!(
        matches!(update_findings(db,missing,&findings).await,Err(ContractReviewError::NotFound(found)) if found==missing)
    );
    assert!(
        matches!(update_risk_summary(db,missing,"Synthetic").await,Err(ContractReviewError::NotFound(found)) if found==missing)
    );
    assert!(
        matches!(set_status(db,missing,STATUS_APPROVED).await,Err(ContractReviewError::NotFound(found)) if found==missing)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM contract_review")
            .fetch_one(pg.pool())
            .await
            .unwrap(),
        2
    );
    assert!(by_id(db, missing).await.unwrap().is_none());
}

async fn clear_and_reject(pg: &PostgresDb, db: &ApplicationStore, approved: &ContractReview) {
    update_findings(db, approved.id, &[]).await.unwrap();
    set_status(db, approved.id, STATUS_REJECTED).await.unwrap();
    let final_row = by_id(db, approved.id).await.unwrap().unwrap();
    assert_eq!(final_row.id, approved.id);
    assert_eq!(final_row.notation_id, approved.notation_id);
    assert_eq!(final_row.playbook_id, approved.playbook_id);
    assert_eq!(final_row.status, STATUS_REJECTED);
    assert_eq!(
        final_row.risk_summary.as_deref(),
        Some("Synthetic edited summary")
    );
    assert_eq!(final_row.inserted_at, approved.inserted_at);
    assert!(findings_of(&final_row).unwrap().is_empty());
    assert_stored(pg, &final_row).await;
}
