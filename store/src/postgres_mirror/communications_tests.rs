use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

#[test]
fn communication_keys_expose_only_opaque_identifiers() {
    let id = Uuid::nil();
    let key = MirrorKey::SummarySenderApproval(id);
    assert_eq!(key.table(), "summary_sender_approval");
    assert_eq!(key.to_string(), format!("summary_sender_approval:{id}"));
    let event = MirrorKey::SummarySenderApprovalEvent(id);
    assert_eq!(event.table(), "summary_sender_approval_event");
    assert_eq!(
        event.to_string(),
        format!("summary_sender_approval_event:{id}")
    );
}

#[tokio::test]
async fn approval_and_audit_rows_reconcile_without_duplicate_audit_events() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let firm_id = Uuid::now_v7();
    let person_id = Uuid::now_v7();
    let approval_id = Uuid::now_v7();
    let event_id = Uuid::now_v7();

    sqlx::query(
        "INSERT INTO firm (id, name, status, slug) VALUES ($1, 'Synthetic Firm', 'active', $2)",
    )
    .bind(firm_id)
    .bind(format!("s-{firm_id}"))
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query("INSERT INTO person (id, name, email) VALUES ($1, 'Synthetic Person', 'person@example.com')")
        .bind(person_id).execute(&pg.0).await.unwrap();
    surreal.query("CREATE $id SET firm_id = $firm, provider_account = 'synthetic', person_id = $person, sender_email = 'person@example.com', status = 'approved', updated_at = time::now()")
        .bind(("id", crate::surreal::record_id("summary_sender_approval", approval_id)))
        .bind(("firm", crate::surreal::record_id("firm", firm_id)))
        .bind(("person", crate::surreal::record_id("person", person_id)))
        .await.unwrap().check().unwrap();
    surreal.query("CREATE $id SET firm_id = $firm, actor_person_id = $person, provider_account = 'synthetic', person_id = $person, sender_email = 'person@example.com', action = 'approved', inserted_at = time::now()")
        .bind(("id", crate::surreal::record_id("summary_sender_approval_event", event_id)))
        .bind(("firm", crate::surreal::record_id("firm", firm_id)))
        .bind(("person", crate::surreal::record_id("person", person_id)))
        .await.unwrap().check().unwrap();

    assert_eq!(
        mirror(
            &surreal,
            &pg,
            &MirrorKey::SummarySenderApproval(approval_id)
        )
        .await
        .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(
            &surreal,
            &pg,
            &MirrorKey::SummarySenderApprovalEvent(event_id)
        )
        .await
        .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(
            &surreal,
            &pg,
            &MirrorKey::SummarySenderApprovalEvent(event_id)
        )
        .await
        .unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    surreal
        .query("UPDATE $id SET status = 'revoked', updated_at = time::now()")
        .bind((
            "id",
            crate::surreal::record_id("summary_sender_approval", approval_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query("DELETE FROM summary_sender_approval WHERE id = $1")
        .bind(approval_id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let status: String =
        sqlx::query_scalar("SELECT status FROM summary_sender_approval WHERE id = $1")
            .bind(approval_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(status, "revoked");

    pg.0.close().await;
}
