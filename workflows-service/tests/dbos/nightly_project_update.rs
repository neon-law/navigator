//! The Nightly Project Update's registered DBOS parent reattaches to its
//! daily run after restart and does not repeat receipt-backed Slack notices.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::test_support::mem_surreal;
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;
use workflows::dbos_engine;
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel};
use workflows_service::dbos_ingress::{NightlyIngress, WorkerIngress};
use workflows_service::dbos_nightly_project_update::{
    self as nightly, Context, NightlyError, Providers,
};
use workflows_service::nightly_project_update::repository::RepositoryForge;
use workflows_service::nightly_project_update::NightlyProjectUpdateReport;

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone, Default)]
struct AcceptInviteThenHold {
    inner: CapturingSlackBot,
    members: Arc<Mutex<Vec<String>>>,
    invite_calls: Arc<AtomicUsize>,
    invited_users: Arc<Mutex<Vec<String>>>,
    accepted: Arc<Notify>,
    hold_invite: bool,
    hold_project_notice: bool,
    notice_calls: Arc<AtomicUsize>,
}

impl AcceptInviteThenHold {
    fn holding_invites() -> Self {
        Self {
            hold_invite: true,
            ..Default::default()
        }
    }

    fn holding_project_notices() -> Self {
        Self {
            hold_project_notice: true,
            ..Default::default()
        }
    }
}

#[async_trait]
impl SlackBot for AcceptInviteThenHold {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.inner.create_private_channel(name).await
    }
    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.inner.post_message(channel, text).await
    }
    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.inner.user_id_by_email(email).await
    }
    async fn channel_member_ids(&self, _channel: &str) -> Result<Vec<String>, SlackBotError> {
        Ok(self.members.lock().await.clone())
    }
    async fn invite_to_channel(
        &self,
        _channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        if !users.is_empty() {
            self.invited_users
                .lock()
                .await
                .extend(users.iter().cloned());
        }
        self.members.lock().await.extend(users.iter().cloned());
        if self.hold_invite && self.invite_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // Slack accepted the invite, but the process loses its reply before
            // DBOS can checkpoint the step result.
            self.accepted.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation: Option<&str>,
    ) -> Result<workflows::SlackMessageReceipt, SlackBotError> {
        let receipt = self
            .inner
            .post_message_with_receipt(channel, text, correlation)
            .await?;
        if self.hold_project_notice
            && correlation.is_some_and(|id| {
                id.starts_with("nightly-project-update-2026-10-23-")
                    && !id.ends_with("-start")
                    && !id.ends_with("-complete")
            })
            && self.notice_calls.fetch_add(1, Ordering::SeqCst) == 0
        {
            self.accepted.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(receipt)
    }
    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation: &str,
    ) -> Result<Option<workflows::SlackMessageReceipt>, SlackBotError> {
        self.inner
            .message_receipt_by_correlation(channel, correlation)
            .await
    }
    async fn channel_canvas_id(&self, channel: &str) -> Result<Option<String>, SlackBotError> {
        self.inner.channel_canvas_id(channel).await
    }
    async fn create_channel_canvas(
        &self,
        channel: &str,
        markdown: &str,
    ) -> Result<String, SlackBotError> {
        self.inner.create_channel_canvas(channel, markdown).await
    }
    async fn replace_canvas(&self, id: &str, markdown: &str) -> Result<(), SlackBotError> {
        self.inner.replace_canvas(id, markdown).await
    }
}

