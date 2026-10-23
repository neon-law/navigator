//! The `neon` web process hosting the DBOS executor beside its server.
//!
//! `neon`'s `main` boots through `portal::hosting::run_with_dbos`, which wraps
//! the bound server in [`portal::hosting::with_dbos`]. These tests drive that
//! seam with the real `neon` router on a real listener against a disposable
//! PostgreSQL database: with the flag on, `Heartbeat` runs from this process
//! under its slot-keyed id and stops when the server drains; with it off, the
//! server never touches DBOS.
//!
//! Without `NAVIGATOR_TEST_POSTGRES_PORT` they skip, unless
//! `NAV_REQUIRE_POSTGRES=1`.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use workflows::dbos_engine::{self, heartbeat};
use workflows::CapturingNotifier;

const DEADLINE: Duration = Duration::from_mins(1);

struct TestDatabase {
    env: Vec<(&'static str, String)>,
    pool: sqlx::PgPool,
}

impl TestDatabase {
    fn lookup(&self, enabled: Option<&'static str>) -> impl Fn(&str) -> Option<String> + 'static {
        let mut env = self.env.clone();
        if let Some(enabled) = enabled {
            env.push((dbos_engine::ENABLED_ENV, enabled.to_owned()));
        }
        move |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        }
    }
}

/// A fresh synthetic database, or `None` when no test PostgreSQL is set.
async fn database() -> Option<TestDatabase> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let port: u16 = port
        .parse()
        .expect("NAVIGATOR_TEST_POSTGRES_PORT is a port");
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone())
        .await
        .expect("connect to the test server");
    let name = format!("neon_dbos_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("create the test database");
    admin.close().await;
    let pool = sqlx::PgPool::connect_with(options.database(&name))
        .await
        .expect("connect to the test database");
    Some(TestDatabase {
        env: vec![
            (dbos_engine::HOST_ENV, "127.0.0.1".to_owned()),
            (dbos_engine::PORT_ENV, port.to_string()),
            (dbos_engine::USER_ENV, "postgres".to_owned()),
            (dbos_engine::PASSWORD_ENV, "synthetic".to_owned()),
            (dbos_engine::DATABASE_ENV, name),
            (dbos_engine::SSLMODE_ENV, "disable".to_owned()),
            (dbos_engine::RELEASE_ENV, "test".to_owned()),
            (
                dbos_engine::EXECUTOR_ID_ENV,
                "navigator-neon-test".to_owned(),
            ),
        ],
        pool,
    })
}

async fn neon_router() -> axum::Router {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    portal::bootstrap(
        state.clone(),
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
        neon::public_routes(),
        neon::PUBLIC_PATHS,
        neon::public_dioxus_routers(&state),
    )
    .expect("neon router")
}

/// Whether the server on `addr` answers `GET /` without a server error, over
/// a plain socket.
async fn answers(addr: std::net::SocketAddr) -> bool {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to the server");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("send the probe");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read the probe");
    let status = response.lines().next().unwrap_or_default();
    status.starts_with("HTTP/1.1 ") && !status.starts_with("HTTP/1.1 5")
}

/// Serve the `neon` router through `with_dbos` until `stop` fires.
async fn serve(
    lookup: impl Fn(&str) -> Option<String> + Send + 'static,
    notifier: CapturingNotifier,
) -> (
    std::net::SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a port");
    let addr = listener.local_addr().expect("the bound address");
    let router = neon_router().await;
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(portal::hosting::with_dbos(
        lookup,
        Arc::new(notifier),
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await?;
            Ok(())
        },
    ));
    (addr, stop, server)
}

