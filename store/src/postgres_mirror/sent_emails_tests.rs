use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[test]
fn audit_key_and_timestamps_do_not_expose_message_content() {
    let id = Uuid::new_v4();
    let key = MirrorKey::SentEmail(id);
    assert_eq!(key.table(), "sent_email");
    assert_eq!(key.to_string(), format!("sent_email:{id}"));
    let at: DateTime<Utc> = "2026-10-16T12:00:00.123456789Z".parse().unwrap();
    assert_eq!(micros(at).timestamp_subsec_nanos(), 123_456_000);
}

#[tokio::test]
async fn sent_email_audit_replays_preserve_history_and_report_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let first = crate::sent_emails::record(
        &surreal,
        &crate::sent_emails::NewSentEmail {
            recipient: "recipient@example.com".into(),
            subject: "Synthetic subject".into(),
            sender: "sender@example.com".into(),
            project_id: None,
            template_slug: Some("synthetic".into()),
            body: "Synthetic body".into(),
            outcome: "sent".into(),
            sg_message_id: Some("synthetic-receipt".into()),
            sent_at: "2026-10-16T12:00:00.123456789Z".parse().unwrap(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, first.id).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, first.id).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    let stored = sqlx::query("SELECT body, sg_message_id, sent_at FROM sent_email WHERE id=$1")
        .bind(first.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(stored.get::<String, _>("body"), "Synthetic body");
    assert_eq!(
        stored.get::<String, _>("sg_message_id"),
        "synthetic-receipt"
    );
    assert_eq!(
        stored.get::<DateTime<Utc>, _>("sent_at"),
        micros(first.sent_at)
    );

    sqlx::query("DELETE FROM sent_email WHERE id=$1")
        .bind(first.id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());

    sqlx::query("UPDATE sent_email SET body='Synthetic changed body' WHERE id=$1")
        .bind(first.id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(!reconcile(&surreal, &pg).await.unwrap().is_clean());
    let body: String = sqlx::query_scalar("SELECT body FROM sent_email WHERE id=$1")
        .bind(first.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(
        body, "Synthetic changed body",
        "reconciliation never rewrites audit history"
    );

    surreal
        .query("DELETE $id")
        .bind(("id", crate::surreal::record_id("sent_email", first.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, first.id).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(
        !reconcile(&surreal, &pg).await.unwrap().is_clean(),
        "extra audit rows remain visible"
    );

    // A referenced Project must exist; unscoped audit records remain valid.
    assert!(sqlx::query("INSERT INTO sent_email (id,recipient,subject,sender,project_id,body,outcome,sent_at,inserted_at,updated_at) VALUES ($1,'recipient@example.com','Synthetic','sender@example.com',$2,'Synthetic','sent',now(),now(),now())")
        .bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&pg.0).await.is_err());
    pg.0.close().await;
}