async fn seed_project(surreal: &store::surreal::SurrealDb) -> uuid::Uuid {
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: "synthetic-nightly".into(),
            name: "Synthetic Nightly Project".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("seed synthetic Project");
    store::projects::set_internal_slack_channel_id(surreal, project.id, "C0123456789")
        .await
        .expect("set synthetic internal Slack channel");
    let person = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Lawyer",
            "synthetic@example.invalid",
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("seed synthetic Project member");
    store::projects::add_participation(surreal, project.id, person.id, "lawyer")
        .await
        .expect("assign synthetic Project member");
    store::projects::designate_dri_in_surreal(
        surreal,
        project.id,
        person.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .expect("designate synthetic Lawyer DRI");
    store::external_identities::link(
        surreal,
        person.id,
        store::external_identities::ExternalSystem::Slack,
        "U_SYNTHETIC",
        None,
    )
    .await
    .expect("link synthetic Slack identity");
    project.id
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One crash/restart scenario keeps the DBOS evidence together.
async fn registered_nightly_parent_resumes_the_same_date_and_deduplicates_notices() {
    let Some(db) = database().await else { return };
    let surreal = mem_surreal().await;
    store::schema::apply(&surreal)
        .await
        .expect("apply synthetic Surreal schema");
    let _project = seed_project(&surreal).await;
    let slack = Arc::new(AcceptInviteThenHold::holding_invites());
    let context = Context::new(
        surreal.clone(),
        slack.clone(),
        "synthetic-messages".into(),
        true,
        Providers {
            github_forge: None,
            repository_forge: RepositoryForge::None,
            runtime_version: "26.10.22".into(),
            portal_origin: None,
        },
    );
    let id = nightly::workflow_id("2026-10-22");

    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow = nightly::register(&first, context.clone()).expect("register DBOS parent");
    first.launch().await.expect("launch first executor");
    let router = workflows_service::health::router_with_dbos_ingress(WorkerIngress {
        nightly_project_update: Some(NightlyIngress::new(
            "synthetic-service-token".into(),
            first_workflow,
        )),
        ..Default::default()
    });
    let body = serde_json::json!({ "workflow_id": id.clone() });
    let denied = router
        .clone()
        .oneshot(
            Request::post("/internal/dbos/nightly-project-update")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(
                Request::post("/internal/dbos/nightly-project-update")
                    .header("authorization", "Bearer synthetic-service-token")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    tokio::time::timeout(DEADLINE, slack.accepted.notified())
        .await
        .expect("synthetic Slack invite accepted before the worker loses its reply");
    assert_eq!(slack.members.lock().await.as_slice(), ["U_SYNTHETIC"]);
    first.shutdown().await;

    let restarted = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let restarted_workflow =
        nightly::register(&restarted, context).expect("register recovered DBOS parent");
    restarted.launch().await.expect("launch restarted executor");
    restarted_workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .expect("reattach same daily run");
    let resumed_report = tokio::time::timeout(
        DEADLINE,
        restarted
            .retrieve_workflow::<NightlyProjectUpdateReport, NightlyError>(&id)
            .expect("retrieve recovered DBOS parent")
            .result(),
    )
    .await
    .expect("recovered DBOS parent finishes")
    .expect("recovered DBOS parent succeeds");
    restarted.shutdown().await;

    assert_eq!(
        resumed_report.invited, 0,
        "replay observes the accepted provider state"
    );
    // Replayed reconciliation may call Slack with an empty delta, but never
    // invites the already-accepted identity a second time.
    assert_eq!(slack.invite_calls.load(Ordering::SeqCst), 2);
    assert_eq!(slack.invited_users.lock().await.as_slice(), ["U_SYNTHETIC"]);
    assert_eq!(
        slack.inner.posted_messages().len(),
        3,
        "start, Project, and outcome notices each have one durable receipt"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end registered child crash/restart scenario.
async fn registered_slack_child_reuses_receipt_after_acceptance_before_checkpoint() {
    let Some(db) = database().await else { return };
    let surreal = mem_surreal().await;
    store::schema::apply(&surreal)
        .await
        .expect("apply synthetic Surreal schema");
    let _project_id = seed_project(&surreal).await;
    let slack = Arc::new(AcceptInviteThenHold::holding_project_notices());
    let context = Context::new(
        surreal,
        slack.clone(),
        "synthetic-messages".into(),
        true,
        Providers {
            github_forge: None,
            repository_forge: RepositoryForge::None,
            runtime_version: "26.10.22".into(),
            portal_origin: None,
        },
    );
    let ran_on = "2026-10-23";
    let id = nightly::workflow_id(ran_on);

    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow =
        nightly::register(&first, context.clone()).expect("register DBOS workflows");
    first.launch().await.expect("launch first executor");
    first_workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .expect("start Nightly parent and its Slack child");
    tokio::time::timeout(DEADLINE, slack.accepted.notified())
        .await
        .expect("Slack accepted the daily Project notice");
    assert_eq!(slack.inner.posted_messages().len(), 2);
    first.shutdown().await;

    let restarted = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = nightly::register(&restarted, context).expect("register recovered workflows");
    restarted.launch().await.expect("launch restarted executor");
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .expect("reattach same Nightly parent");
    let result = tokio::time::timeout(
        DEADLINE,
        restarted
            .retrieve_workflow::<NightlyProjectUpdateReport, NightlyError>(&id)
            .expect("retrieve recovered Nightly parent")
            .result(),
    )
    .await
    .expect("recovered Nightly parent finishes")
    .expect("recovered Nightly parent succeeds");
    restarted.shutdown().await;
    assert_eq!(result.notices_posted, 1);
    let messages = slack.inner.posted_messages();
    assert_eq!(messages.len(), 3, "start, Project, and completion notices");
    assert_eq!(
        messages
            .iter()
            .filter(|(channel, _)| channel == "C0123456789")
            .count(),
        1
    );
}
