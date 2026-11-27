//! Manual starts from the Cron schedules operations page.
//!
//! Each control submits to DBOS. A manual
//! invocation uses a unique key so an explicit run stays distinct from a scheduled slot.

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Extension,
};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

/// The Archives handle registered by this process before its executor launches.
#[derive(Clone, Default)]
pub(crate) struct LocalArchives(Arc<OnceLock<archives::workflow::ArchivesDbosWorkflow>>);

impl LocalArchives {
    pub(crate) fn register(
        &self,
        workflow: archives::workflow::ArchivesDbosWorkflow,
    ) -> dbos::Result<()> {
        self.0.set(workflow).map_err(|_| {
            dbos::Error::Config("Archives already registered for manual starts".into())
        })
    }

    async fn start(&self, run_key: &str) -> Result<(), workflows::TriggerError> {
        let workflow = self.0.get().ok_or(workflows::TriggerError::Transport)?;
        let request = archives::workflow::RunRequest::configured(None)
            .map_err(|_| workflows::TriggerError::Transport)?;
        let workflow_id = format!("Archives:{run_key}");
        workflow
            .start_with(
                request,
                dbos::StartOptions {
                    workflow_id: Some(&workflow_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| workflows::TriggerError::Transport)?;
        Ok(())
    }
}

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

/// `POST /app/admin/schedules/:job/run` — queue one configured scheduled workflow.
pub(crate) async fn run(
    Path(slug): Path<String>,
    local: Option<Extension<LocalArchives>>,
) -> Response {
    let ingress = workflows::trigger::dbos_ingress_from_env().ok();
    run_configured(
        &slug,
        ingress
            .as_ref()
            .map(|(url, token)| (url.as_str(), token.as_str())),
        local.as_ref().map(|Extension(local)| local),
    )
    .await
}

async fn run_configured(
    slug: &str,
    ingress: Option<(&str, &str)>,
    local: Option<&LocalArchives>,
) -> Response {
    let Some(job) = MANUAL_JOBS.iter().find(|job| job.slug == slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run_key = format!("manual-{}", Uuid::new_v4());
    let queued = if let Some(local) = local.filter(|_| job.service == "Archives") {
        local.start(&run_key).await
    } else if let Some((url, token)) = ingress {
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
    use sqlx::ConnectOptions as _;
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
    async fn local_archives_starts_the_registered_sdk_workflow_once() {
        store::test_support::with_postgres(async {
            use futures::FutureExt as _;
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let url = pg.pool().connect_options().to_url_lossy().to_string();
            let runtime = dbos::DBOS::new(workflows::dbos_engine::config(&url, "manual-archives-test"));
            let starts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = starts.clone();
            let workflow = runtime.register_workflow("Archives", move |request: archives::workflow::RunRequest| {
                let counted = counted.clone();
                async move {
                    assert!(request.scheduled_for.is_none());
                    assert!(request.write_only.is_none());
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<_, dbos::Error<archives::workflow::ArchivesDbosError>>(archives::workflow::RunReport {
                        run_date: chrono::Utc::now().date_naive(),
                        invocation_id: dbos::workflow_id().unwrap(),
                        tables: 0, rows: 0, failures: 0, write_only: None,
                    })
                }
            }).unwrap();
            let local = super::LocalArchives::default();
            local.register(workflow).unwrap();
            runtime.launch().await.unwrap();
            let result = std::panic::AssertUnwindSafe(async {
            let key = format!("manual-{}", uuid::Uuid::new_v4());
            local.start(&key).await.unwrap();
            local.start(&key).await.unwrap();
            let id = format!("Archives:{key}");
            let handle = runtime.retrieve_workflow::<archives::workflow::RunReport, archives::workflow::ArchivesDbosError>(&id).unwrap();
            let report = tokio::time::timeout(std::time::Duration::from_secs(20), handle.result()).await.unwrap().unwrap();
            assert_eq!(report.invocation_id, id);
            assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);
            }).catch_unwind().await;
            runtime.shutdown().await;
            if let Err(panic) = result { std::panic::resume_unwind(panic); }
        }).await;
    }

    #[tokio::test]
    async fn local_archive_owner_never_uses_the_remote_ingress() {
        let remote = MockServer::start().await;
        let local = super::LocalArchives::default();
        let response = run_configured(
            "archives",
            Some((&remote.uri(), "synthetic-token")),
            Some(&local),
        )
        .await;
        assert!(location(&response).contains("notice=not_queued:archives"));
        assert!(remote.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn retired_manual_billing_controls_never_submit() {
        let server = MockServer::start().await;
        for slug in ["billing-canary", "billing-digest", "reconcile-invoices"] {
            let response =
                run_configured(slug, Some((&server.uri(), "synthetic-token")), None).await;
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
            let response =
                run_configured(job.slug, Some((&dbos.uri(), "synthetic-token")), None).await;
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
        let response = run(Path("unknown".to_string()), None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = run_configured("notation-reset", None, None).await;
        assert!(location(&response).contains("notice=not_queued:notation-reset"));

        let dbos = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&dbos)
            .await;
        let response = run_configured(
            "notation-reset",
            Some((&dbos.uri(), "synthetic-token")),
            None,
        )
        .await;
        assert!(location(&response).contains("notice=not_queued:notation-reset"));
    }
}
