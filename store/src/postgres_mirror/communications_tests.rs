use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

#[tokio::test]
async fn approval_and_audit_rows_reconcile_without_duplicate_audit_events() {
    super::super::during_initial_copy(async {
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
    sqlx::query("UPDATE summary_sender_approval SET status='approved' WHERE id=$1").bind(approval_id).execute(pg.pool()).await.unwrap();
    assert_eq!(mirror(&surreal,&pg,&MirrorKey::SummarySenderApproval(approval_id)).await.unwrap(),Mirrored::Unchanged);
    assert!(!reconcile(&surreal,&pg).await.unwrap().is_clean());
    surreal.query("DELETE $id").bind(("id",crate::surreal::record_id("summary_sender_approval",approval_id))).await.unwrap().check().unwrap();
    assert!(!reconcile(&surreal,&pg).await.unwrap().is_clean());
    let retained:String=sqlx::query_scalar("SELECT status FROM summary_sender_approval WHERE id=$1").bind(approval_id).fetch_one(pg.pool()).await.unwrap();
    assert_eq!(retained,"approved");


    pg.0.close().await;
    }).await;
}
