//! Manual starts from the Cron schedules operations page.
//!
//! Each control selects the workflow's configured durable engine. A manual
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
    dbos_flag: &'static str,
}

const MANUAL_JOBS: &[ManualJob] = &[
    ManualJob {
        slug: "archives",
        service: "Archives",
        dbos_flag: "NAVIGATOR_DBOS_ARCHIVES_ENABLED",
    },
    ManualJob {
        slug: "billing-canary",
        service: "BillingCanary",
        dbos_flag: "NAVIGATOR_DBOS_BILLING_CANARY_ENABLED",
    },
    ManualJob {
        slug: "billing-digest",
        service: "BillingDigest",
        dbos_flag: "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED",
    },
    ManualJob {
        slug: "notation-reset",
        service: "NotationReset",
        dbos_flag: "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED",
    },
    ManualJob {
        slug: "reconcile-invoices",
        service: "ReconcileInvoices",
        dbos_flag: "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED",
    },
];

/// `POST /app/admin/schedules/:job/run` — queue one configured scheduled workflow.
pub async fn run(Path(slug): Path<String>) -> Response {
    let broker = restate_broker_url();
    let token = std::env::var("RESTATE_AUTH_TOKEN").ok();
    let dbos_ingress = dbos_ingress_url();
    let dbos_token = std::env::var("NAVIGATOR_DBOS_INGRESS_TOKEN").ok();
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(dbos_enabled) = workflows::dbos_engine::flag_enabled(
        job.dbos_flag,
        std::env::var(job.dbos_flag).ok().as_deref(),
    ) else {
        return redirect_with_notice(job.slug, "not_queued");
    };
    run_configured_with_dbos(
        &slug,
        broker.as_deref(),
        token.as_deref(),
        dbos_ingress.as_deref(),
        dbos_token.as_deref(),
        dbos_enabled,
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
    dbos_enabled: bool,
) -> Response {
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run_key = format!("manual-{}", Uuid::new_v4());
    let queued = if dbos_enabled {
        match (
            dbos_ingress.filter(|url| !url.trim().is_empty()),
            dbos_token.filter(|token| !token.trim().is_empty()),
        ) {
            (Some(ingress), Some(token)) => trigger_dbos(ingress, token, job, &run_key).await,
            _ => Err(workflows::TriggerError::Transport),
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
    if job.service == "ReconcileInvoices" {
        workflows::trigger::start_dbos_reconcile(
            ingress,
            token,
            &serde_json::json!({ "workflow_id": format!("{}:{run_key}", job.service) }),
        )
        .await
    } else {
        workflows::trigger::start_dbos_schedule(ingress, token, job.service, run_key).await
    }
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
            let response = run_configured_with_dbos(
                job.slug,
                Some(&restate.uri()),
                None,
                Some(&dbos.uri()),
                Some("synthetic-dbos-token"),
                true,
            )
            .await;
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
                let response = run_configured_with_dbos(
                    job.slug,
                    Some(&restate.uri()),
                    None,
                    url.as_deref(),
                    token,
                    true,
                )
                .await;
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