#[tokio::test]
async fn neon_runs_the_heartbeat_with_dbos_on_and_stops_it_on_shutdown() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = CapturingNotifier::new();
    let (addr, stop, server) = serve(db.lookup(Some("true")), notifier.clone()).await;

    // The line posts inside `notify` before its checkpoint commits, so wait for
    // the run to finish rather than for the line.
    tokio::time::timeout(DEADLINE, async {
        loop {
            let finished: Option<bool> = sqlx::query_scalar(
                "SELECT bool_and(status = 'SUCCESS') FROM dbos.workflow_status WHERE name = $1",
            )
            .bind(heartbeat::WORKFLOW)
            .fetch_one(&db.pool)
            .await
            .ok()
            .flatten();
            if finished == Some(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the web process never finished a heartbeat");
    assert!(answers(addr).await, "the neon router serves");

    stop.send(()).expect("the server is running");
    tokio::time::timeout(DEADLINE, server)
        .await
        .expect("the server never drained")
        .expect("the server task panicked")
        .expect("the server and executor shut down cleanly");

    let runs: Vec<(String, String)> =
        sqlx::query_as("SELECT workflow_uuid, status FROM dbos.workflow_status WHERE name = $1")
            .bind(heartbeat::WORKFLOW)
            .fetch_all(&db.pool)
            .await
            .expect("read the heartbeat runs");
    let [(id, status)] = &runs[..] else {
        panic!("expected one heartbeat run: {runs:?}");
    };
    assert_eq!(status, "SUCCESS");
    let slot = heartbeat::slot(chrono::Utc::now());
    let previous = slot - chrono::Duration::hours(6);
    assert!(
        *id == heartbeat::workflow_id(slot) || *id == heartbeat::workflow_id(previous),
        "the run is keyed by its slot: {id}"
    );
    let [line] = &notifier.captured()[..] else {
        panic!("expected one heartbeat line");
    };
    assert!(line.contains("Durable execution OK"), "{line}");
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the listener closed with the server"
    );
}

#[tokio::test]
async fn neon_never_touches_dbos_with_the_flag_off() {
    let Some(db) = database().await else {
        return;
    };
    for flag in [None, Some("false")] {
        let notifier = CapturingNotifier::new();
        let (addr, stop, server) = serve(db.lookup(flag), notifier.clone()).await;
        assert!(answers(addr).await, "the neon router serves");
        stop.send(()).expect("the server is running");
        server
            .await
            .expect("the server task panicked")
            .expect("the server shuts down cleanly");
        assert!(notifier.captured().is_empty(), "{flag:?}");
    }
    let schema: Option<String> = sqlx::query_scalar("SELECT to_regnamespace('dbos')::text")
        .fetch_one(&db.pool)
        .await
        .expect("look for the dbos schema");
    assert_eq!(schema, None, "no executor launched, so no dbos schema");
}

#[tokio::test]
async fn neon_refuses_to_serve_with_dbos_on_and_its_login_missing() {
    let served = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&served);
    let result = portal::hosting::with_dbos(
        |key| (key == dbos_engine::ENABLED_ENV).then(|| "true".to_owned()),
        Arc::new(CapturingNotifier::new()),
        async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    )
    .await;
    let error = format!("{:#}", result.expect_err("missing settings fail the boot"));
    assert!(error.contains(dbos_engine::HOST_ENV), "{error}");
    assert!(
        !served.load(std::sync::atomic::Ordering::SeqCst),
        "the server never starts without its executor"
    );

    let typo = portal::hosting::with_dbos(
        |key| (key == dbos_engine::ENABLED_ENV).then(|| "yes".to_owned()),
        Arc::new(CapturingNotifier::new()),
        async { Ok(()) },
    )
    .await;
    assert!(typo.is_err(), "an unknown flag value is refused");
}

#[tokio::test]
async fn axum_drains_an_in_flight_request_before_the_dbos_host_returns() {
    let Some(db) = database().await else {
        return;
    };
    let handler_started = Arc::new(tokio::sync::Notify::new());
    let release_handler = Arc::new(tokio::sync::Notify::new());
    let router = axum::Router::new().route(
        "/held-request",
        axum::routing::get({
            let handler_started = Arc::clone(&handler_started);
            let release_handler = Arc::clone(&release_handler);
            move || {
                let handler_started = Arc::clone(&handler_started);
                let release_handler = Arc::clone(&release_handler);
                async move {
                    handler_started.notify_one();
                    release_handler.notified().await;
                    "drained"
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the synthetic Axum listener");
    let addr = listener.local_addr().expect("listener address");
    let (stop, stopped) = oneshot::channel::<()>();
    let mut server = tokio::spawn(portal::hosting::with_dbos(
        db.lookup(Some("true")),
        Arc::new(CapturingNotifier::new()),
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await?;
            Ok(())
        },
    ));

    let mut request = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to the synthetic Axum listener");
    request
        .write_all(b"GET /held-request HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("send the held request");
    tokio::time::timeout(DEADLINE, handler_started.notified())
        .await
        .expect("the Axum handler was not reached");

    stop.send(()).expect("the Axum server is serving");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut server)
            .await
            .is_err(),
        "Axum and the DBOS host must wait for the in-flight request to drain"
    );

    release_handler.notify_one();
    let mut response = String::new();
    tokio::time::timeout(DEADLINE, request.read_to_string(&mut response))
        .await
        .expect("the drained response was not returned")
        .expect("read the drained response");
    assert!(response.contains("drained"), "{response}");
    tokio::time::timeout(DEADLINE, &mut server)
        .await
        .expect("Axum did not finish graceful shutdown")
        .expect("the host task panicked")
        .expect("Axum and DBOS shut down cleanly");
}

#[tokio::test]
async fn neon_hosts_dbos_with_a_login_isolated_from_application_tables() {
    let Some(db) = database().await else {
        return;
    };
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let dbos_login = format!("navigator_dbos_test_{suffix}");
    let app_login = format!("navigator_app_test_{suffix}");
    provision_restricted_dbos_fixture(&db, &dbos_login, &app_login).await;

    let notifier = CapturingNotifier::new();
    let (addr, stop, mut server) = serve(restricted_dbos_lookup(&db, &dbos_login), notifier).await;

    wait_for_restricted_heartbeat(&db, &mut server).await;
    assert!(answers(addr).await, "the neon router serves");
    assert_eq!(
        schema_owner(&db).await,
        dbos_login,
        "the DBOS login owns its schema"
    );
    assert_role_isolation(&db, &dbos_login, &app_login).await;

    stop.send(()).expect("the server is running");
    server
        .await
        .expect("the server task panicked")
        .expect("the server shuts down cleanly");
    cleanup_restricted_dbos_fixture(&db, &dbos_login, &app_login).await;
}

async fn provision_restricted_dbos_fixture(db: &TestDatabase, dbos_login: &str, app_login: &str) {
    // Model the grants from k8s/postgres/access/roles.sql in this disposable
    // database. Both roles are synthetic and scoped to this test run.
    for login in [dbos_login, app_login] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE ROLE {login} LOGIN PASSWORD 'synthetic' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        )))
        .execute(&db.pool)
        .await
        .expect("create synthetic login");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "GRANT CONNECT ON DATABASE {} TO {login}",
            setting(db, dbos_engine::DATABASE_ENV)
        )))
        .execute(&db.pool)
        .await
        .expect("allow database connection");
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT CREATE ON DATABASE {} TO {dbos_login}",
        setting(db, dbos_engine::DATABASE_ENV)
    )))
    .execute(&db.pool)
    .await
    .expect("allow DBOS to create its system schema");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON SCHEMA public FROM {dbos_login}"
    )))
    .execute(&db.pool)
    .await
    .expect("keep DBOS out of the application schema");
    sqlx::query("CREATE TABLE public.synthetic_application_data (id integer)")
        .execute(&db.pool)
        .await
        .expect("create synthetic application data");
}

