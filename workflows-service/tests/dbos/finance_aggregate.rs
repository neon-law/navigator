//! `FinanceAggregate`'s registered DBOS workflow against a disposable
//! PostgreSQL executor and synthetic object-store/Surreal data.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cloud::{FsStorage, StorageService};
use tower::ServiceExt;
use workflows::dbos_engine;
use workflows_service::dbos_finance_aggregate::{
    self, Context, FinanceAggregateError, FinanceAggregateReport, RunRequest,
};
use workflows_service::dbos_ingress::{FinanceIngress, WorkerIngress};

use crate::support::{database, APP_VERSION, DEADLINE};

const INPUT: &str = include_str!("../../../store/tests/data/finance/sample-finance-input.json");

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end run covers duplicate admission and restart identity.
async fn registered_finance_workflow_joins_duplicate_manual_run_id() {
    let Some(db) = database().await else { return };
    let directory = tempfile::tempdir().expect("temporary synthetic object store");
    let storage: Arc<dyn StorageService> = Arc::new(
        FsStorage::new(directory.path())
            .await
            .expect("open synthetic object store"),
    );
    let surreal = store::test_support::mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&surreal, "DBOS Finance").await;
    let project = store::projects::find_by_id(&surreal, project_id)
        .await
        .expect("load synthetic project")
        .expect("synthetic project exists");
    let project_code = project.code;
    storage
        .put(
            &store::finance_aggregate::input_key(&project_code),
            INPUT.as_bytes(),
            "application/json",
        )
        .await
        .expect("write synthetic input");
    let postgres = db.store().await;
    let settings = dbos_engine::config(&db.url, APP_VERSION);
    let executor = dbos::DBOS::new(settings.clone());
    let workflow =
        dbos_finance_aggregate::register(&executor, Context::new(surreal.clone(), storage.clone()))
            .expect("register FinanceAggregate");
    executor.launch().await.expect("launch DBOS");
    let router = workflows_service::health::router_with_dbos_ingress(WorkerIngress {
        finance: Some(FinanceIngress::new(
            "synthetic-service-token".into(),
            workflow.clone(),
            executor.clone(),
            surreal.clone(),
            postgres.clone(),
        )),
        reconcile: None,
        nightly_project_update: None,
        email_summary: None,
        inbound_message_alert: None,
        project_slack: None,
    });

    let request = RunRequest {
        project_id,
        project_code: project_code.clone(),
        parameters: None,
    };
    let legacy_id =
        dbos_finance_aggregate::workflow_id("manual-0190e0d0-0000-7000-8000-000000000003");
    workflow
        .start_with(
            request.clone(),
            dbos::StartOptions {
                workflow_id: Some(&legacy_id),
                ..Default::default()
            },
        )
        .await
        .expect("start claimless legacy workflow directly");
    executor
        .retrieve_workflow::<FinanceAggregateReport, FinanceAggregateError>(&legacy_id)
        .expect("retrieve claimless legacy workflow")
        .result()
        .await
        .expect("legacy workflow completed");
    let run_id = "manual-0190e0d0-0000-7000-8000-000000000001";
    let id = dbos_finance_aggregate::workflow_id(run_id);
    let payload = serde_json::json!({
        "workflow_id": id.clone(),
        "request": request,
    });
    let preexisting = Request::builder()
        .method("POST")
        .uri("/internal/dbos/finance-aggregate")
        .header("authorization", "Bearer synthetic-service-token")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": legacy_id,
                "request": {
                    "project_id": project_id,
                    "project_code": project_code.clone()
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(preexisting).await.unwrap().status(),
        StatusCode::CONFLICT,
        "claimless DBOS workflows cannot be rebound through ingress"
    );
    let wrong_project = Request::builder()
        .method("POST")
        .uri("/internal/dbos/finance-aggregate")
        .header("authorization", "Bearer synthetic-service-token")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": id.clone(),
                "request": {
                    "project_id": project_id,
                    "project_code": "different-synthetic-project"
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(wrong_project)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND,
        "worker rejects a Project id and code that do not match"
    );
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/internal/dbos/finance-aggregate")
            .header("authorization", "Bearer synthetic-service-token")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
    }
    let other_project_id =
        store::test_support::seed_project_surreal(&surreal, "DBOS Finance Other").await;
    let other_project = store::projects::find_by_id(&surreal, other_project_id)
        .await
        .expect("load second synthetic project")
        .expect("second synthetic project exists");
    let conflicting = Request::builder()
        .method("POST")
        .uri("/internal/dbos/finance-aggregate")
        .header("authorization", "Bearer synthetic-service-token")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "workflow_id": id.clone(),
                "request": {
                    "project_id": other_project_id,
                    "project_code": other_project.code,
                    "parameters": {
                        "overhead_pct": 30,
                        "attorney_pct": 70,
                        "lead_pct": 80
                    }
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(conflicting).await.unwrap().status(),
        StatusCode::CONFLICT,
        "a global workflow id cannot be rebound to a different project or parameter set"
    );
    let race_id =
        dbos_finance_aggregate::workflow_id("manual-0190e0d0-0000-7000-8000-000000000002");
    let raced_requests = [
        serde_json::json!({
            "workflow_id": race_id,
            "request": {
                "project_id": other_project_id,
                "project_code": other_project.code,
                "parameters": {"overhead_pct": 28, "attorney_pct": 70, "lead_pct": 80}
            }
        }),
        serde_json::json!({
            "workflow_id": race_id,
            "request": {
                "project_id": other_project_id,
                "project_code": other_project.code,
                "parameters": {"overhead_pct": 29, "attorney_pct": 70, "lead_pct": 80}
            }
        }),
    ];
    let request_a = Request::builder()
        .method("POST")
        .uri("/internal/dbos/finance-aggregate")
        .header("authorization", "Bearer synthetic-service-token")
        .body(Body::from(serde_json::to_vec(&raced_requests[0]).unwrap()))
        .unwrap();
    let request_b = Request::builder()
        .method("POST")
        .uri("/internal/dbos/finance-aggregate")
        .header("authorization", "Bearer synthetic-service-token")
        .body(Body::from(serde_json::to_vec(&raced_requests[1]).unwrap()))
        .unwrap();
    let (left, right) = tokio::join!(
        router.clone().oneshot(request_a),
        router.clone().oneshot(request_b)
    );
    let mut statuses = [left.unwrap().status(), right.unwrap().status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::ACCEPTED, StatusCode::CONFLICT]);
    let retry_id =
        dbos_finance_aggregate::workflow_id("manual-0190e0d0-0000-7000-8000-000000000004");
    let retry_payload = serde_json::json!({
        "workflow_id": retry_id,
        "request": {
            "project_id": other_project_id,
            "project_code": other_project.code,
            "parameters": {"overhead_pct": 28, "attorney_pct": 70, "lead_pct": 80}
        }
    });
    let same_request = || {
        Request::builder()
            .method("POST")
            .uri("/internal/dbos/finance-aggregate")
            .header("authorization", "Bearer synthetic-service-token")
            .body(Body::from(serde_json::to_vec(&retry_payload).unwrap()))
            .unwrap()
    };
    let (left, right) = tokio::join!(
        router.clone().oneshot(same_request()),
        router.clone().oneshot(same_request())
    );
    assert_eq!(left.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(right.unwrap().status(), StatusCode::ACCEPTED);
    let retry_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(retry_id)
            .fetch_one(&db.pool)
            .await
            .expect("count the one same-payload retry workflow");
    assert_eq!(retry_rows, 1);
    let stored_claim: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT workflow_name, request_fingerprint FROM dbos_workflow_request WHERE workflow_id = $1",
    )
    .bind(&id)
    .fetch_optional(postgres.pool())
    .await
    .expect("read synthetic request identity claim");
    assert_eq!(
        stored_claim.map(|claim| claim.0).as_deref(),
        Some("FinanceAggregate")
    );

    let report = tokio::time::timeout(
        DEADLINE,
        executor
            .retrieve_workflow::<FinanceAggregateReport, FinanceAggregateError>(&id)
            .expect("retrieve DBOS FinanceAggregate")
            .result(),
    )
    .await
    .expect("FinanceAggregate did not complete")
    .expect("FinanceAggregate succeeded");
    assert_eq!(id, format!("FinanceAggregate:{run_id}"));
    assert_eq!(report.run_id, run_id);
    assert_eq!(report.matters, 633);
    assert_eq!(
        report.report_key,
        store::finance_aggregate::report_key(&project_code, &report.run_id)
    );
    let object = storage
        .get(&report.report_key)
        .await
        .expect("persisted synthetic report");
    let stored: store::finance_aggregate::FinanceReport =
        serde_json::from_slice(&object.bytes).expect("valid report JSON");
    assert_eq!(stored.run_id, report.run_id);
    assert_eq!(
        store::finance_aggregate::latest_for_project(&surreal, project_id)
            .await
            .expect("query synthetic report row")
            .expect("report row exists"),
        stored
    );
    executor.shutdown().await;

    // A replacement executor with the same release reattaches to the
    // completed run. Its input request is intentionally repeated with the
    // original global ID, so a restart cannot mint a second report row.
    let recovered = dbos::DBOS::new(settings);
    let recovered_workflow = dbos_finance_aggregate::register(
        &recovered,
        Context::new(surreal.clone(), storage.clone()),
    )
    .expect("register recovered FinanceAggregate");
    recovered.launch().await.expect("launch recovered DBOS");
    recovered_workflow
        .start_with(
            RunRequest {
                project_id,
                project_code: project_code.clone(),
                parameters: None,
            },
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .expect("reattach to the existing manual run");
    let recovered_report = tokio::time::timeout(
        DEADLINE,
        recovered
            .retrieve_workflow::<FinanceAggregateReport, FinanceAggregateError>(&id)
            .expect("retrieve recovered workflow")
            .result(),
    )
    .await
    .expect("recovered workflow did not settle")
    .expect("recovered workflow succeeded");
    recovered.shutdown().await;
    assert_eq!(recovered_report, report);
    let workflow_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count the single global workflow row");
    assert_eq!(workflow_rows, 1);
}
