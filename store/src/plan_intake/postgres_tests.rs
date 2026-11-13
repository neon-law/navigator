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
    pg.0.close().await;
}