fn restricted_dbos_lookup(db: &TestDatabase, dbos_login: &str) -> impl Fn(&str) -> Option<String> {
    let mut env = db.env.clone();
    env.push((dbos_engine::ENABLED_ENV, "true".to_owned()));
    for (key, value) in [
        (dbos_engine::USER_ENV, dbos_login.to_owned()),
        (dbos_engine::PASSWORD_ENV, "synthetic".to_owned()),
    ] {
        env.iter_mut()
            .find(|(name, _)| *name == key)
            .expect("setting")
            .1 = value;
    }
    move |key| {
        env.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.clone())
    }
}

async fn wait_for_restricted_heartbeat(
    db: &TestDatabase,
    server: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            if server.is_finished() {
                let result = (&mut *server).await;
                panic!("the restricted DBOS host exited before Heartbeat: {result:?}");
            }
            let finished: Option<bool> = sqlx::query_scalar(
                "SELECT bool_and(status = 'SUCCESS') FROM dbos.workflow_status WHERE name = $1",
            )
            .bind(heartbeat::WORKFLOW)
            .fetch_one(&db.pool)
            .await
            .ok()
            .flatten();
            if finished == Some(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the restricted DBOS login never ran Heartbeat");
}

async fn schema_owner(db: &TestDatabase) -> String {
    sqlx::query_scalar("SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'dbos'")
        .fetch_one(&db.pool)
        .await
        .expect("DBOS creates its system schema")
}

async fn assert_role_isolation(db: &TestDatabase, dbos_login: &str, app_login: &str) {
    let dbos_pool = sqlx::PgPool::connect_with(login_options(db, dbos_login))
        .await
        .expect("connect as the DBOS login");
    let app_data = sqlx::query("SELECT id FROM public.synthetic_application_data")
        .fetch_all(&dbos_pool)
        .await;
    assert_permission_denied(app_data, "DBOS cannot read application data");
    dbos_pool.close().await;

    let app_pool = sqlx::PgPool::connect_with(login_options(db, app_login))
        .await
        .expect("connect as the application login");
    let dbos_data = sqlx::query("SELECT workflow_uuid FROM dbos.workflow_status")
        .fetch_all(&app_pool)
        .await;
    assert_permission_denied(dbos_data, "the application cannot read DBOS state");
    app_pool.close().await;
}

fn login_options(db: &TestDatabase, login: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(
            setting(db, dbos_engine::PORT_ENV)
                .parse()
                .expect("test port"),
        )
        .username(login)
        .password("synthetic")
        .database(setting(db, dbos_engine::DATABASE_ENV))
        .ssl_mode(PgSslMode::Disable)
}

fn assert_permission_denied<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, message: &str) {
    assert_eq!(
        result
            .expect_err(message)
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501"),
        "{message} is denied by PostgreSQL permissions"
    );
}

fn setting<'a>(db: &'a TestDatabase, key: &'static str) -> &'a str {
    db.env
        .iter()
        .find(|(name, _)| *name == key)
        .expect("database setting")
        .1
        .as_str()
}

async fn cleanup_restricted_dbos_fixture(db: &TestDatabase, dbos_login: &str, app_login: &str) {
    sqlx::query("DROP TABLE public.synthetic_application_data")
        .execute(&db.pool)
        .await
        .expect("drop synthetic application data");
    sqlx::query("DROP SCHEMA dbos CASCADE")
        .execute(&db.pool)
        .await
        .expect("drop synthetic DBOS state");
    for login in [&dbos_login, &app_login] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "REVOKE ALL PRIVILEGES ON DATABASE {} FROM {login}",
            setting(db, dbos_engine::DATABASE_ENV)
        )))
        .execute(&db.pool)
        .await
        .expect("revoke synthetic database grants");
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {login}")))
            .execute(&db.pool)
            .await
            .expect("drop synthetic login");
    }
}
