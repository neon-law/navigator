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

async fn captured_message(
    source: &SurrealDb,
    new: &crate::sent_emails::NewSentEmail,
) -> SentEmailImage {
    let id = Uuid::now_v7();
    source.query("CREATE $id CONTENT {recipient:$recipient,subject:$subject,sender:$sender,template_slug:$template,body:$body,outcome:$outcome,sg_message_id:$receipt,sent_at:$at,inserted_at:$at,updated_at:$at}")
        .bind(("id", crate::surreal::record_id("sent_email", id)))
        .bind(("recipient", new.recipient.clone()))
        .bind(("subject", new.subject.clone()))
        .bind(("sender", new.sender.clone()))
        .bind(("template", new.template_slug.clone()))
        .bind(("body", new.body.clone()))
        .bind(("outcome", new.outcome.clone()))
        .bind(("receipt", new.sg_message_id.clone()))
        .bind(("at", Datetime::from(new.sent_at)))
        .await.unwrap().check().unwrap();
    source_one(source, id).await.unwrap().unwrap()
}

#[tokio::test]
async fn sent_email_audit_replays_preserve_history_and_report_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    super::super::during_initial_copy(async {
    let surreal = super::super::from_source(crate::test_support::mem_surreal()).await;
    let first = captured_message(
        &surreal,
        &crate::sent_emails::NewSentEmail {
            recipient: "recipient@example.com".into(),
            subject: "Synthetic subject".into(),
            sender: "sender@example.com".into(),
            project_id: None,
            person_id: None,
            template_slug: Some("synthetic".into()),
            body: "Synthetic body".into(),
            outcome: "sent".into(),
            sg_message_id: Some("synthetic-receipt".into()),
            sent_at: "2026-10-16T12:00:00.123456789Z".parse().unwrap(),
        },
    )
    .await;
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

    // PostgreSQL is the system of record: its rows cannot be rewritten, and a
    // row SurrealDB never held (every send since the switch) is not drift.
    assert!(
        sqlx::query("UPDATE sent_email SET body='Synthetic changed body' WHERE id=$1")
            .bind(first.id)
            .execute(&pg.0)
            .await
            .is_err(),
        "the append-only trigger refuses a rewrite"
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
        reconcile(&surreal, &pg).await.unwrap().is_clean(),
        "a PostgreSQL-only row is the normal case"
    );

    // A historical row on a day the retention purge removed stays out.
    let purged_day: chrono::NaiveDate = "2024-01-02".parse().unwrap();
    sqlx::query("INSERT INTO sent_email_export (day, row_count, content_sha256, data_file, snapshot_id, metadata_version, purged_at) VALUES ($1, 0, $2, 'gs://synthetic/part.parquet', 1, 1, now())")
        .bind(purged_day)
        .bind("0".repeat(64))
        .execute(&pg.0)
        .await
        .unwrap();
    let old = captured_message(
        &surreal,
        &crate::sent_emails::NewSentEmail {
            recipient: "recipient@example.com".into(),
            subject: "Synthetic subject".into(),
            sender: "sender@example.com".into(),
            project_id: None,
            person_id: None,
            template_slug: None,
            body: "Synthetic body".into(),
            outcome: "sent".into(),
            sg_message_id: None,
            sent_at: "2024-01-02T09:00:00Z".parse().unwrap(),
        },
    )
    .await;
    assert_eq!(
        mirror(&surreal, &pg, old.id).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());

    // A referenced Project must exist; unscoped audit records remain valid.
    assert!(sqlx::query("INSERT INTO sent_email (id,recipient,subject,sender,project_id,body,outcome,sent_at,inserted_at,updated_at) VALUES ($1,'recipient@example.com','Synthetic','sender@example.com',$2,'Synthetic','sent',now(),now(),now())")
        .bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&pg.0).await.is_err());
    }).await;
    pg.0.close().await;
}

fn retained_message() -> crate::sent_emails::NewSentEmail {
    crate::sent_emails::NewSentEmail {
        recipient: "recipient@example.invalid".into(),
        subject: "Synthetic audit".into(),
        sender: "sender@example.invalid".into(),
        project_id: None,
        person_id: None,
        template_slug: None,
        body: "Synthetic history".into(),
        outcome: "sent".into(),
        sg_message_id: None,
        sent_at: "2024-01-02T12:00:00Z".parse().unwrap(),
    }
}

