use super::{
    channel, direction, for_project, for_project_client_visible, ingest, purge_expired_matters,
    purge_for_project, CommunicationError, IngestArgs, RETENTION_YEARS,
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;

fn args<'a>(project_id: Uuid, ch: &'a str, dir: &'a str, body: &'a str) -> IngestArgs<'a> {
    IngestArgs {
        project_id,
        channel: ch,
        direction: dir,
        author_person_id: None,
        counterparty: None,
        subject: None,
        body,
        source_ref: None,
        asset_id: None,
        occurred_at: "2026-06-08T10:00:00Z",
    }
}

#[tokio::test]
async fn historical_messages_without_email_metadata_remain_readable() {
    let db = crate::surreal::test_support::mem().await;
    let project = crate::test_support::seed_project_surreal(&db, "Test Matter").await;
    let created = ingest(
        &db,
        &args(
            project,
            channel::EMAIL_OUTBOUND,
            direction::OUTBOUND,
            "Sent",
        ),
    )
    .await
    .unwrap();
    db.query("UPDATE $id UNSET counterparty_role, acknowledges")
        .bind((
            "id",
            crate::surreal::record_id(super::TABLE, created.communication_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    let row = super::find(&db, project, created.communication_id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.counterparty_role.is_none());
    assert!(row.acknowledges.is_none());
    assert!(row.receipt_acknowledged_at.is_none());
}

#[tokio::test]
async fn receipt_projection_uses_earliest_instant_and_only_the_same_project() {
    let db = crate::surreal::test_support::mem().await;
    let project = crate::test_support::seed_project_surreal(&db, "Test Matter").await;
    let outbound = ingest(
        &db,
        &args(
            project,
            channel::EMAIL_OUTBOUND,
            direction::OUTBOUND,
            "Sent",
        ),
    )
    .await
    .unwrap();
    let mut receipt = args(
        project,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "Acknowledged",
    );
    let metadata = super::EmailDetails {
        counterparty_role: Some("court"),
        acknowledges: Some(outbound.communication_id),
    };
    receipt.occurred_at = "2026-06-08T12:00:00+03:00";
    let earliest = super::ingest_email(&db, &receipt, metadata).await.unwrap();
    receipt.occurred_at = "2026-06-08T10:00:00Z";
    super::ingest_email(&db, &receipt, metadata).await.unwrap();
    let rows = super::for_project(&db, project).await.unwrap();
    let sent = rows
        .iter()
        .find(|row| row.id == outbound.communication_id)
        .unwrap();
    assert_eq!(
        sent.receipt_acknowledged_at.as_deref(),
        Some("2026-06-08T12:00:00+03:00")
    );
    let other = crate::test_support::seed_project_surreal(&db, "Test Matter").await;
    super::move_to_project(&db, earliest.communication_id, other)
        .await
        .unwrap();
    let rows = super::for_project(&db, project).await.unwrap();
    let sent = rows
        .iter()
        .find(|row| row.id == outbound.communication_id)
        .unwrap();
    assert_eq!(
        sent.receipt_acknowledged_at.as_deref(),
        Some("2026-06-08T10:00:00Z")
    );
}

#[tokio::test]
async fn ingest_inserts_a_spine_row() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let out = ingest(
        &surreal,
        &args(
            project_id,
            channel::DOCUMENT_COMMENT,
            direction::INBOUND,
            "Should this be my full legal name?",
        ),
    )
    .await
    .unwrap();
    assert!(!out.deduped);

    let rows = for_project(&surreal, project_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, out.communication_id);
    assert_eq!(rows[0].channel, channel::DOCUMENT_COMMENT);
    assert_eq!(rows[0].direction, direction::INBOUND);
    assert_eq!(rows[0].body, "Should this be my full legal name?");
    assert!(rows[0].source_ref.is_none());
}

/// LAW-140: the schema accepts `call` and refuses a channel it does not
/// list; a call hides from the client-visible read unless
/// `client_visible` is set, while the firm read carries its structure;
/// agreements stay unconfirmed until an email Communication confirms
/// them.
#[tokio::test]
async fn a_call_is_logged_with_agreements_until_an_email_confirms_them() {
    // LAW-424: the row's top-level direction is the call's own now,
    // so this fixture (outbound, opposing counsel, not shared) no
    // longer reads `internal`.
    use super::{confirm_agreements, record_call, CallDetails, NewCall};
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let unknown = ingest(
        &surreal,
        &args(project_id, "carrier_pigeon", "internal", "x"),
    )
    .await;
    assert!(
        unknown.is_err(),
        "an unlisted channel is refused by the schema"
    );

    let call = record_call(
        &surreal,
        &NewCall {
            project_id,
            counterparty: "counsel@example.com".into(),
            body: "Discussed the discovery schedule.".into(),
            at: "2026-10-09T15:00:00-07:00".into(),
            call: CallDetails {
                direction: "outbound".into(),
                counterparty_role: "opposing_counsel".into(),
                duration_minutes: Some(20),
                recorded: "none".into(),
                meet_and_confer: Some("Responses to Set One".into()),
                client_visible: false,
            },
            agreements: vec!["Extend responses to 2026-11-02".into()],
            ..NewCall::default()
        },
    )
    .await
    .expect("log the call");
    assert_eq!(call.channel, "call");
    assert_eq!(call.direction, "outbound");
    assert_eq!(call.counterparty_role.as_deref(), Some("opposing_counsel"));
    assert_eq!(call.call.as_ref().unwrap().duration_minutes, Some(20));
    assert_eq!(call.agreements[0].status, "unconfirmed");
    assert!(call.has_unconfirmed_agreements());

    let firm = for_project(&surreal, project_id).await.unwrap();
    assert_eq!(firm.len(), 1);
    assert_eq!(firm[0].call, call.call);
    assert!(for_project_client_visible(&surreal, project_id)
        .await
        .unwrap()
        .is_empty());

    // A call with opposing counsel cannot be shared with the client.
    let mut shared = NewCall {
        project_id,
        counterparty: "counsel@example.com".into(),
        body: "note".into(),
        at: "2026-10-09T15:00:00-07:00".into(),
        call: call.call.clone().unwrap(),
        client_visible: true,
        ..NewCall::default()
    };
    assert!(record_call(&surreal, &shared).await.is_err());
    shared.call.counterparty_role = "client".into();
    let visible = record_call(&surreal, &shared)
        .await
        .expect("a client call may be shared");
    assert_eq!(visible.direction, "outbound");
    assert_eq!(
        for_project_client_visible(&surreal, project_id)
            .await
            .unwrap()
            .len(),
        1
    );

    let email = ingest(
        &surreal,
        &args(
            project_id,
            "email_outbound",
            "outbound",
            "Confirming our call.",
        ),
    )
    .await
    .unwrap();
    let confirmed = confirm_agreements(&surreal, project_id, call.id, email.communication_id)
        .await
        .expect("confirm");
    assert_eq!(confirmed.agreements[0].status, "confirmed");
    assert_eq!(
        confirmed.agreements[0].confirmed_by.as_deref(),
        Some(email.communication_id.to_string().as_str())
    );
    assert!(!confirmed.has_unconfirmed_agreements());
    assert!(
        confirm_agreements(&surreal, project_id, email.communication_id, call.id)
            .await
            .is_err(),
        "only a call carries agreements"
    );
}

#[tokio::test]
async fn a_note_is_internal_without_a_counterparty_and_never_client_visible() {
    use super::{for_project, log_note, NewNote};
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let note = log_note(
        &surreal,
        &NewNote {
            project_id,
            author_person_id: None,
            subject: Some("  Call notes  ".into()),
            body: "  Client confirmed the schedule.  ".into(),
        },
    )
    .await
    .expect("log the note");
    assert_eq!(note.channel, "note");
    assert_eq!(note.direction, "internal");
    assert_eq!(note.subject.as_deref(), Some("Call notes"));
    assert_eq!(note.body, "Client confirmed the schedule.");
    assert_eq!(note.counterparty, None);
    assert!(note.call.is_none());

    let firm = for_project(&surreal, project_id).await.unwrap();
    assert_eq!(firm.len(), 1);
    assert!(for_project_client_visible(&surreal, project_id)
        .await
        .unwrap()
        .is_empty());

    let empty = log_note(
        &surreal,
        &NewNote {
            project_id,
            author_person_id: None,
            subject: None,
            body: "   ".into(),
        },
    )
    .await;
    assert!(
        matches!(empty, Err(CommunicationError::Invalid(_))),
        "a whitespace-only note is refused"
    );
}

#[tokio::test]
async fn ingest_dedupes_on_channel_and_source_ref() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let mut a = args(project_id, channel::EMAIL_INBOUND, direction::INBOUND, "hi");
    a.source_ref = Some("msg-abc@mail.example.com");

    let first = ingest(&surreal, &a).await.unwrap();
    assert!(!first.deduped);

    // Same Message-ID re-delivered (SendGrid retry): no duplicate row.
    let second = ingest(&surreal, &a).await.unwrap();
    assert!(second.deduped);
    assert_eq!(second.communication_id, first.communication_id);

    assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn ingest_refuses_source_ref_replay_across_projects() {
    let surreal = crate::surreal::test_support::mem().await;
    let first_project = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;
    let second_project = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;
    let mut first = args(
        first_project,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "private",
    );
    first.source_ref = Some("gmail:lawyer@example.test:message-1");
    ingest(&surreal, &first).await.unwrap();

    let mut replay = args(
        second_project,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "replacement",
    );
    replay.source_ref = first.source_ref;
    let error = ingest(&surreal, &replay).await.unwrap_err();
    assert!(matches!(error, CommunicationError::Invalid(_)));
    assert_eq!(for_project(&surreal, first_project).await.unwrap().len(), 1);
    assert!(for_project(&surreal, second_project)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn same_source_ref_different_channel_is_distinct() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let mut inbound = args(project_id, channel::EMAIL_INBOUND, direction::INBOUND, "q");
    inbound.source_ref = Some("ref-1");
    let mut outbound = args(
        project_id,
        channel::EMAIL_OUTBOUND,
        direction::OUTBOUND,
        "a",
    );
    outbound.source_ref = Some("ref-1");

    let a = ingest(&surreal, &inbound).await.unwrap();
    let b = ingest(&surreal, &outbound).await.unwrap();
    assert_ne!(a.communication_id, b.communication_id);
    assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_source_ref_key_is_enforced_by_the_database_not_only_the_lookup() {
    // The read-then-write in `ingest` is the common path; the unique
    // index is the guarantee. Written around the command — as a racer
    // that got past the lookup would be — the second insert must still
    // be refused, or one client message can land twice in their own
    // privileged thread.
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;
    let mut first = args(
        project_id,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "the only copy",
    );
    first.source_ref = Some("<dupe@mail.example.com>");
    ingest(&surreal, &first).await.unwrap();

    let raw = surreal
        .query(
            "CREATE type::record('communication', rand::uuid::v7()) SET \
             project_id = $project, channel = $channel, direction = 'inbound', \
             body = 'a second copy', source_ref = $source_ref, \
             occurred_at = '2026-06-08T10:00:00Z'",
        )
        .bind((
            "project",
            crate::surreal::record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("channel", channel::EMAIL_INBOUND.to_string()))
        .bind(("source_ref", "<dupe@mail.example.com>".to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("the unique index must refuse a duplicate source_ref");
    assert!(
        super::is_duplicate_source_ref(&raw),
        "the refusal must be recognizable as the source-ref key: {raw}"
    );
    assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn null_source_ref_never_dedupes() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    // Two comments with no external id are distinct events. This also
    // holds the `(channel, source_ref)` unique index honest: an absent
    // `source_ref` must not collide, or the first comment on a matter
    // would block every later one.
    ingest(
        &surreal,
        &args(
            project_id,
            channel::DOCUMENT_COMMENT,
            direction::INBOUND,
            "one",
        ),
    )
    .await
    .unwrap();
    ingest(
        &surreal,
        &args(
            project_id,
            channel::DOCUMENT_COMMENT,
            direction::INBOUND,
            "two",
        ),
    )
    .await
    .unwrap();

    assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn client_visible_excludes_internal_firm_notes() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    // A client-facing inbound message and a firm-internal note.
    ingest(
        &surreal,
        &args(
            project_id,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "client question",
        ),
    )
    .await
    .unwrap();
    ingest(
        &surreal,
        &args(
            project_id,
            channel::PORTAL_MESSAGE,
            direction::INTERNAL,
            "FIRM EYES ONLY — strategy note",
        ),
    )
    .await
    .unwrap();

    // The firm sees both; the client sees only the non-internal row, and
    // never the internal note's body.
    assert_eq!(for_project(&surreal, project_id).await.unwrap().len(), 2);
    let client_view = for_project_client_visible(&surreal, project_id)
        .await
        .unwrap();
    assert_eq!(client_view.len(), 1);
    assert_eq!(client_view[0].body, "client question");
    assert!(
        client_view
            .iter()
            .all(|c| c.direction != direction::INTERNAL),
        "a client must never read a firm-internal note"
    );
}

#[tokio::test]
async fn for_project_returns_thread_oldest_first() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    let mut later = args(
        project_id,
        channel::EMAIL_OUTBOUND,
        direction::OUTBOUND,
        "reply",
    );
    later.occurred_at = "2026-06-08T12:00:00Z";
    let mut earlier = args(
        project_id,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "question",
    );
    earlier.occurred_at = "2026-06-08T09:00:00Z";

    ingest(&surreal, &later).await.unwrap();
    ingest(&surreal, &earlier).await.unwrap();

    let rows = for_project(&surreal, project_id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].body, "question");
    assert_eq!(rows[1].body, "reply");
}

async fn fs_storage() -> Arc<dyn cloud::StorageService> {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(cloud::FsStorage::new(root).await.unwrap())
}

#[tokio::test]
async fn purge_for_project_deletes_the_log_and_its_raw_payload() {
    let surreal = crate::surreal::test_support::mem().await;
    let storage = fs_storage().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;

    // A raw .eml payload stored as a content-addressed asset, referenced by
    // an inbound-email communication.
    let asset_id =
        crate::assets::ingest_content(&surreal, &storage, b"raw rfc5322 bytes", "message/rfc822")
            .await
            .unwrap();
    let storage_key = crate::assets::find_by_id(&surreal, asset_id)
        .await
        .unwrap()
        .unwrap()
        .storage_key;

    let mut with_asset = args(
        project_id,
        channel::EMAIL_INBOUND,
        direction::INBOUND,
        "raw email body",
    );
    with_asset.asset_id = Some(asset_id);
    ingest(&surreal, &with_asset).await.unwrap();
    ingest(
        &surreal,
        &args(
            project_id,
            channel::PORTAL_MESSAGE,
            direction::INTERNAL,
            "a note",
        ),
    )
    .await
    .unwrap();

    let report = purge_for_project(&surreal, &storage, project_id)
        .await
        .unwrap();
    assert_eq!(report.communications_deleted, 2);
    assert_eq!(report.assets_deleted, 1);

    // The conversation log is gone, the asset row is gone, and the raw
    // payload is gone from storage — the row and its bytes share a fate.
    assert!(for_project(&surreal, project_id).await.unwrap().is_empty());
    assert!(crate::assets::find_by_id(&surreal, asset_id)
        .await
        .unwrap()
        .is_none());
    assert!(storage.get(&storage_key).await.is_err());
}

#[tokio::test]
async fn purge_expired_matters_only_purges_past_the_retention_window() {
    let surreal = crate::surreal::test_support::mem().await;
    let storage = fs_storage().await;

    // now, an old close (11 years ago → expired), a recent close (kept).
    let now: DateTime<Utc> = "2026-06-08T00:00:00Z".parse().unwrap();
    let old_project = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;
    let recent_project = crate::test_support::seed_project_surreal(&surreal, "Test Matter").await;
    set_closed_at(&surreal, old_project, "2015-01-01T00:00:00Z").await;
    set_closed_at(&surreal, recent_project, "2025-12-01T00:00:00Z").await;

    ingest(
        &surreal,
        &args(
            old_project,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "old",
        ),
    )
    .await
    .unwrap();
    ingest(
        &surreal,
        &args(
            recent_project,
            channel::EMAIL_INBOUND,
            direction::INBOUND,
            "recent",
        ),
    )
    .await
    .unwrap();

    let report = purge_expired_matters(&surreal, &storage, now, RETENTION_YEARS)
        .await
        .unwrap();
    assert_eq!(report.communications_deleted, 1, "only the expired matter");

    // The 11-year-old matter's log is destroyed; the recent one is kept.
    assert!(for_project(&surreal, old_project).await.unwrap().is_empty());
    assert_eq!(
        for_project(&surreal, recent_project).await.unwrap().len(),
        1
    );
}

async fn set_closed_at(surreal: &crate::surreal::SurrealDb, project_id: Uuid, when: &str) {
    surreal
        .query("UPDATE $id SET status = 'closed', closed_at = $closed_at")
        .bind(("id", crate::surreal::record_id("project", project_id)))
        .bind(("closed_at", when.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
}
