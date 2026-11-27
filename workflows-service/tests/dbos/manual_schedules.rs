//! Retired billing controls cannot execute even with a retained native handle.

use crate::support::database;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use billing::{ContactId, ContactRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use workflows::billing_canary::{BillingCanaryProvider, CanaryProviderError};
use workflows::dbos_engine::{self, billing_canary, Executor, Schedules, Settings};
use workflows::{CapturingNotifier, EmailError, EmailService, OutboundEmail, SendReceipt};
use workflows_service::dbos_ingress::{add_routes, ManualIngress, WorkerIngress};

#[derive(Clone, Default)]
struct Effects(Arc<Mutex<HashMap<String, usize>>>);
impl Effects {
    fn record(&self) {
        let key = dbos_engine::effect_key().expect("provider is called inside a durable step");
        *self.0.lock().unwrap().entry(key).or_default() += 1;
    }
    fn count(&self, id: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.starts_with(&format!("{id}/")))
            .map(|(_, count)| count)
            .sum()
    }
}
#[async_trait::async_trait]
impl BillingCanaryProvider for Effects {
    async fn ensure_contact(&self, _: &ContactRequest) -> Result<ContactId, CanaryProviderError> {
        self.record();
        Ok(ContactId("synthetic-contact".into()))
    }
}
#[async_trait::async_trait]
impl EmailService for Effects {
    async fn send(&self, _: OutboundEmail) -> Result<SendReceipt, EmailError> {
        self.record();
        Ok(SendReceipt { message_id: None })
    }
}
async fn post(router: Router, key: &str) -> StatusCode {
    router
        .oneshot(
            Request::post("/internal/dbos/schedules")
                .header("authorization", "Bearer synthetic-manual-token")
                .body(Body::from(
                    serde_json::json!({"service":"BillingCanary", "run_key":key}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn retired_manual_billing_cannot_execute_a_retained_native_handle() {
    let Some(database) = database().await else {
        return;
    };
    let contacts = Effects::default();
    let notices = Effects::default();
    let executor = Executor::launch_hosting(
        &Settings::from_lookup(database.lookup()).unwrap(),
        Arc::new(CapturingNotifier::new()),
        Schedules {
            billing_canary: Some(
                billing_canary::Context::new(Arc::new(contacts.clone()), Arc::new(notices.clone()))
                    .with_schedule_enabled(false),
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let router = add_routes(
        Router::new(),
        WorkerIngress {
            manual_schedules: Some(ManualIngress {
                token: "synthetic-manual-token".into(),
                builtin: executor.manual_schedules(),
            }),
            ..Default::default()
        },
    );
    for _ in 0..2 {
        let key = format!("manual-{}", uuid::Uuid::new_v4());
        let id = format!("BillingCanary:{key}");
        let (first, retry) = tokio::join!(post(router.clone(), &key), post(router.clone(), &key));
        assert_eq!(first, StatusCode::GONE);
        assert_eq!(retry, StatusCode::GONE);
        let retained: i64 =
            sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid=$1")
                .bind(&id)
                .fetch_one(database.store().pool())
                .await
                .unwrap();
        assert_eq!(retained, 0);
        assert_eq!(contacts.count(&id), 0);
        assert_eq!(notices.count(&id), 0);
    }
    let automatic: i64 = sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE name='BillingCanary' AND workflow_uuid LIKE 'BillingCanary-%'")
        .fetch_one(database.store().pool()).await.unwrap();
    assert_eq!(automatic, 0, "retired billing starts no automatic workflow");
    executor.shutdown().await;
    database.store().pool().close().await;
}
