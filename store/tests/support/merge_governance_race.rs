//! An unlocked tier preflight cannot authorize a later protected identity change.
use serde_json::Value;
use std::time::Duration;
use store::person_merge::{PersonMergeError, PersonMergeReport};
use store::persons::Role;
use store::{application_store::ApplicationStore, postgres::PostgresDb};
use tokio::task::JoinSet;
use uuid::Uuid;

async fn row(pg: &PostgresDb, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(p) FROM person p WHERE id=$1")
        .bind(id)
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

async fn mailboxes(pg: &PostgresDb, people: &[Uuid]) -> Value {
    sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(m) ORDER BY id),'[]'::jsonb) FROM person_mailbox m WHERE person_id=ANY($1::uuid[])")
        .bind(people).fetch_one(pg.pool()).await.unwrap()
}

async fn stop(jobs: &mut JoinSet<Result<PersonMergeReport, PersonMergeError>>) {
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
}

async fn wait_on_owned_row(
    pg: &PostgresDb,
    blocker: i32,
    jobs: &mut JoinSet<Result<PersonMergeReport, PersonMergeError>>,
) -> Result<(), &'static str> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if jobs.try_join_next().is_some() {
                return Err("merge completed before the owned Person row wait was observed");
            }
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid
                 WHERE a.datname=current_database() AND l.locktype='transactionid' AND NOT l.granted
                 AND $1=ANY(pg_blocking_pids(a.pid))
                 AND a.query LIKE '%FROM person WHERE id=ANY%FOR UPDATE')",
            )
            .bind(blocker)
            .fetch_one(pg.pool())
            .await
            .map_err(|_| "owned lock query failed")?;
            if waiting {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "merge never reached the owned Person row lock")?
}

#[tokio::test]
async fn lawyer_merge_rechecks_the_locked_kept_tier_after_owner_promotion() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let kept = store::persons::create(&db, &store::persons::NewPerson::with_role(
            "Synthetic kept lawyer", format!("kept-{}@example.test", Uuid::new_v4()), Role::Lawyer,
        )).await.unwrap();
        let dropped = store::persons::create(&db, &store::persons::NewPerson::new(
            "Synthetic duplicate client", format!("dropped-{}@example.test", Uuid::new_v4()),
        )).await.unwrap();
        let subject = format!("synthetic-race-provider-{}", Uuid::new_v4());
        store::persons::link_microsoft_subject(&db, dropped.id, &subject).await.unwrap().unwrap();
        let dropped_before = row(&pg, dropped.id).await;
        let claims_before = mailboxes(&pg, &[kept.id, dropped.id]).await;
        let mut promotion = pg.pool().begin().await.unwrap();
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *promotion).await.unwrap();
        // The same canonical role/updated_at assignment as set_role, held uncommitted.
        // No Project rows exist in this sample, so no derived participation needs changing.
        sqlx::query("SELECT id FROM person WHERE id=$1 FOR UPDATE")
            .bind(kept.id).execute(&mut *promotion).await.unwrap();
        let promoted: Value = sqlx::query_scalar(
            "UPDATE person SET role='owner',updated_at=now() WHERE id=$1 RETURNING to_jsonb(person)",
        ).bind(kept.id).fetch_one(&mut *promotion).await.unwrap();
        let visible: String = sqlx::query_scalar("SELECT role FROM person WHERE id=$1")
            .bind(kept.id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(visible, "lawyer", "uncommitted Owner tier must not leak into preflight reads");
        let keep_email = kept.email.clone();
        let drop_email = dropped.email.clone();
        let worker = pg.clone();
        let mut jobs = JoinSet::new();
        jobs.spawn(async move {
            PostgresDb::with_application_db(worker, async {
                store::person_merge::merge_by_email_as_actor(
                    &ApplicationStore::uninitialized(), &keep_email, &drop_email, Role::Lawyer,
                ).await
            }).await.expect("genuine native application admission")
        });
        let waiting = wait_on_owned_row(&pg, blocker, &mut jobs).await;
        if let Err(reason) = waiting {
            stop(&mut jobs).await;
            promotion.rollback().await.unwrap();
            panic!("{reason}");
        }
        let committed = promotion.commit().await;
        if committed.is_err() { stop(&mut jobs).await; }
        committed.unwrap();
        let joined = tokio::time::timeout(Duration::from_secs(20), jobs.join_next()).await;
        if joined.is_err() { stop(&mut jobs).await; }
        let result = joined.expect("merge did not finish after owned promotion committed")
            .expect("one owned merge task").unwrap();
        assert!(matches!(result, Err(PersonMergeError::Governance)));
        assert_eq!(row(&pg, kept.id).await, promoted);
        assert_eq!(row(&pg, dropped.id).await, dropped_before);
        assert_eq!(mailboxes(&pg, &[kept.id, dropped.id]).await, claims_before);
        assert_eq!(store::persons::find_by_microsoft_subject(&db, &subject)
            .await.unwrap().unwrap().id, dropped.id);
    }).await;
}
