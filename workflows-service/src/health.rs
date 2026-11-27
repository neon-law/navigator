//! Liveness and PostgreSQL readiness for the DBOS executor.

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

/// Listener for the DBOS ingress and its health probes.
const DEFAULT_HEALTH_LISTEN: &str = "0.0.0.0:9083";

pub fn router() -> Router {
    Router::new().route("/healthz", get(healthz))
}

pub fn router_with_dbos_ingress(
    ingress: crate::dbos_ingress::WorkerIngress,
    dbos: Option<DbosReadiness>,
) -> Router {
    // `/healthz` must survive the combination: both deployment manifests send
    // the liveness probe here, and the production worker always combines the
    // DBOS ingress, so a combined router without it restarts healthy workers.
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(dbos.map(Arc::new));
    crate::dbos_ingress::add_routes(router, ingress)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Report whether the DBOS executor is running and can reach its system database.
async fn readyz(State(dbos): State<Option<Arc<DbosReadiness>>>) -> axum::response::Response {
    let Some(readiness) = dbos.as_deref() else {
        return readiness_response(StatusCode::OK, "disabled");
    };
    if !(readiness.is_running)() {
        return readiness_response(StatusCode::SERVICE_UNAVAILABLE, "stopped");
    }
    let Some(url) = readiness.system_database_url.as_deref() else {
        return readiness_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "system-database-unconfigured",
        );
    };
    match tokio::time::timeout(READY_DB_TIMEOUT, system_database_ping(url)).await {
        Ok(Ok(())) => readiness_response(StatusCode::OK, "ok"),
        Ok(Err(reason)) => {
            // Staged, URL-free detail for the log only; the unsigned body
            // stays `system-database-unreachable`.
            tracing::error!(%reason, "readiness probe: system database unreachable");
            readiness_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "system-database-unreachable",
            )
        }
        Err(_) => {
            tracing::error!("readiness probe: system database ping timed out");
            readiness_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "system-database-unreachable",
            )
        }
    }
}

fn readiness_response(status: StatusCode, dbos: &str) -> axum::response::Response {
    (status, Json(json!({ "dbos": dbos }))).into_response()
}

/// Probe the executor's system database with one cheap connection: opening
/// the pool's first connection IS the probe (the executor's own pools use
/// the same `PgConnectOptions` plus its session settings; this one only
/// answers "is the system database there"). The error classifies the failure
/// stage for the log; the route is unsigned, so the response never echoes
/// detail.
async fn system_database_ping(url: &str) -> Result<(), SystemDatabasePingError> {
    let options: sqlx::postgres::PgConnectOptions =
        url.parse().map_err(|_| SystemDatabasePingError::Parse)?;
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(READY_DB_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|error| SystemDatabasePingError::Connect(classify_connect(&error)))?;
    Ok(())
}

/// Where the system-database probe failed, carrying only what is safe to
/// log. Driver detail that may embed the connection URL (configuration
/// errors) collapses to the variant name; no variant ever holds the URL or
/// its credentials.
#[derive(Debug)]
enum SystemDatabasePingError {
    /// The configured URL did not parse as connection options.
    Parse,
    /// The server was unreachable, refused the connection, or rejected the
    /// login.
    Connect(ConnectFailure),
}

impl std::fmt::Display for SystemDatabasePingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse => f.write_str("connection URL did not parse"),
            Self::Connect(failure) => write!(f, "connect failed: {failure}"),
        }
    }
}

/// The connect-stage failure's safe detail.
#[derive(Debug)]
enum ConnectFailure {
    /// Transport-level: connection refused, timeouts, DNS.
    Io(String),
    /// The PostgreSQL server answered with an error, e.g. a rejected login.
    Database(String),
    /// TLS negotiation failed.
    Tls(String),
    /// Pool/configuration failures, whose text may embed the connection
    /// URL — logged as the variant name only.
    Other(&'static str),
}

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(detail) => write!(f, "transport: {detail}"),
            Self::Database(detail) => write!(f, "server rejected the connection: {detail}"),
            Self::Tls(detail) => write!(f, "tls: {detail}"),
            Self::Other(stage) => f.write_str(stage),
        }
    }
}

fn classify_connect(error: &sqlx::Error) -> ConnectFailure {
    match error {
        sqlx::Error::Io(io) => ConnectFailure::Io(io.to_string()),
        sqlx::Error::Database(database) => ConnectFailure::Database(database.to_string()),
        sqlx::Error::Tls(detail) => ConnectFailure::Tls(detail.to_string()),
        _ => ConnectFailure::Other("connection failed (configuration or pool)"),
    }
}

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
    async fn healthz_responds_ok() {
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
        let response = readiness_router(None)
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_fails_when_the_executor_is_stopped() {
        let readiness = readiness_with(false, Some("postgres://127.0.0.1:1/dbos_probe"));
        let response = readiness_router(Some(readiness))
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_fails_when_the_system_database_is_unreachable() {
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
        let readiness = readiness_with(true, None);
        let response = readiness_router(Some(readiness))
            .oneshot(readiness_request())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn combined_router_exposes_liveness() {
        // The manifests pin the liveness probe to /healthz on the SAME
        // listener the DBOS ingress and /readyz share; dropping it restarts
        // healthy workers.
        let healthz_request = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .expect("unsigned liveness request builds");
        let response = readiness_router(None)
            .oneshot(healthz_request)
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn connect_failure_classification_never_echoes_the_url() {
        // Configuration errors carry the raw connect options (and therefore
        // the URL with its credentials); their classification must collapse
        // to the variant name.
        let leaking =
            sqlx::Error::Configuration("postgres://user:secret@host/db".to_owned().into());
        let classified = super::SystemDatabasePingError::Connect(super::classify_connect(&leaking));
        let rendered = classified.to_string();
        assert!(!rendered.contains("postgres://"), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
        // A transport failure keeps its safe detail: operators can tell a
        // refused connection from a rejected login.
        let refused = super::SystemDatabasePingError::Connect(super::ConnectFailure::Io(
            "Connection refused (os error 111)".to_owned(),
        ));
        assert!(refused.to_string().contains("Connection refused"));
    }
}
