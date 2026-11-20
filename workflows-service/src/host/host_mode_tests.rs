fn object_fixture_options() -> Option<sqlx::postgres::PgConnectOptions> {
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
        return None;
    };
    Some(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable)
            .options([("session_preload_libraries", "age")]),
    )
}

fn object_dbos_lookup(key: &str) -> Option<String> {
    match key {
        "NAVIGATOR_DBOS_ENABLED"
        | "NAVIGATOR_DBOS_NOTATION_ENABLED"
        | "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED" => Some("true".into()),
        "NAVIGATOR_WORKFLOW_ENGINE" => Some("dbos".into()),
        _ => None,
    }
}

#[tokio::test]
async fn verified_object_activation_removes_only_the_two_legacy_sdk_http_services() {
    let Some(options) = object_fixture_options() else {
        return;
    };
    for active in [false, true] {
        let fixture = if active {
            store::test_support::PostgresFixture::create(options.clone()).await
        } else {
            store::test_support::PostgresFixture::create_capture_target(options.clone()).await
        }
        .unwrap();
        let selection = store::durable_object_handover::require_runtime_selection(
            fixture.postgres(),
            object_dbos_lookup,
        )
        .await
        .unwrap();
        let source = store::surreal::SurrealDb::uninitialized();
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
        let objects = selection.legacy_objects().then(|| {
            (
                NotationService::new(
                    source.clone(),
                    Arc::new(workflows::CapturingEmail::default()),
                    storage,
                ),
                ProjectSlackService::new(
                    source.clone(),
                    Arc::new(crate::firm_slack::FirmSlackBots::new(Arc::new(
                        cloud::NoRuntimeKms,
                    ))),
                    false,
                ),
            )
        });
        let endpoint = bind_legacy_objects(
            Endpoint::builder().bind(SandboxCreditResetService::new(source)),
            objects,
        )
        .build();
        let discovery = endpoint.handle(
            axum::http::Request::builder()
                .uri("/discover")
                .header("accept", "application/vnd.restate.endpointmanifest.v2+json")
                .body(axum::body::Body::empty())
                .unwrap(),
        );
        assert_eq!(discovery.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(axum::body::Body::new(discovery.into_body()), 1024 * 1024)
            .await
            .unwrap();
        let discovery: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let services = discovery["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|service| service["name"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            services.contains("SandboxCreditReset"),
            "unrelated service remains bound"
        );
        assert_eq!(services.contains("notation"), !active);
        assert_eq!(services.contains("project-slack"), !active);
        for (service, handler) in [
            ("notation", "workflow_start"),
            ("project-slack", "client_project_view"),
        ] {
            let response = endpoint.handle(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/invoke/{service}/{handler}"))
                    .header("content-type", "application/vnd.restate.invocation.v5")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            );
            assert_eq!(
                response.status(),
                if active {
                    axum::http::StatusCode::NOT_FOUND
                } else {
                    axum::http::StatusCode::OK
                }
            );
        }
        let unrelated = endpoint.handle(
            axum::http::Request::builder()
                .method("POST")
                .uri("/invoke/SandboxCreditReset/run")
                .header("content-type", "application/vnd.restate.invocation.v5")
                .body(axum::body::Body::empty())
                .unwrap(),
        );
        assert_eq!(unrelated.status(), axum::http::StatusCode::OK);
        fixture.close().await.unwrap();
    }
}

use super::*;
#[test]
fn embedded_dbos_host_needs_no_restate_identity_configuration() {
    assert!(restate_endpoint(
        HostMode::Embedded,
        store::DeploymentEnvironment::Production,
        |_| None
    )
    .unwrap()
    .is_none());
    assert!(restate_endpoint(
        HostMode::Standalone,
        store::DeploymentEnvironment::Production,
        |_| None
    )
    .is_err());
    assert!(restate_endpoint(
        HostMode::Standalone,
        store::DeploymentEnvironment::Dev,
        |_| None
    )
    .unwrap()
    .is_some());
}
