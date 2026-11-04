use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use chrono::{Days, TimeDelta, TimeZone};

fn message(sent_at: DateTime<Utc>) -> NewSentEmail {
    NewSentEmail {
        recipient: "recipient@example.com".into(),
        subject: "Synthetic subject".into(),
        sender: "sender@example.com".into(),
        project_id: None,
        person_id: Some(Uuid::now_v7()),
        template_slug: Some("password_reset".into()),
        body: "Synthetic body".into(),
        outcome: "sent".into(),
        sg_message_id: Some("synthetic-receipt".into()),
        sent_at,
    }
}

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

fn noon(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(12, 0, 0).unwrap().and_utc()
}

async fn export(pg: &PostgresDb, day: NaiveDate) -> DayExport {
    let rows = day_rows(pg, day).await.unwrap();
    let export = DayExport {
        day,
        row_count: rows.len() as u64,
        content_sha256: content_sha256(rows.iter().map(|row| row.id)),
        data_file: format!("gs://synthetic/sent_email/data/sent_at_day={day}/part.parquet"),
        snapshot_id: 1,
        metadata_version: 1,
        exported_at: Utc::now(),
        purged_at: None,
    };
    record_export(pg, &export).await.unwrap();
    export
}

async fn count(pg: &PostgresDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sent_email")
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_recorded_send_reads_back_with_its_person_and_receipt() {
    let Some(pg) = database().await else { return };
    let written = record(&pg, &message(Utc::now())).await.unwrap();
    assert!(written.person_id.is_some());
    assert_eq!(written.sg_message_id.as_deref(), Some("synthetic-receipt"));
    assert_eq!(written.template_slug.as_deref(), Some("password_reset"));
    assert_eq!(all(&pg).await.unwrap(), vec![written.clone()]);
    let first = page(&pg, 9, 50, None).await.unwrap();
    assert_eq!((first.page, first.total_pages), (1, 1));
    assert_eq!(first.rows, vec![written]);
    assert!(page(&pg, 1, 50, Some(&[])).await.unwrap().rows.is_empty());
    pg.0.close().await;
}

#[tokio::test]
async fn the_database_refuses_to_change_or_remove_a_sent_email() {
    let Some(pg) = database().await else { return };
    let written = record(&pg, &message(Utc::now())).await.unwrap();

    let update = sqlx::query("UPDATE sent_email SET body = 'rewritten' WHERE id = $1")
        .bind(written.id)
        .execute(&pg.0)
        .await
        .unwrap_err();
    assert!(update.to_string().contains("append-only"), "{update}");

    let delete = sqlx::query("DELETE FROM sent_email WHERE id = $1")
        .bind(written.id)
        .execute(&pg.0)
        .await
        .unwrap_err();
    assert!(delete.to_string().contains("append-only"), "{delete}");

    // `project_service` references the table, so PostgreSQL refuses a plain
    // TRUNCATE before the trigger runs; the trigger stands behind it.
    assert!(sqlx::query("TRUNCATE sent_email")
        .execute(&pg.0)
        .await
        .is_err());

    // Naming the purge does not admit a row inside its retention year.
    let mut tx = pg.0.begin().await.unwrap();
    sqlx::query("SELECT set_config('navigator.sent_email_purge', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(sqlx::query("DELETE FROM sent_email WHERE id = $1")
        .bind(written.id)
        .execute(&mut *tx)
        .await
        .is_err());
    drop(tx);

    assert_eq!(all(&pg).await.unwrap(), vec![written]);
    pg.0.close().await;
}

#[tokio::test]
async fn an_unexported_old_row_cannot_be_deleted_even_by_the_purge_setting() {
    let Some(pg) = database().await else { return };
    let old = Utc::now() - TimeDelta::days(400);
    let written = record(&pg, &message(old)).await.unwrap();
    let mut tx = pg.0.begin().await.unwrap();
    sqlx::query("SELECT set_config('navigator.sent_email_purge', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let refused = sqlx::query("DELETE FROM sent_email WHERE id = $1")
        .bind(written.id)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("append-only"), "{refused}");
    drop(tx);
    assert!(purge(&pg, Utc::now()).await.unwrap().is_empty());
    assert_eq!(count(&pg).await, 1);
    pg.0.close().await;
}

#[tokio::test]
async fn a_closed_day_is_pending_until_exported_and_again_when_it_gains_a_row() {
    let Some(pg) = database().await else { return };
    let today = Utc::now().date_naive();
    let closed = today - Days::new(2);
    record(&pg, &message(noon(closed))).await.unwrap();
    record(&pg, &message(Utc::now())).await.unwrap();

    assert_eq!(pending_days(&pg, today).await.unwrap(), vec![closed]);
    let first = export(&pg, closed).await;
    assert!(pending_days(&pg, today).await.unwrap().is_empty());
    assert_eq!(export_for(&pg, closed).await.unwrap(), Some(first.clone()));

    record(&pg, &message(noon(closed) + TimeDelta::hours(1)))
        .await
        .unwrap();
    assert_eq!(pending_days(&pg, today).await.unwrap(), vec![closed]);
    let second = export(&pg, closed).await;
    assert_eq!(second.row_count, 2);
    assert_ne!(second.content_sha256, first.content_sha256);
    assert!(pending_days(&pg, today).await.unwrap().is_empty());
    pg.0.close().await;
}

#[tokio::test]
async fn the_purge_removes_only_exported_days_past_a_year_and_keeps_service_evidence() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let (_, last_day) = retention_cutoff(now);
    let old_day = last_day - Days::new(3);
    let recent_day = now.date_naive() - Days::new(30);
    let served = record(&pg, &message(noon(old_day))).await.unwrap();
    record(&pg, &message(noon(old_day) + TimeDelta::hours(1)))
        .await
        .unwrap();
    record(&pg, &message(noon(recent_day))).await.unwrap();
    export(&pg, old_day).await;
    export(&pg, recent_day).await;

    // A Project Service cites one old send as evidence; the fixture skips the
    // service's own foreign keys, which this test does not exercise.
    let mut conn = pg.0.acquire().await.unwrap();
    sqlx::query("SET session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO project_service (id, project_id, document_id, recipient, method, served_on, \
         address, declarant, sent_email_id, inserted_at, updated_at) VALUES \
         ($1, $2, $3, 'Synthetic recipient', 'e_service', $4, 'Synthetic address', \
         'Synthetic declarant', $5, now(), now())",
    )
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(old_day)
    .bind(served.id)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("SET session_replication_role = origin")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let report = purge(&pg, now).await.unwrap();
    assert_eq!(
        report,
        vec![(
            old_day,
            DayPurge::Purged {
                deleted: 1,
                retained: 1
            }
        )]
    );
    assert_eq!(count(&pg).await, 2, "the served send and the recent send");
    assert!(export_for(&pg, old_day)
        .await
        .unwrap()
        .unwrap()
        .purged_at
        .is_some());
    assert!(
        pending_days(&pg, now.date_naive())
            .await
            .unwrap()
            .is_empty(),
        "a purged day is never exported again"
    );
    assert!(
        purge(&pg, now).await.unwrap().is_empty(),
        "a re-run is a no-op"
    );
    pg.0.close().await;
}

