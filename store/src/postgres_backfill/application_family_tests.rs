//! Actual captured Application enrollment, independent native history, and cutoff refusal.

use super::*;
use crate::firm_journal::{BeneficialOwnership, Category, JournalEntry, Posting, Unit};
use crate::postgres_mirror::tests::{connect, fresh_schema, verified_family_capture};
use crate::sent_emails::NewSentEmail;

pub(super) async fn database() -> Option<(PostgresDb, SurrealDb, Uuid, Uuid, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let (source, actor, project) =
        crate::postgres_mirror::tenancy::family_tests::enrolled_source(&pg).await;
    let firm = sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1")
        .bind(project)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    Some((pg, source, actor, project, firm))
}

pub(super) async fn database_with_domain_parents(
) -> Option<(PostgresDb, SurrealDb, Uuid, Uuid, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let (source, actor, project) =
        crate::postgres_mirror::tenancy::family_tests::enrolled_source_with_domain_parents(&pg)
            .await;
    let firm = sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1")
        .bind(project)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    Some((pg, source, actor, project, firm))
}

fn event(firm: Uuid, actor: Uuid, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::now_v7(),
        firm_id: firm,
        actor_id: actor,
        key: key.into(),
        occurred_at: "2026-10-11T07:12:13.123456789+05:30".into(),
        memo: "Synthetic captured movement".into(),
        postings: [("cash", "100"), ("equity", "-100")]
            .into_iter()
            .map(|(account, quantity)| Posting {
                account: account.into(),
                quantity: quantity.into(),
                unit: Unit::fiat("USD", 2),
                ownership: BeneficialOwnership::Firm,
                restrictions: vec![],
                project_id: None,
                category: Category::Cash,
            })
            .collect(),
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}

async fn captured_event(source: &SurrealDb, entry: &JournalEntry) {
    source
        .query(
            "CREATE $id CONTENT {firm_id:$firm,command_key:$key,payload:$payload,inserted_at:$at}",
        )
        .bind(("id", crate::surreal::record_id("journal_entry", entry.id)))
        .bind(("firm", entry.firm_id.to_string()))
        .bind(("key", entry.key.clone()))
        .bind(("payload", serde_json::to_string(entry).unwrap()))
        .bind(("at", "2026-10-12T01:02:03.987654321-07:00"))
        .await
        .unwrap()
        .check()
        .unwrap();
}

fn message(project: Option<Uuid>) -> NewSentEmail {
    NewSentEmail {
        recipient: "synthetic@example.invalid".into(),
        subject: "Synthetic captured audit".into(),
        sender: "synthetic-sender@example.invalid".into(),
        project_id: project,
        person_id: None,
        template_slug: None,
        body: "Synthetic audit body".into(),
        outcome: "sent".into(),
        sg_message_id: None,
        sent_at: "2024-01-02T12:00:00Z".parse().unwrap(),
    }
}

async fn captured_email(source: &SurrealDb, message: &NewSentEmail) -> Uuid {
    let id = Uuid::now_v7();
    source.query("CREATE $id CONTENT {recipient:$recipient,subject:$subject,sender:$sender,project_id:$project,body:$body,outcome:$outcome,sent_at:$at,inserted_at:$at,updated_at:$at}")
        .bind(("id", crate::surreal::record_id("sent_email", id)))
        .bind(("recipient", message.recipient.clone())).bind(("subject", message.subject.clone()))
        .bind(("sender", message.sender.clone())).bind(("body", message.body.clone()))
        .bind(("outcome", message.outcome.clone()))
        .bind(("project", message.project_id.map(|id| crate::surreal::record_id("project", id))))
        .bind(("at", surrealdb::types::Datetime::from(message.sent_at)))
        .await.unwrap().check().unwrap();
    id
}

