//! The `InboundMessageAlert` workflow when Slack accepts the alert but the
//! worker stops before the post step's checkpoint commits.

use std::sync::Arc;

use store::application_store::ApplicationStore;
use store::inbox::{ingest_inbound, IngestInbound};
use store::postgres::PostgresDb;
use uuid::Uuid;
use workflows::{InboundMessageAlertReport, InboundMessageAlertRequest};
use workflows_service::dbos_inbound_message_alert::{self, InboundMessageAlertError};
use workflows_service::inbound_message_alert::InboundMessageAlertService;

use crate::support::{database, APP_VERSION, DEADLINE};

#[path = "../support/lost_response_slack.rs"]
mod lost_response_slack;
use lost_response_slack::LostResponseSlackBot;

const MESSAGES_CHANNEL: &str = "CSYNTHETICMESSAGES";

/// One inbound email for the Firm that wears the deployment's default Brand,
/// so the alert routes to the deployment's messages channel.
async fn inbound_for_default_brand(application_store: &ApplicationStore) -> Uuid {
    let firm_id = match store::firms::firm_id_for_brand_key(application_store, "neon")
        .await
        .expect("read the default Brand")
    {
        Some(firm_id) => firm_id,
        None => {
            store::test_support::seed_firm_with_new_brand(application_store, "neon")
                .await
                .0
        }
    };
    ingest_inbound(
        application_store,
        &IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: "email",
            provider_account: "synthetic-account",
            provider_message_id: "alert-lost-response",
            provider_thread_ref: None,
            external_address: "sender@example.com",
            external_name: None,
            subject: None,
            body: "Synthetic message",
            source_page: None,
            source_channel: None,
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            project_id: None,
            occurred_at: "2026-11-23T00:00:00Z",
            raw_storage_key: None,
        },
    )
    .await
    .expect("ingest a synthetic email")
    .message_id
}

#[tokio::test]
async fn an_alert_accepted_before_its_checkpoint_is_reconciled_not_reposted() {
    let Some(db) = database().await else {
        return;
    };
    let postgres = db.store();
    let application_store = ApplicationStore::uninitialized();
    let message_id = PostgresDb::with_test_db(
        postgres.clone(),
        inbound_for_default_brand(&application_store),
    )
    .await;
    let bot = Arc::new(LostResponseSlackBot::default());
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_inbound_message_alert::register(
        &dbos,
        dbos_inbound_message_alert::Context::new(
            InboundMessageAlertService::new(
                application_store,
                bot.clone(),
                MESSAGES_CHANNEL.to_owned(),
                "CSYNTHETICSTAGING".to_owned(),
                false,
                true,
                Some("https://navigator.example.com".to_owned()),
            ),
            postgres,
        ),
    )
    .expect("register InboundMessageAlert");
    dbos.launch().await.expect("launch DBOS");

    let id = dbos_inbound_message_alert::workflow_id(message_id);
    let running = {
        let id = id.clone();
        tokio::spawn(async move {
            workflow
                .run_with(
                    InboundMessageAlertRequest { message_id },
                    dbos::RunOptions {
                        workflow_id: Some(&id),
                        ..Default::default()
                    },
                )
                .await
        })
    };
    tokio::time::timeout(DEADLINE, bot.reached.notified())
        .await
        .expect("the alert never reached Slack");
    dbos.shutdown().await;
    let interrupted = running.await.expect("the alert task panicked");
    assert!(
        matches!(interrupted, Err(dbos::Error::Interrupted { .. })),
        "shutdown interrupts the post: {interrupted:?}"
    );

    dbos.launch().await.expect("relaunch DBOS");
    let report = tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<InboundMessageAlertReport, InboundMessageAlertError>(&id)
            .expect("retrieve the recovered alert")
            .result(),
    )
    .await
    .expect("the recovered alert never finished")
    .expect("the recovered alert succeeds");
    dbos.shutdown().await;

    assert_eq!(report.state, "notified", "the reserved alert reconciles");
    let posted = bot.posted();
    assert_eq!(posted.len(), 1, "the alert posted twice: {posted:?}");
    assert_eq!(posted[0].0, MESSAGES_CHANNEL);
    assert_eq!(posted[0].1, Some(message_id.to_string()));
    db.close().await;
}
