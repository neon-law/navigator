#![allow(clippy::unused_async)]

use std::sync::Arc;

use features::{app_state, fs_storage};
use portal::{policy::PolicyClient, SessionStore};
use workflows::InMemoryRuntime;

#[tokio::main]
async fn main() {
    features::portfolio_acceptance::configure_test_environment();
    let counts = features::portfolio_acceptance::prepare_test_fixture().await;
    let sessions = SessionStore::new("test-portfolio-acceptance-session-key");
    let storage = fs_storage("portfolio-acceptance").await;
    let state = app_state(
        Arc::new(InMemoryRuntime::new()),
        storage,
        PolicyClient::passthrough(),
        None,
        sessions.clone(),
    )
    .await;
    let app = features::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let mut results = Vec::new();

    for scenario in features::portfolio_acceptance::SCENARIOS {
        let response = features::portfolio_acceptance::request_api(&app, &sessions, scenario.id)
            .await
            .expect("shared portfolio scenario executes");
        let check = features::portfolio_acceptance::check_result(
            scenario.id,
            response.status,
            &response.body,
        );
        results.push(features::portfolio_acceptance::ScenarioResult {
            scenario_id: scenario.id,
            actual_status: response.status,
            passed: check.is_ok(),
            error: check.err(),
        });
    }

    let report = features::portfolio_acceptance::report_json(&results, counts)
        .expect("serialize portfolio acceptance report");
    let path = std::env::var("NAV_ENG_821_API_REPORT")
        .unwrap_or_else(|_| "/tmp/navigator-screenshots/eng-821/api-report.json".into());
    let path = std::path::Path::new(&path);
    std::fs::create_dir_all(path.parent().expect("report parent"))
        .expect("create acceptance report directory");
    std::fs::write(path, report).expect("write acceptance API report");
    eprintln!("ENG-821 API acceptance report: {}", path.display());
    assert!(
        results.iter().all(|result| result.passed),
        "API acceptance failures: {:?}",
        results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| (&result.scenario_id, &result.error))
            .collect::<Vec<_>>(),
    );
}
