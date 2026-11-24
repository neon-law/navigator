//! `navigator relationship add` against a deployment's Relationship Log door.

use serde_json::json;
use uuid::Uuid;

use crate::site::Site;

fn add<'a>(scope: &'a str, subject: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["relationship", "add", "--scope", scope];
    args.extend(["--type", "witness", "--detail", "Observed the signing"]);
    args.extend(subject);
    args
}

#[tokio::test]
async fn relationship_add_records_one_observation_in_the_named_scope() {
    let site = Site::start().await;
    let request = Uuid::now_v7();
    let rid = request.to_string();
    let subject = Uuid::now_v7().to_string();
    let firm = Uuid::now_v7().to_string();
    let person = ["--person", subject.as_str()];
    let in_project = [&person[..], &["--project", "sample"]].concat();

    site.navigator(&add("project", &person))
        .await
        .refused(1, "pass --project or run from a Project checkout");
    site.navigator(&add("firm", &in_project))
        .await
        .refused(1, "use --project with project scope");
    site.navigator(&add("project", &in_project))
        .await
        .refused(1, "site refused the observation (HTTP 404 Not Found)");

    let log = "/app/api/graph/relationship-log";
    site.answer("POST", log, 201, json!({ "id": request }))
        .await;
    let recorded = format!("Recorded Relationship Log {request}");
    let retried = [&in_project[..], &["--request-id", &rid]].concat();
    let ran = site.navigator(&add("project", &retried)).await;
    assert_eq!(ran.succeeded().trim(), recorded);
    let (_, _, body) = site.last_request().await;
    assert_eq!(
        body,
        Some(json!({
            "request_id": request, "scope": "project", "firm_id": null,
            "project_id": site.project, "subject": "person", "subject_id": subject,
            "action": "witness", "detail": "Observed the signing",
        }))
    );

    let entity = ["--firm", firm.as_str(), "--entity", subject.as_str()];
    let ran = site.navigator(&add("firm", &entity)).await;
    assert_eq!(ran.succeeded().trim(), recorded);
    let (_, _, body) = site.last_request().await;
    let body = body.unwrap();
    assert_eq!(
        [
            &body["scope"],
            &body["firm_id"],
            &body["project_id"],
            &body["subject"]
        ],
        [&json!("firm"), &json!(firm), &json!(null), &json!("entity")]
    );
}
