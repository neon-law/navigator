use super::*;
use crate::communications::{self, IngestArgs};
use crate::email_conversations::{self, NewConversation, NewMessage};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;

#[test]
fn communication_and_email_mirror_keys_disclose_only_opaque_ids() {
    let id = Uuid::nil();
    for key in [
        MirrorKey::Communication(id),
        MirrorKey::EmailConversation(id),
        MirrorKey::EmailConversationMessage(id),
    ] {
        assert!(key.to_string().ends_with(&id.to_string()));
        assert_eq!(super::super::row_id(&key), Some(id));
    }
}

#[tokio::test]
async fn reconciliation_refuses_cyclic_receipts_without_writing_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project =
        crate::test_support::seed_project_surreal(&surreal, "Synthetic receipt cycle").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;

    for size in [1, 2] {
        let mut ids = Vec::new();
        for _ in 0..size {
            let row = communications::log_note(
                &surreal,
                &communications::NewNote {
                    project_id: project,
                    author_person_id: None,
                    subject: None,
                    body: "Synthetic receipt".into(),
                },
            )
            .await
            .unwrap();
            ids.push(row.id);
        }
        for (index, id) in ids.iter().enumerate() {
            surreal
                .query("UPDATE $id SET acknowledges=$receipt")
                .bind(("id", crate::surreal::record_id("communication", *id)))
                .bind((
                    "receipt",
                    crate::surreal::record_id("communication", ids[(index + 1) % size]),
                ))
                .await
                .unwrap()
                .check()
                .unwrap();
        }
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), reconcile(&surreal, &pg))
                .await
                .expect("a receipt cycle must be refused without indefinite recursion");
        assert!(matches!(result, Err(MirrorError::Source)));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication")
            .fetch_one(&pg.0)
            .await
            .unwrap();
        assert_eq!(count, 0, "cyclic receipt rows must not be written");
        surreal
            .query("DELETE communication")
            .await
            .unwrap()
            .check()
            .unwrap();
    }
    pg.0.close().await;
}

#[tokio::test]
async fn reconciliation_preserves_deep_receipts_and_shared_ancestors() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project =
        crate::test_support::seed_project_surreal(&surreal, "Synthetic receipt chain").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;
    let mut ids = Vec::new();
    for index in 0..34 {
        let row = communications::log_note(
            &surreal,
            &communications::NewNote {
                project_id: project,
                author_person_id: None,
                subject: None,
                body: format!("Synthetic receipt {index}"),
            },
        )
        .await
        .unwrap();
        let ancestor = if index >= 32 { ids.first() } else { ids.last() };
        if let Some(ancestor) = ancestor {
            surreal
                .query("UPDATE $id SET acknowledges=$receipt")
                .bind(("id", crate::surreal::record_id("communication", row.id)))
                .bind((
                    "receipt",
                    crate::surreal::record_id("communication", *ancestor),
                ))
                .await
                .unwrap()
                .check()
                .unwrap();
        }
        ids.push(row.id);
    }
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication")
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(count, 34);
    for id in &ids[32..] {
        let ancestor: Uuid =
            sqlx::query_scalar("SELECT acknowledges FROM communication WHERE id=$1")
                .bind(id)
                .fetch_one(&pg.0)
                .await
                .unwrap();
        assert_eq!(ancestor, ids[0]);
    }
    pg.0.close().await;
}

