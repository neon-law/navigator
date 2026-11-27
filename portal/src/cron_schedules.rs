//! Manual starts from the Cron schedules operations page.
//!
//! Each control submits to DBOS. A manual
//! invocation uses a unique key so an explicit run stays distinct from a scheduled slot.

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use uuid::Uuid;

struct ManualJob {
    slug: &'static str,
    service: &'static str,
}

const MANUAL_JOBS: &[ManualJob] = &[
    ManualJob {
        slug: "billing-canary",
        service: "BillingCanary",
    },
    ManualJob {
        slug: "billing-digest",
        service: "BillingDigest",
    },
    ManualJob {
        slug: "notation-reset",
        service: "NotationReset",
    },
    ManualJob {
        slug: "reconcile-invoices",
        service: "ReconcileInvoices",
    },
];

/// `POST /app/admin/schedules/:job/run` — queue one configured scheduled workflow.
pub(crate) async fn run(Path(slug): Path<String>) -> Response {
    let ingress = workflows::trigger::dbos_ingress_from_env().ok();
    run_configured(
        &slug,
        ingress
            .as_ref()
            .map(|(url, token)| (url.as_str(), token.as_str())),
    )
    .await
}

async fn run_configured(slug: &str, ingress: Option<(&str, &str)>) -> Response {
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run_key = format!("manual-{}", Uuid::new_v4());
    let queued = if let Some((url, token)) = ingress {
        workflows::trigger::start_dbos_schedule(url, token, job.service, &run_key).await
    } else {
        Err(workflows::TriggerError::Transport)
    };
    match queued {
        Ok(()) => {
            tracing::info!(
                job = job.slug,
                service = job.service,
                run_key,
                "manual cron job queued"
            );
            redirect_with_notice(job.slug, "queued")
        }
        Err(error) => {
            tracing::error!(job = job.slug, service = job.service, run_key, %error, "manual cron job was not queued");
            redirect_with_notice(job.slug, "not_queued")
        }
    }
}

fn redirect_with_notice(slug: &str, outcome: &str) -> Response {
    Redirect::to(&format!("/app/admin/schedules?notice={outcome}:{slug}")).into_response()
}

#[cfg(test)]
mod tests {
    use axum::{
        extract::Path,
        http::{header::LOCATION, StatusCode},
    };

    use super::{run, run_configured, MANUAL_JOBS};
    use wiremock::{
        matchers::{body_partial_json, header, method, path_regex},
        Mock, MockServer, ResponseTemplate,
    };

    fn location(response: &axum::response::Response) -> &str {
        response
            .headers()
            .get(LOCATION)
            .expect("redirect location")
            .to_str()
            .expect("ASCII redirect location")
    }

    #[tokio::test]
    async fn retired_manual_billing_controls_never_submit() {
        let server = MockServer::start().await;
        for slug in ["billing-canary", "billing-digest", "reconcile-invoices"] {
            let response = run_configured(slug, Some((&server.uri(), "synthetic-token"))).await;
            assert!(location(&response).contains("notice=not_queued:"));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn manual_schedules_use_authenticated_dbos_with_a_unique_run_key() {
        for job in MANUAL_JOBS
            .iter()
            .filter(|job| !store::workflow_engine::disabled_billing_service(job.service))
        {
            let dbos = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path_regex(r"^/internal/dbos/schedules$"))
                .and(header("authorization", "Bearer synthetic-token"))
                .and(body_partial_json(
                    serde_json::json!({"service": job.service}),
                ))
                .respond_with(ResponseTemplate::new(202))
                .expect(1)
                .mount(&dbos)
                .await;
            let response = run_configured(job.slug, Some((&dbos.uri(), "synthetic-token"))).await;
            assert!(location(&response).contains("notice=queued:"));
            let requests = dbos.received_requests().await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            let key = body["run_key"]
                .as_str()
                .unwrap()
                .strip_prefix("manual-")
                .unwrap();
            uuid::Uuid::parse_str(key).unwrap();
        }
    }

    #[tokio::test]
    async fn run_reports_unknown_unconfigured_and_rejected_jobs() {
        let response = run(Path("unknown".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = run_configured("notation-reset", None).await;
        assert!(location(&response).contains("notice=not_queued:notation-reset"));

        let dbos = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&dbos)
            .await;
        let response =
            run_configured("notation-reset", Some((&dbos.uri(), "synthetic-token"))).await;
        assert!(location(&response).contains("notice=not_queued:notation-reset"));
    }
}
