//! Always-on, unsigned liveness/readiness probe for the worker (ENG-551).
//!
//! The Restate SDK's endpoint on `:9080` requires a valid Restate Cloud
//! signature on every request once `RESTATE_IDENTITY_KEY` is set — kubelet
//! and the GCE LB can never produce one — and Envoy answers
//! `GET /restate/health` with a static `200` instead of proxying to the
//! worker (ENG-509, and rightly so: do not change that). Neither path ever
//! exercises the worker's own request-handling code, which is exactly how
//! ENG-550's `CryptoProvider` panic shipped invisibly. This listener is a
//! second, separate Axum server (distinct from both the Restate endpoint and
//! the GitHub webhook receiver) whose one route calls
//! [`crate::request_identity::crypto_provider_is_healthy`], round-tripping a
//! throwaway signature through the same verification path real traffic
//! uses, so a future regression there fails the probe instead of shipping
//! silently again.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::request_identity::crypto_provider_is_healthy;

/// Upper bound for one readiness system-database probe. The route must answer
/// inside kubelet's probe timeout even while a system database hangs.
const READY_DB_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness inputs for the DBOS executor behind [`readyz`], built in `main`
/// from the live executor's probe handle.
#[derive(Clone)]
pub struct DbosReadiness {
    /// Live running-check: true while the executor is launched and no
    /// shutdown is in flight.
    pub is_running: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The executor's system database URL, when the executor has one.
    pub system_database_url: Option<Arc<str>>,
}

/// Distinct from both the Restate endpoint's `:9080` and the GitHub webhook
/// receiver's `:9082`, so kubelet can reach this unconditionally on every
/// deployment (the webhook receiver binds only on the automation home).
const DEFAULT_HEALTH_LISTEN: &str = "0.0.0.0:9083";

pub fn router() -> Router {
    Router::new().route("/healthz", get(healthz))
}

pub fn router_with_dbos_ingress(
    ingress: crate::dbos_ingress::WorkerIngress,
    dbos: Option<DbosReadiness>,
) -> Router {
    let router = Router::new()
        .route("/readyz", get(readyz))
        .with_state(dbos.map(Arc::new));
    crate::dbos_ingress::add_routes(router, ingress)
}

async fn healthz() -> StatusCode {
    if crypto_provider_is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Readiness: everything liveness checks plus the DBOS executor's health —
/// the executor reports itself running and its system database is reachable.
/// The route is unsigned, so it never echoes probe error detail (details go
/// to the log); a disabled executor reads as ready rather than failing pods
/// that legitimately run the Restate engine only.
async fn readyz(State(dbos): State<Option<Arc<DbosReadiness>>>) -> axum::response::Response {
    if !crypto_provider_is_healthy() {
        return readiness_response(StatusCode::INTERNAL_SERVER_ERROR, "unhealthy", "unknown");
    }
    let Some(readiness) = dbos.as_deref() else {
        return readiness_response(StatusCode::OK, "ok", "disabled");
    };
    if !(readiness.is_running)() {
        return readiness_response(StatusCode::SERVICE_UNAVAILABLE, "ok", "stopped");
    }
    let Some(url) = readiness.system_database_url.as_deref() else {
        return readiness_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "ok",
            "system-database-unconfigured",
        );
    };
    match tokio::time::timeout(READY_DB_TIMEOUT, system_database_ping(url)).await {
        Ok(Ok(())) => readiness_response(StatusCode::OK, "ok", "ok"),
        Ok(Err(unreachable)) => {
            tracing::error!(?unreachable, "readiness probe: system database unreachable");
            readiness_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "ok",
                "system-database-unreachable",
            )
        }
        Err(_) => {
            tracing::error!("readiness probe: system database ping timed out");
            readiness_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "ok",
                "system-database-unreachable",
            )
        }
    }
}

fn readiness_response(status: StatusCode, crypto: &str, dbos: &str) -> axum::response::Response {
    (status, Json(json!({ "crypto": crypto, "dbos": dbos }))).into_response()
}