#[tokio::test]
async fn missing_receipt_ancestor_refuses_repair_without_changing_target_rows() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project =
        crate::test_support::seed_project_surreal(&surreal, "Synthetic orphan receipt").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;
    let mut ids = Vec::new();
    for body in ["Synthetic ancestor", "Synthetic receipt"] {
        ids.push(
            communications::log_note(
                &surreal,
                &communications::NewNote {
                    project_id: project,
                    author_person_id: None,
                    subject: None,
                    body: body.into(),
                },
            )
            .await
            .unwrap()
            .id,
        );
    }
    surreal
        .query("UPDATE $id SET acknowledges=$ancestor")
        .bind(("id", crate::surreal::record_id("communication", ids[1])))
        .bind((
            "ancestor",
            crate::surreal::record_id("communication", ids[0]),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    surreal
        .query("DELETE $ancestor; UPDATE $id SET body='Synthetic changed source'")
        .bind(("id", crate::surreal::record_id("communication", ids[1])))
        .bind((
            "ancestor",
            crate::surreal::record_id("communication", ids[0]),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    assert!(matches!(
        reconcile(&surreal, &pg).await,
        Err(MirrorError::Source)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication")
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(
        count, 2,
        "the stale ancestor must not be deleted during refusal"
    );
    let receipt: (String, Option<Uuid>) =
        sqlx::query_as("SELECT body, acknowledges FROM communication WHERE id=$1")
            .bind(ids[1])
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(receipt, ("Synthetic receipt".into(), Some(ids[0])));
    let source = communications::find(&surreal, project, ids[1])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source.body, "Synthetic changed source");
    assert_eq!(source.acknowledges, Some(ids[0]));
    pg.0.close().await;
}

#[tokio::test]
async fn communications_and_email_threads_replay_reconcile_and_delete() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project =
        crate::test_support::seed_project_surreal(&surreal, "Synthetic Communication").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;

    let ingested = communications::ingest(
        &surreal,
        &IngestArgs {
            project_id: project,
            channel: communications::channel::EMAIL_INBOUND,
            direction: communications::direction::INBOUND,
            author_person_id: None,
            counterparty: Some("client@example.test"),
            subject: Some("Synthetic subject"),
            body: "Synthetic body",
            source_ref: Some("provider-ref-1"),
            asset_id: None,
            occurred_at: "2026-10-16T10:00:00Z",
        },
    )
    .await
    .unwrap();
    let communication = MirrorKey::Communication(ingested.communication_id);
    assert_eq!(
        mirror(&surreal, &pg, &communication).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &communication).await.unwrap(),
        Mirrored::Unchanged
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM communication WHERE channel=$1 AND source_ref=$2")
            .bind(communications::channel::EMAIL_INBOUND)
            .bind("provider-ref-1")
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(count, 1, "the source dedup key has one mirrored row");

    sqlx::query("UPDATE communication SET body='stale target'")
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let body: String = sqlx::query_scalar("SELECT body FROM communication WHERE id=$1")
        .bind(ingested.communication_id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(body, "Synthetic body");

    surreal
        .query("DELETE $id")
        .bind((
            "id",
            crate::surreal::record_id("communication", ingested.communication_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &communication).await.unwrap(),
        Mirrored::Deleted
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn email_threads_preserve_unassigned_scope_and_repeated_provider_ids() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project =
        crate::test_support::seed_project_surreal(&surreal, "Synthetic Email Thread").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;

    let conversation = email_conversations::open(
        &surreal,
        &NewConversation {
            token: "opaque-synthetic-token",
            external_email: "prospect@example.test",
            external_name: None,
            subject: "Synthetic intake",
            person_id: None,
            notation_id: None,
        },
    )
    .await
    .unwrap();
    let conversation_key = MirrorKey::EmailConversation(conversation);
    mirror(&surreal, &pg, &conversation_key).await.unwrap();
    let scope: (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT firm_id, project_id FROM email_conversation WHERE id=$1")
            .bind(conversation)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(scope, (None, None), "unattributed intake stays unassigned");
    let message = email_conversations::append(
        &surreal,
        &NewMessage {
            conversation_id: conversation,
            direction: email_conversations::DIRECTION_FROM_EXTERNAL,
            from_addr: "prospect@example.test",
            to_addr: "support@example.test",
            subject: "Synthetic intake",
            body_text: "Synthetic message",
            provider_message_id: Some("provider-message-1"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let message_key = MirrorKey::EmailConversationMessage(message);
    mirror(&surreal, &pg, &message_key).await.unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &message_key).await.unwrap(),
        Mirrored::Unchanged
    );
    let repeated_provider_id = email_conversations::append(
        &surreal,
        &NewMessage {
            conversation_id: conversation,
            direction: email_conversations::DIRECTION_TO_EXTERNAL,
            from_addr: "support@example.test",
            to_addr: "prospect@example.test",
            subject: "Synthetic intake",
            body_text: "A later hop",
            provider_message_id: Some("provider-message-1"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_ne!(message, repeated_provider_id);
    mirror(
        &surreal,
        &pg,
        &MirrorKey::EmailConversationMessage(repeated_provider_id),
    )
    .await
    .unwrap();
    let hops: i64 = sqlx::query_scalar("SELECT count(*) FROM email_conversation_message WHERE conversation_id=$1 AND provider_message_id=$2")
        .bind(conversation).bind("provider-message-1").fetch_one(&pg.0).await.unwrap();
    assert_eq!(
        hops, 2,
        "the mirror preserves repeated provider IDs on distinct hops"
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn email_receipt_mirror_preserves_link_role_and_reconciles_metadata() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project = crate::test_support::seed_project_surreal(&surreal, "Synthetic receipt").await;
    let entity = crate::projects::find_by_id(&surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(&pg, &[entity], &[]).await;
    let mut args = IngestArgs {
        project_id: project,
        channel: "email_outbound",
        direction: "outbound",
        author_person_id: None,
        counterparty: Some("counsel@example.test"),
        subject: None,
        body: "Synthetic message",
        source_ref: Some("synthetic:sent"),
        asset_id: None,
        occurred_at: "2026-10-16T10:00:00Z",
    };
    let sent = communications::ingest_email(
        &surreal,
        &args,
        communications::EmailDetails {
            counterparty_role: Some("opposing_counsel"),
            acknowledges: None,
        },
    )
    .await
    .unwrap();
    args.channel = "email_inbound";
    args.direction = "inbound";
    args.source_ref = Some("synthetic:receipt");
    args.occurred_at = "2026-10-16T11:00:00Z";
    let receipt = communications::ingest_email(
        &surreal,
        &args,
        communications::EmailDetails {
            counterparty_role: Some("opposing_counsel"),
            acknowledges: Some(sent.communication_id),
        },
    )
    .await
    .unwrap();
    let key = MirrorKey::Communication(receipt.communication_id);
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    let row: (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT counterparty_role, acknowledges FROM communication WHERE id=$1")
            .bind(receipt.communication_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        row,
        (Some("opposing_counsel".into()), Some(sent.communication_id))
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    sqlx::query("UPDATE communication SET counterparty_role=NULL, acknowledges=NULL WHERE id=$1")
        .bind(receipt.communication_id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}
