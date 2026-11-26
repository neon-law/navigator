use super::postgres;
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

pub(crate) fn staged_row(firm_id: Uuid) -> StagedAssignment {
    let now = Utc::now();
    StagedAssignment {
        id: Uuid::now_v7(),
        firm_id,
        conversation_id: Uuid::now_v7(),
        source_message_id: Uuid::now_v7(),
        provider: "synthetic_plan".into(),
        member_name: Some("Synthetic Member".into()),
        member_email: Some("member@example.com".into()),
        member_person_id: Some(Uuid::now_v7()),
        member_entity_id: Some(Uuid::now_v7()),
        plan_member_id: Some("M-1".into()),
        benefit_code: None,
        plan_number: Some("P-1".into()),
        plan_case_number: Some("CASE-1".into()),
        notice_matter_type: Some("Will preparation".into()),
        plan_matter_type: "will".into(),
        plan_state: Some("nv".into()),
        assigned_at: now,
        legal_plan_id: Some(Uuid::now_v7()),
        legal_plan_name: Some("Synthetic Plan".into()),
        plan_portal_only: false,
        fee_covered_benefit: Some("Will".into()),
        fee_plan_fee_cents: Some(15_000),
        fee_supplemental_flat_cents: None,
        fee_supplemental_hourly_cents: Some(25_000),
        fee_effective_on: NaiveDate::from_ymd_opt(2026, 1, 1),
        conflict_summary: vec!["first finding".into(), "second finding".into()],
        conflict_blocking: false,
        conflict_checked: true,
        status: Status::Staged.as_str().into(),
        project_id: None,
        accepted_by: None,
        decided_at: None,
        take_started_at: None,
        decline_reason: None,
        inserted_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn staging_is_idempotent_per_message_and_a_decision_lands_once() {
    let Some(pg) = database().await else { return };
    let firm = Uuid::now_v7();
    let row = staged_row(firm);
    let staged = postgres::stage(&pg, &row).await.unwrap();
    assert_eq!(staged.conflict_summary, row.conflict_summary);
    assert_eq!(staged.fee_effective_on, row.fee_effective_on);

    let mut redelivered = staged_row(firm);
    redelivered.source_message_id = row.source_message_id;
    let again = postgres::stage(&pg, &redelivered).await.unwrap();
    assert_eq!(
        again.id, row.id,
        "a redelivered notice re-stages the same row"
    );
    assert_eq!(
        postgres::find_by_source_message(&pg, row.source_message_id)
            .await
            .unwrap()
            .map(|found| found.id),
        Some(row.id)
    );

    let mut declined = staged.clone();
    declined.status = Status::Declined.as_str().into();
    declined.decided_at = Some(Utc::now());
    declined.decline_reason = Some("Synthetic reason".into());
    let decided = postgres::decide(&pg, &declined).await.unwrap();
    assert_eq!(decided.status, "declined");
    assert_eq!(decided.decline_reason.as_deref(), Some("Synthetic reason"));
    assert_eq!(decided.fee_plan_fee_cents, Some(15_000));

    let mut accepted = staged;
    accepted.status = Status::Accepted.as_str().into();
    assert!(matches!(
        postgres::decide(&pg, &accepted).await,
        Err(PlanIntakeError::NotStaged)
    ));
    assert_eq!(
        postgres::find_by_id(&pg, row.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "declined"
    );
    pg.0.close().await;
}

#[tokio::test]
async fn a_firms_queue_is_newest_first_and_its_own() {
    let Some(pg) = database().await else { return };
    let firm = Uuid::now_v7();
    let mut older = staged_row(firm);
    older.assigned_at -= Duration::hours(1);
    postgres::stage(&pg, &older).await.unwrap();
    let newer = postgres::stage(&pg, &staged_row(firm)).await.unwrap();
    postgres::stage(&pg, &staged_row(Uuid::now_v7()))
        .await
        .unwrap();

    let queue: Vec<Uuid> = postgres::list_by_firm(&pg, firm)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(queue, vec![newer.id, older.id]);
    pg.0.close().await;
}

#[tokio::test]
async fn surreal_rows_copy_once_with_their_decision() {
    let Some(pg) = database().await else { return };
    let surreal = crate::test_support::mem_surreal().await;
    let row = staged_row(Uuid::now_v7());
    surreal
        .query(format!(
            "CREATE $id SET firm_id = $firm_id, conversation_id = $conversation_id, \
             source_message_id = $source_message_id, provider = 'synthetic_plan', \
             plan_matter_type = 'will', assigned_at = time::now(), plan_portal_only = false, \
             conflict_summary = 'first finding', conflict_blocking = false, \
             conflict_checked = false, status = 'declined', decided_at = time::now(), \
             decline_reason = 'Synthetic reason', fee_effective_on = '2026-01-01' \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, row.id)))
        .bind(("firm_id", record_id("firm", row.firm_id)))
        .bind((
            "conversation_id",
            record_id("inbox_conversation", row.conversation_id),
        ))
        .bind((
            "source_message_id",
            record_id("message", row.source_message_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    let copied = postgres::copy_from_surreal(&surreal, &pg).await.unwrap();
    assert_eq!((copied.applied, copied.failures.len()), (1, 0));
    assert_eq!(
        postgres::copy_from_surreal(&surreal, &pg)
            .await
            .unwrap()
            .applied,
        0
    );
    let carried = postgres::find_by_id(&pg, row.id).await.unwrap().unwrap();
    assert_eq!(carried.status, "declined");
    assert_eq!(carried.conflict_summary, vec!["first finding".to_string()]);
    assert_eq!(
        carried.fee_effective_on,
        NaiveDate::from_ymd_opt(2026, 1, 1)
    );
    assert_global_retry_after_failed_copy(&surreal, &pg, row.id).await;
    pg.0.close().await;
}

#[tokio::test]
async fn source_context_refuses_review_mutations_before_effects() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::configured().await.unwrap().unwrap();
        let source = SurrealDb::uninitialized();
        let id = Uuid::now_v7();
        let actor = Uuid::now_v7();
        let input = AcceptInput {
            attestation: true,
            acting_person_id: actor,
            brand: "synthetic".into(),
            code: "synthetic-plan".into(),
            name: None,
            plan_case_number: Some("CASE-1".into()),
            plan_matter_type: "will".into(),
            plan_state: None,
        };
        let calls = AtomicUsize::new(0);
        let error = crate::postgres_mirror::from_source(accept_with_onboarding(
            &source,
            &pg,
            id,
            &input,
            |_| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, PlanIntakeError::PostgresRequired));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            crate::postgres_mirror::from_source(resolve_member(
                &source,
                &pg,
                id,
                actor,
                "synthetic",
                None,
                "member@example.test"
            ))
            .await,
            Err(PlanIntakeError::PostgresRequired)
        ));
        assert!(matches!(
            crate::postgres_mirror::from_source(decline_from_review(
                &source,
                &pg,
                id,
                actor,
                "synthetic",
                "reason"
            ))
            .await,
            Err(PlanIntakeError::PostgresRequired)
        ));
    })
    .await;
}

// A completed global run can still contain a recorded row failure. Once the
// underlying problem is repaired, a later global run must retry that row.
async fn assert_global_retry_after_failed_copy(
    surreal: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) {
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query("DELETE FROM plan_intake WHERE id = $1")
        .bind(id)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE plan_intake ADD CONSTRAINT synthetic_copy_failure CHECK (status <> 'declined') NOT VALID")
        .execute(pg.pool())
        .await
        .unwrap();
    let first = crate::postgres_backfill::start(surreal, pg, 500, None)
        .await
        .unwrap();
    assert_eq!(first.status, crate::postgres_backfill::RunStatus::Completed);
    let failures: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mirror_backfill_failure WHERE run_id=$1 AND row_table='plan_intake' AND row_id=$2",
    )
    .bind(first.run_id)
    .bind(id.to_string())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(failures, 1, "the real run records the failed assignment");
    assert!(postgres::find_by_id(pg, id).await.unwrap().is_none());
    sqlx::query("ALTER TABLE plan_intake DROP CONSTRAINT synthetic_copy_failure")
        .execute(pg.pool())
        .await
        .unwrap();
    let retry = crate::postgres_backfill::start(surreal, pg, 500, None)
        .await
        .unwrap();
    assert!(retry.is_clean(), "{retry:?}");
    let recovered = postgres::find_by_id(pg, id).await.unwrap().unwrap();
    assert_eq!(recovered.status, "declined");
    assert_eq!(
        recovered.decline_reason.as_deref(),
        Some("Synthetic reason")
    );
}