async fn prior_histories(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object('runs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.scope,r.id) FROM mirror_backfill_run r WHERE r.scope <> 'application:1'),'checkpoints',(SELECT jsonb_agg(to_jsonb(c) ORDER BY c.run_id,c.mirror) FROM mirror_backfill_checkpoint c JOIN mirror_backfill_run r ON r.id=c.run_id WHERE r.scope <> 'application:1'))")
        .fetch_one(pg.pool()).await.unwrap()
}

#[tokio::test]
async fn captured_application_history_enrolls_journal_and_sent_email_without_rewriting_native_history(
) {
    let Some((pg, source, actor, project, firm)) = database().await else {
        return;
    };
    let captured = event(firm, actor, "synthetic-captured");
    captured_event(&source, &captured).await;
    let captured_email = captured_email(&source, &message(Some(project))).await;
    let native = event(firm, actor, "synthetic-native");
    // The prior configured Journal writer is already PostgreSQL-native while its
    // Firm metadata lookup still uses the explicitly captured compatibility lane.
    crate::postgres_mirror::from_source(crate::firm_journal::post(&source, &pg, &native))
        .await
        .unwrap();
    let native_email = crate::sent_emails::postgres::record(&pg, &message(None))
        .await
        .unwrap();
    let header: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(r) FROM journal_entry r WHERE id=$1")
            .bind(native.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let before = prior_histories(&pg).await;
    let receipt = verified_family_capture(&source).await;
    let started = start_family(
        &source,
        &pg,
        family::Family::Application,
        receipt,
        1,
        Some(1),
        false,
    )
    .await
    .unwrap();
    assert_eq!(started.status, RunStatus::Paused);
    let completed = resume_family(
        &source,
        &pg,
        family::Family::Application,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert!(completed.is_clean(), "{completed:?}");
    let journal = completed
        .parity
        .iter()
        .find(|row| row.table == "journal_entry")
        .unwrap()
        .journal_handoff
        .as_ref()
        .unwrap();
    assert!(journal.is_clean());
    assert_eq!(
        (
            journal.source_rows,
            journal.represented_source_rows,
            journal.native_target_only
        ),
        (1, 1, 1)
    );
    let audit = completed
        .parity
        .iter()
        .find(|row| row.table == "sent_email")
        .unwrap()
        .sent_email_union
        .as_ref()
        .unwrap();
    assert_eq!((audit.compared_overlaps, audit.native_target_only), (1, 1));
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(r) FROM journal_entry r WHERE id=$1")
            .bind(native.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(after, header);
    for id in [native_email.id, captured_email] {
        assert!(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM sent_email WHERE id=$1)"
        )
        .bind(id)
        .fetch_one(pg.pool())
        .await
        .unwrap());
    }
    let replay = resume_family(
        &SurrealDb::uninitialized(),
        &pg,
        family::Family::Application,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert!(replay.is_clean());
    assert_eq!(prior_histories(&pg).await, before);
    pg.pool().close().await;
}

async fn mismatched_cached_capture(capture_changed_image: bool) {
    let Some((pg, source, actor, _, firm)) = database().await else {
        return;
    };
    let original = event(firm, actor, "synthetic-binding");
    captured_event(&source, &original).await;
    let receipt = verified_family_capture(&source).await;
    let mut changed = original.clone();
    changed.memo = "Synthetic same-count changed payload".into();
    if capture_changed_image {
        source
            .query("UPDATE $id SET payload=$payload")
            .bind((
                "id",
                crate::surreal::record_id("journal_entry", original.id),
            ))
            .bind(("payload", serde_json::to_string(&changed).unwrap()))
            .await
            .unwrap()
            .check()
            .unwrap();
    }
    let frozen = FamilySource {
        surreal: &source,
        journal: tokio::sync::OnceCell::new(),
    };
    assert_eq!(frozen.journal_rows().await.unwrap().len(), 1);
    let after = if capture_changed_image {
        &original
    } else {
        &changed
    };
    source
        .query("UPDATE $id SET payload=$payload")
        .bind((
            "id",
            crate::surreal::record_id("journal_entry", original.id),
        ))
        .bind(("payload", serde_json::to_string(after).unwrap()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let run = Uuid::now_v7();
    let result = with_import_lock(
        &pg,
        Box::pin(async {
            family::enroll(&pg, run, family::Family::Application, 1, receipt).await?;
            family::during_import(
                run,
                family::Family::Application,
                family::advance_with(&frozen, &pg, run, None, true),
            )
            .await
        }),
    )
    .await;
    assert!(matches!(
        result,
        Err(BackfillError::Family(family::FamilyError::Receipt))
    ));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_entry")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "capture mismatch is refused before any copied event"
    );
    assert!(family::require_scope(&pg, family::Family::Application)
        .await
        .is_err());
    pg.pool().close().await;
}

#[tokio::test]
async fn cached_journal_capture_cannot_borrow_a_different_receipt_fingerprint() {
    mismatched_cached_capture(true).await;
}

#[tokio::test]
async fn source_payload_change_before_application_activation_refuses() {
    mismatched_cached_capture(false).await;
}

async fn captured_email_with_deleted_native_parent(purged: bool) {
    use crate::sent_emails::postgres::{self as native, DayExport};
    let Some((pg, source, _, project, _)) = database().await else {
        return;
    };
    let message = message(Some(project));
    let id = captured_email(&source, &message).await;
    // A native deletion must not be undone from a stale compatibility parent.
    sqlx::query("DELETE FROM person_project_role WHERE project_id=$1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM project WHERE id=$1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    if purged {
        let day = message.sent_at.date_naive();
        native::record_export(
            &pg,
            &DayExport {
                day,
                row_count: 0,
                content_sha256: native::content_sha256([]),
                data_file: "gs://synthetic/empty-day.parquet".into(),
                snapshot_id: 1,
                metadata_version: 1,
                exported_at: chrono::Utc::now(),
                purged_at: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            native::purge(&pg, chrono::Utc::now()).await.unwrap(),
            vec![(
                day,
                native::DayPurge::Purged {
                    deleted: 0,
                    retained: 0
                }
            )]
        );
    }
    let receipt = verified_family_capture(&source).await;
    let report = start_family(
        &source,
        &pg,
        family::Family::Application,
        receipt,
        10,
        None,
        true,
    )
    .await
    .unwrap();
    let audit = report
        .parity
        .iter()
        .find(|row| row.table == "sent_email")
        .unwrap();
    let proof = audit.sent_email_union.as_ref().unwrap();
    if purged {
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(proof.excluded_purged, 1);
        assert_eq!(proof.source_only_remaining, 0);
        assert!(report
            .unresolved_references
            .iter()
            .all(|reference| reference.table != "sent_email"));
    } else {
        assert_eq!(report.status, RunStatus::Paused);
        assert_eq!(proof.excluded_purged, 0);
        assert_eq!(proof.source_only_remaining, 1);
        let reference = report
            .unresolved_references
            .iter()
            .find(|reference| reference.table == "sent_email" && reference.field == "project_id")
            .unwrap();
        assert_eq!(reference.newly_target_missing, 1);
        assert!(family::require_scope(&pg, family::Family::Application)
            .await
            .is_err());
    }
    for (table, id) in [("project", project), ("sent_email", id)] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE id=$1"
        )))
        .bind(id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
    pg.pool().close().await;
}

#[tokio::test]
async fn proven_purged_missing_audit_does_not_resurrect_or_require_its_deleted_project() {
    captured_email_with_deleted_native_parent(true).await;
}

#[tokio::test]
async fn unpurged_captured_audit_refuses_a_missing_native_project_without_resurrection() {
    captured_email_with_deleted_native_parent(false).await;
}
