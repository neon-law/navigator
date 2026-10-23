//! Manual queueing for the Cron schedules lawyer page.
//!
//! Every scheduled workflow is started through this module so the operations
//! page, rather than the lawyer dashboard, is the one place to run it on
//! demand. A manual invocation always uses a unique key: an operator's
//! explicit run must not be deduplicated against the schedule's date key.

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

/// `POST /app/admin/schedules/:job/run` — queue one known CronJob workflow.
pub async fn run(Path(slug): Path<String>) -> Response {
    let broker = restate_broker_url();
    let token = std::env::var("RESTATE_AUTH_TOKEN").ok();
    let dbos_ingress = dbos_ingress_url();
    let dbos_token = std::env::var("NAVIGATOR_DBOS_INGRESS_TOKEN").ok();
    let reconcile_dbos_flag = std::env::var("NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED").ok();
    let Ok(reconcile_dbos_enabled) = workflows::dbos_engine::flag_enabled(
        "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED",
        reconcile_dbos_flag.as_deref(),
    ) else {
        return redirect_with_notice("reconcile-invoices", "not_queued");
    };
    run_configured_with_dbos(
        &slug,
        broker.as_deref(),
        token.as_deref(),
        dbos_ingress.as_deref(),
        dbos_token.as_deref(),
        reconcile_dbos_enabled,
    )
    .await
}

#[cfg(test)]
async fn run_configured(slug: &str, broker: Option<&str>, token: Option<&str>) -> Response {
    run_configured_with_dbos(slug, broker, token, None, None, false).await
}

async fn run_configured_with_dbos(
    slug: &str,
    broker: Option<&str>,
    token: Option<&str>,
    dbos_ingress: Option<&str>,
    dbos_token: Option<&str>,
    reconcile_dbos_enabled: bool,
) -> Response {
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run_key = format!("manual-{}", Uuid::new_v4());
    let queued = if job.service == "ReconcileInvoices" {
        if reconcile_dbos_enabled {
            if let Some(ingress) = dbos_ingress {
                match dbos_token.filter(|token| !token.trim().is_empty()) {
                    Some(dbos_token) => trigger_dbos(ingress, dbos_token, job, &run_key).await,
                    None => Err(workflows::TriggerError::Transport),
                }
            } else {
                Err(workflows::TriggerError::Transport)
            }
        } else if let Some(broker) = broker {
            trigger(broker, token, job, &run_key).await.map(|_| ())
        } else {
            Err(workflows::TriggerError::Transport)
        }
    } else if let Some(broker) = broker {
        trigger(broker, token, job, &run_key).await.map(|_| ())
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

async fn trigger_dbos(
    ingress: &str,
    token: &str,
    job: &ManualJob,
    run_key: &str,
) -> Result<(), workflows::TriggerError> {
    workflows::trigger::start_dbos_reconcile(
        ingress,
        token,
        &serde_json::json!({
            "workflow_id": format!("{}:{run_key}", job.service)
        }),
    )
    .await
}

fn redirect_with_notice(slug: &str, outcome: &str) -> Response {
    Redirect::to(&format!("/app/admin/schedules?notice={outcome}:{slug}")).into_response()
}

async fn trigger(
    broker: &str,
    token: Option<&str>,
    job: &ManualJob,
    run_key: &str,
) -> Result<String, workflows::TriggerError> {
    workflows::start_workflow(
        broker,
        token,
        job.service,
        run_key,
        "run",
        &serde_json::json!({}),
        true,
    )
    .await
}

/// The Restate ingress base URL from `RESTATE_BROKER_URL`, trimmed, or `None`
/// when unset/empty (dev, local, and tests).
pub(crate) fn restate_broker_url() -> Option<String> {
    normalize_broker_url(std::env::var("RESTATE_BROKER_URL").ok())
}

fn dbos_ingress_url() -> Option<String> {
    normalize_broker_url(std::env::var("NAVIGATOR_DBOS_INGRESS_URL").ok())
}

fn normalize_broker_url(url: Option<String>) -> Option<String> {
    url.map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use axum::{
        extract::Path,
        http::{header::LOCATION, StatusCode},
    };

    use super::{
        normalize_broker_url, run, run_configured, run_configured_with_dbos, trigger, MANUAL_JOBS,
    };
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

    #[test]
    fn every_manual_control_maps_to_a_known_workflow() {
        assert_eq!(MANUAL_JOBS.len(), 5);
        assert!(MANUAL_JOBS
            .iter()
            .all(|job| !job.slug.is_empty() && !job.service.is_empty()));
    }

    #[tokio::test]
    async fn trigger_queues_the_selected_workflow_with_a_manual_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/BillingCanary/manual-[0-9a-f-]+/run/send$"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("{\"invocationId\":\"inv_x\"}"),
            )
            .mount(&server)
            .await;
        let job = MANUAL_JOBS
            .iter()
            .find(|job| job.slug == "billing-canary")
            .unwrap();
        let body = trigger(
            &server.uri(),
            None,
            job,
            "manual-4dc99f6d-55a4-4408-b8a0-0df5a8437f6e",
        )
        .await
        .expect("ingress accepts invocation");
        assert!(body.contains("inv_x"));
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
        let response = run_configured_with_dbos(
            "reconcile-invoices",
            Some("http://unused-restate"),
            Some("legacy-token"),
            Some(&server.uri()),
            Some("dbos-secret"),
            true,
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
        let response = run_configured_with_dbos(
            "reconcile-invoices",
            Some(&server.uri()),
            Some("legacy-token"),
            None,
            None,
            true,
        )
        .await;
        server.verify().await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).ends_with("notice=not_queued:reconcile-invoices"));
    }

    #[tokio::test]
    async fn reconcile_ingress_url_alone_does_not_select_dbos() {
        let restate = MockServer::start().await;
        let dbos = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/ReconcileInvoices/manual-[^/]+/run/send$"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&restate)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&dbos)
            .await;
        let response = run_configured_with_dbos(
            "reconcile-invoices",
            Some(&restate.uri()),
            None,
            Some(&dbos.uri()),
            Some("dbos-secret"),
            false,
        )
        .await;
        restate.verify().await;
        dbos.verify().await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).ends_with("notice=queued:reconcile-invoices"));
    }

    #[tokio::test]
    async fn run_covers_unknown_unavailable_accepted_and_rejected_jobs() {
        let response = run(Path("unknown".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = run_configured("archives", None, None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            location(&response),
            "/app/admin/schedules?notice=not_queued:archives"
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/BillingCanary/manual-[0-9a-f-]+/run/send$"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("accepted"))
            .expect(1)
            .mount(&server)
            .await;
        let broker =
            normalize_broker_url(Some(format!("{}/", server.uri()))).expect("non-empty broker URL");

        let response = run_configured("billing-canary", Some(&broker), Some("secret")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            location(&response),
            "/app/admin/schedules?notice=queued:billing-canary"
        );

        let rejecting_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .expect(1)
            .mount(&rejecting_server)
            .await;

        let response = run_configured("billing-digest", Some(&rejecting_server.uri()), None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            location(&response),
            "/app/admin/schedules?notice=not_queued:billing-digest"
        );

        assert_eq!(
            normalize_broker_url(Some("https://restate.example.com////".to_string())).as_deref(),
            Some("https://restate.example.com")
        );
        assert!(normalize_broker_url(Some("///".to_string())).is_none());
        assert!(normalize_broker_url(None).is_none());
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
}