async fn exported_day(pg: &PostgresDb, day: NaiveDate) {
    use crate::sent_emails::postgres::{self as native, DayExport};
    let rows = native::day_rows(pg, day).await.unwrap();
    native::record_export(
        pg,
        &DayExport {
            day,
            row_count: u64::try_from(rows.len()).unwrap(),
            content_sha256: native::content_sha256(rows.iter().map(|row| row.id)),
            data_file: "gs://synthetic/sent-email.parquet".into(),
            snapshot_id: 1,
            metadata_version: 1,
            exported_at: Utc::now(),
            purged_at: None,
        },
    )
    .await
    .unwrap();
}

async fn day_waiters(pg: &PostgresDb, day: NaiveDate, expected: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND classid::bigint=1312902742 AND objid::bigint=$1 AND NOT granted")
                .bind(i64::from(chrono::Datelike::num_days_from_ce(&day)))
                .fetch_one(pg.pool()).await.unwrap();
            if waiting == expected { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("both retention operations must reach the shared day lock");
}

#[tokio::test]
async fn purge_winning_before_import_prevents_resurrection() {
    use crate::sent_emails::postgres::{self as native, DayPurge};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(crate::test_support::mem_surreal()).await;
    let captured = captured_message(&source, &retained_message()).await;
    let day = captured.sent_at.date_naive();
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, captured.id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    exported_day(&pg, day).await;
    let mut gate = pg.pool().begin().await.unwrap();
    native::lock_retention_day(&mut gate, day).await.unwrap();
    let purge_pg = pg.clone();
    let purge = tokio::spawn(async move { native::purge(&purge_pg, Utc::now()).await.unwrap() });
    day_waiters(&pg, day, 1).await;
    let import_pg = pg.clone();
    let import_source = source.clone();
    let import = tokio::spawn(async move {
        super::super::during_initial_copy(mirror(&import_source, &import_pg, captured.id))
            .await
            .unwrap()
    });
    day_waiters(&pg, day, 2).await;
    gate.commit().await.unwrap();
    assert_eq!(
        purge.await.unwrap(),
        vec![(
            day,
            DayPurge::Purged {
                deleted: 1,
                retained: 0
            }
        )]
    );
    assert_eq!(import.await.unwrap(), Mirrored::Unchanged);
    assert_eq!(native::day_rows(&pg, day).await.unwrap().len(), 0);
    assert!(parity(&source, &pg).await.unwrap().is_clean());
    pg.pool().close().await;
}

#[tokio::test]
async fn import_winning_before_purge_refuses_a_stale_export() {
    use crate::sent_emails::postgres::{self as native, DayPurge};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = super::super::from_source(crate::test_support::mem_surreal()).await;
    let captured = captured_message(&source, &retained_message()).await;
    let day = captured.sent_at.date_naive();
    exported_day(&pg, day).await;
    let mut gate = pg.pool().begin().await.unwrap();
    native::lock_retention_day(&mut gate, day).await.unwrap();
    let import_pg = pg.clone();
    let import_source = source.clone();
    let import = tokio::spawn(async move {
        super::super::during_initial_copy(mirror(&import_source, &import_pg, captured.id))
            .await
            .unwrap()
    });
    day_waiters(&pg, day, 1).await;
    let purge_pg = pg.clone();
    let purge = tokio::spawn(async move { native::purge(&purge_pg, Utc::now()).await.unwrap() });
    day_waiters(&pg, day, 2).await;
    gate.commit().await.unwrap();
    assert_eq!(import.await.unwrap(), Mirrored::Written);
    assert_eq!(purge.await.unwrap(), vec![(day, DayPurge::NotExported)]);
    assert_eq!(native::day_rows(&pg, day).await.unwrap().len(), 1);
    exported_day(&pg, day).await;
    assert_eq!(
        native::purge(&pg, Utc::now()).await.unwrap(),
        vec![(
            day,
            DayPurge::Purged {
                deleted: 1,
                retained: 0
            }
        )]
    );
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, captured.id))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    assert!(native::day_rows(&pg, day).await.unwrap().is_empty());
    pg.pool().close().await;
}
