//! Manual controls join the same typed DBOS workflow on a repeated request.

use crate::support::{database, DEADLINE};
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
async fn manual_schedule_retries_join_and_distinct_clicks_run_separately() {
    let Some(database) = database().await else {
        return;
    };
    let contacts = Effects::default();
    let notices = Effects::default();
    let executor = Executor::launch_hosting(
        &Settings::from_lookup(database.lookup()).unwrap(),
        Arc::new(CapturingNotifier::new()),
        Schedules {
            billing_canary: Some(billing_canary::Context::new(
                Arc::new(contacts.clone()),
                Arc::new(notices.clone()),
            )),
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
                archives: None,
            }),
            ..Default::default()
        },
    );
    for _ in 0..2 {
        let key = format!("manual-{}", uuid::Uuid::new_v4());
        let id = format!("BillingCanary:{key}");
        let (first, retry) = tokio::join!(post(router.clone(), &key), post(router.clone(), &key));
        assert_eq!(first, StatusCode::ACCEPTED);
        assert_eq!(retry, StatusCode::ACCEPTED);
        tokio::time::timeout(DEADLINE, async {
            loop {
                let status: Option<String> = sqlx::query_scalar(
                    "SELECT status FROM dbos.workflow_status WHERE workflow_uuid=$1",
                )
                .bind(&id)
                .fetch_optional(&database.pool)
                .await
                .unwrap();
                if status.as_deref() == Some("SUCCESS") {
                    break;
                }
                assert_ne!(status.as_deref(), Some("ERROR"));
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            contacts.count(&id),
            2,
            "stable contact is resolved twice in one canary step"
        );
        assert_eq!(
            notices.count(&id),
            1,
            "a repeated start joins the first notification"
        );
    }
    executor.shutdown().await;
    database.pool.close().await;
}