/// Probe the executor's system database with one cheap connection. The error
/// carries no detail on purpose: the route is unsigned, so nothing request-
/// specific is ever echoed back.
async fn system_database_ping(url: &str) -> Result<(), SystemDatabaseUnreachable> {
    let options: sqlx::postgres::PgConnectOptions =
        url.parse().map_err(|_| SystemDatabaseUnreachable)?;
    let db = store::postgres::PostgresDb::connect(options)
        .await
        .map_err(|_| SystemDatabaseUnreachable)?;
    db.ping().await.map_err(|_| SystemDatabaseUnreachable)
}

/// The system database probe failed; detail is logged, never echoed.
#[derive(Debug)]
struct SystemDatabaseUnreachable;

/// Resolve the probe's listen address from `WORKFLOWS_SERVICE_HEALTH_LISTEN`,
/// defaulting to [`DEFAULT_HEALTH_LISTEN`].
///
/// # Errors
///
/// Returns an error when the configured value is not a valid socket address.
pub fn health_listen_addr(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<SocketAddr> {
    get("WORKFLOWS_SERVICE_HEALTH_LISTEN")
        .unwrap_or_else(|| DEFAULT_HEALTH_LISTEN.to_owned())
        .parse()
        .context("parse WORKFLOWS_SERVICE_HEALTH_LISTEN")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use super::{health_listen_addr, router, router_with_dbos_ingress, DbosReadiness};
    use crate::dbos_ingress::WorkerIngress;

    #[test]
    fn listen_addr_defaults_when_unset() {
        let addr = health_listen_addr(|_| None).expect("default parses");
        assert_eq!(addr.to_string(), "0.0.0.0:9083");
    }

    #[test]
    fn listen_addr_honours_the_override() {
        let addr = health_listen_addr(|key| {
            (key == "WORKFLOWS_SERVICE_HEALTH_LISTEN").then(|| "127.0.0.1:7001".to_owned())
        })
        .expect("override parses");
        assert_eq!(addr.to_string(), "127.0.0.1:7001");
    }

    #[test]
    fn listen_addr_rejects_a_malformed_value() {
        assert!(health_listen_addr(|_| Some("not-an-address".to_owned())).is_err());
    }

    #[tokio::test]
    async fn healthz_responds_ok_without_any_restate_signature() {
        crate::request_identity::install_crypto_provider();
        let request = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .expect("unsigned health request builds");
        let response = router().oneshot(request).await.expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn readiness_router(dbos: Option<DbosReadiness>) -> Router {
        router_with_dbos_ingress(WorkerIngress::default(), dbos)
    }

    fn readiness_request() -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .expect("unsigned readiness request builds")
    }

    fn readiness_with(is_running: bool, system_database_url: Option<&str>) -> DbosReadiness {
        DbosReadiness {
            is_running: Arc::new(move || is_running),
            system_database_url: system_database_url.map(Arc::from),
        }
    }

    #[tokio::test]
    async fn readyz_reads_disabled_without_a_configured_executor() {
        crate::request_identity::install_crypto_provider();
        let response = readiness_router(None)
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_fails_when_the_executor_is_stopped() {
        crate::request_identity::install_crypto_provider();
        let readiness = readiness_with(false, Some("postgres://127.0.0.1:1/dbos_probe"));
        let response = readiness_router(Some(readiness))
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_fails_when_the_system_database_is_unreachable() {
        crate::request_identity::install_crypto_provider();
        // Loopback port 1 refuses instantly, so the probe takes the
        // unreachable path without a database dependency.
        let readiness = readiness_with(true, Some("postgres://127.0.0.1:1/dbos_probe"));
        let response = readiness_router(Some(readiness))
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_fails_without_a_configured_system_database_url() {
        crate::request_identity::install_crypto_provider();
        let readiness = readiness_with(true, None);
        let response = readiness_router(Some(readiness))
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
