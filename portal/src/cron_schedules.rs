//! Manual DBOS starts from the Cron schedules operations page.
//!
//! Manual identity stays distinct from scheduled slots; timer policy never changes the backend.

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
        slug: "archives",
        service: "Archives",
    },
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

/// `POST /app/admin/schedules/:job/run` — queue one native scheduled workflow.
pub async fn run(Path(slug): Path<String>) -> Response {
    let ingress = std::env::var(workflows::trigger::DBOS_INGRESS_URL_ENV).ok();
    let token = std::env::var(workflows::trigger::DBOS_INGRESS_TOKEN_ENV).ok();
    run_configured(&slug, ingress.as_deref(), token.as_deref()).await
}

async fn run_configured(slug: &str, ingress: Option<&str>, token: Option<&str>) -> Response {
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run_key = format!("manual-{}", Uuid::new_v4());
    let queued = match workflows::trigger::service_target(
        ingress.map(str::to_owned),
        token.map(str::to_owned),
    ) {
        Ok(workflows::trigger::ServiceTarget::Dbos { ingress, token }) => {
            trigger_dbos(&ingress, &token, job, &run_key).await
        }
        Ok(workflows::trigger::ServiceTarget::Restate) | Err(_) => {
            Err(workflows::TriggerError::Transport)
        }
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
            tracing::error!(job=job.slug, service=job.service, run_key, %error, "manual cron job was not queued");
            redirect_with_notice(job.slug, "not_queued")
        }
    }
}

async fn trigger_dbos(
    ingress: &str,
    token: &str,
    job: &ManualJob,
    run_key: &str,
) -> Result<(), workflows::TriggerError> {
    if job.service == "ReconcileInvoices" {
        workflows::trigger::start_dbos_reconcile(
            ingress,
            token,
            &serde_json::json!({"workflow_id": format!("{}:{run_key}", job.service)}),
        )
        .await
    } else {
        workflows::trigger::start_dbos_schedule(ingress, token, job.service, run_key).await
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
    async fn every_other_manual_schedule_uses_dbos_without_a_restate_request() {
        for job in MANUAL_JOBS
            .iter()
            .filter(|job| job.service != "ReconcileInvoices")
        {
            let dbos = MockServer::start().await;
            let restate = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path_regex(r"^/internal/dbos/schedules$"))
                .and(header("authorization", "Bearer synthetic-dbos-token"))
                .and(body_partial_json(
                    serde_json::json!({"service": job.service}),
                ))
                .respond_with(ResponseTemplate::new(202))
                .expect(1)
                .mount(&dbos)
                .await;
            let response =
                run_configured(job.slug, Some(&dbos.uri()), Some("synthetic-dbos-token")).await;
            assert!(location(&response).contains("notice=queued:"));
            assert!(restate.received_requests().await.unwrap().is_empty());
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
    async fn selected_manual_dbos_never_falls_back_when_unconfigured_or_rejected() {
        for job in MANUAL_JOBS {
            let restate = MockServer::start().await;
            let dbos = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(503))
                .mount(&dbos)
                .await;
            for (url, token) in [
                (None, Some("synthetic-token")),
                (Some(dbos.uri()), None),
                (Some(dbos.uri()), Some("synthetic-token")),
            ] {
                let response = run_configured(job.slug, url.as_deref(), token).await;
                assert!(location(&response).contains("notice=not_queued:"));
            }
            assert!(restate.received_requests().await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn reconcile_manual_control_uses_authenticated_dbos_and_its_namespaced_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/internal/dbos/reconcile-invoices$"))
            .and(header("authorization", "Bearer dbos-secret"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        let response = run_configured(
            "reconcile-invoices",
            Some(&server.uri()),
            Some("dbos-secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).ends_with("notice=queued:reconcile-invoices"));
    }

    #[tokio::test]
    async fn enabled_reconcile_dbos_fails_closed_without_ingress() {
        use wiremock::{matchers::method, Mock, MockServer};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&server)
            .await;
        let response = run_configured("reconcile-invoices", None, None).await;
        server.verify().await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).ends_with("notice=not_queued:reconcile-invoices"));
    }

    #[test]
    fn every_run_now_button_the_console_renders_has_a_handler_here() {
        // `webapp::schedules` derives its rows from the deployment render, so a
        // manifest landing in the exports kustomization puts a job on the page
        // without touching this file. A row carrying a slug this dispatch table
        // does not know would render a button that answers 404, so bind the two
        // rather than trusting them to be edited together.
        for job in webapp::schedules::cron_jobs() {
            let Some(slug) = job.slug else { continue };
            assert!(
                MANUAL_JOBS.iter().any(|manual| manual.slug == slug),
                "the console offers `Run now` for {} (slug {slug}), which no MANUAL_JOBS entry starts",
                job.name
            );
        }
    }
    #[tokio::test]
    async fn unknown_and_unconfigured_manual_jobs_fail_without_a_backend_switch() {
        assert_eq!(
            run(Path("unknown".into())).await.status(),
            StatusCode::NOT_FOUND
        );
        let response = run_configured("archives", None, None).await;
        assert!(location(&response).ends_with("notice=not_queued:archives"));
    }
}