#[tokio::test]
async fn a_day_that_gained_rows_since_its_export_is_not_purged() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let (_, last_day) = retention_cutoff(now);
    let day = last_day - Days::new(1);
    record(&pg, &message(noon(day))).await.unwrap();
    export(&pg, day).await;
    record(&pg, &message(noon(day) + TimeDelta::minutes(5)))
        .await
        .unwrap();
    assert_eq!(
        purge(&pg, now).await.unwrap(),
        vec![(day, DayPurge::NotExported)]
    );
    assert_eq!(count(&pg).await, 2);
    assert_eq!(
        pending_days(&pg, now.date_naive()).await.unwrap(),
        vec![day]
    );
    pg.0.close().await;
}

#[test]
fn a_day_is_purgeable_only_once_all_of_it_is_past_the_year() {
    let now = Utc.with_ymd_and_hms(2026, 11, 4, 2, 0, 0).unwrap();
    let (cutoff, last_day) = retention_cutoff(now);
    assert_eq!(cutoff, Utc.with_ymd_and_hms(2025, 11, 4, 2, 0, 0).unwrap());
    assert_eq!(last_day, NaiveDate::from_ymd_opt(2025, 11, 3).unwrap());
}

#[test]
fn the_content_digest_names_the_ids_in_order() {
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    assert_eq!(content_sha256([a, b]), content_sha256([a, b]));
    assert_ne!(content_sha256([a, b]), content_sha256([b, a]));
    assert_eq!(content_sha256([]).len(), 64);
}
