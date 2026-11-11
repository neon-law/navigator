//! Enable the GCP APIs `setup` calls during the rest of the
//! pipeline. We use `serviceusage.batchEnable` so the whole list
//! goes through a single long-running operation.
//!
//! Enabling an already-enabled service is a no-op on Google's side
//! — the LRO just completes successfully. So we don't need to
//! special-case 409 here.

use serde_json::json;

use super::client::{GcpClient, GcpService};
use super::error::{SetupError, SetupResult};
use super::lro;

/// The APIs every `setup` run needs. Order is cosmetic.
pub const REQUIRED_SERVICES: &[&str] = &[
    "compute.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "servicenetworking.googleapis.com",
    "storage.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "sts.googleapis.com",
    "artifactregistry.googleapis.com",
    "container.googleapis.com",
    "gkehub.googleapis.com",
    "gkebackup.googleapis.com",
    "anthosconfigmanagement.googleapis.com",
    "logging.googleapis.com",
    "monitoring.googleapis.com",
    "cloudtrace.googleapis.com",
    "secretmanager.googleapis.com",
    // The key each deployment's `secrets.enc.yaml` is encrypted against.
    // Without this the project cannot hold one, so `sops` has nothing to
    // encrypt to and the deployment can never be written into the tree.
    "cloudkms.googleapis.com",
    "certificatemanager.googleapis.com",
    "identitytoolkit.googleapis.com",
    "speech.googleapis.com",
    "drive.googleapis.com",
    "admin.googleapis.com",
    "aiplatform.googleapis.com",
    // `navigator-deploy mail` calls Gmail with an OAuth client the Firm creates in
    // this project; Google refuses the client's tokens until the project
    // enables the API. See docs/mail-cli.md#provision-gmail-for-navigator-deploy-mail.
    "gmail.googleapis.com",
];

pub async fn enable_services(client: &GcpClient, project_id: &str) -> SetupResult<()> {
    enable(client, project_id, REQUIRED_SERVICES).await
}

/// Service Usage accepts at most twenty services in one `batchEnable` request.
/// Navigator needs more than twenty control planes, so the same idempotent
/// operation is issued in bounded batches.
const BATCH_ENABLE_LIMIT: usize = 20;

/// Identify the narrow GCP response emitted while a completed Service Usage
/// enable operation is still propagating to the target API.
pub(super) fn activation_is_propagating(body: &str, service_id: &str) -> bool {
    body.contains("SERVICE_DISABLED") && body.contains(service_id)
}

/// Enable an arbitrary list of GCP APIs on `project_id` via
/// `serviceusage.batchEnable`. Used by focused subcommands that want
/// only one API turned on rather than the full `REQUIRED_SERVICES` set.
pub async fn enable(client: &GcpClient, project_id: &str, service_ids: &[&str]) -> SetupResult<()> {
    for batch in service_ids.chunks(BATCH_ENABLE_LIMIT) {
        let body = json!({ "serviceIds": batch });
        let resp = client
            .post_json(
                GcpService::ServiceUsage,
                &format!("/v1/projects/{project_id}/services:batchEnable"),
                &body,
            )
            .await?;
        let status = resp.status_u16();
        if !(200..=299).contains(&status) {
            return Err(SetupError::BadStatus {
                operation: "batchEnable".into(),
                status,
                body: resp.into_text(),
            });
        }
        let body: serde_json::Value =
            serde_json::from_str(&resp.into_text()).map_err(|source| SetupError::Json {
                what: "batchEnable response",
                source,
            })?;
        lro::wait(client, GcpService::ServiceUsage, &body, "/v1/{name}").await?;
    }
    Ok(())
}

/// Service Usage's largest page of a project's services.
const LIST_PAGE_SIZE: u32 = 200;

/// The services in `required` that `project_id` does not report as enabled,
/// in `required` order. Read-only: it lists the project's enabled services
/// through `services.list` and never enables anything.
pub async fn missing(
    client: &GcpClient,
    project_id: &str,
    required: &[&str],
) -> SetupResult<Vec<String>> {
    let mut enabled = std::collections::BTreeSet::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut path = format!(
            "/v1/projects/{project_id}/services?filter=state:ENABLED&pageSize={LIST_PAGE_SIZE}"
        );
        if let Some(token) = &page_token {
            path.push_str("&pageToken=");
            path.push_str(token);
        }
        let resp = client.get(GcpService::ServiceUsage, &path).await?;
        let status = resp.status_u16();
        if !(200..=299).contains(&status) {
            return Err(SetupError::BadStatus {
                operation: "services.list".into(),
                status,
                body: resp.into_text(),
            });
        }
        let body: serde_json::Value =
            serde_json::from_str(&resp.into_text()).map_err(|source| SetupError::Json {
                what: "services.list response",
                source,
            })?;
        for service in body["services"].as_array().into_iter().flatten() {
            let name = service["config"]["name"].as_str().or_else(|| {
                service["name"]
                    .as_str()
                    .and_then(|name| name.rsplit("/services/").next())
            });
            if let Some(name) = name {
                enabled.insert(name.to_owned());
            }
        }
        match body["nextPageToken"].as_str() {
            Some(token) if !token.is_empty() => page_token = Some(token.to_owned()),
            _ => break,
        }
    }
    Ok(required
        .iter()
        .filter(|service| !enabled.contains(**service))
        .map(|service| (*service).to_owned())
        .collect())
}

/// The secret-free drift report `navigator-deploy ops gcp services` prints: one line
/// per required API, `ok` or `MISSING`, then the fix when any is missing.
#[must_use]
pub fn drift_report(project_id: &str, required: &[&str], missing: &[String]) -> String {
    use std::fmt::Write as _;

    let mut text = format!("APIs ops gcp setup enables in {project_id}\n");
    for service in required {
        let mark = if missing.iter().any(|m| m == service) {
            "MISSING "
        } else {
            "ok      "
        };
        let _ = writeln!(text, "  [{mark}] {service}");
    }
    if missing.is_empty() {
        text.push_str("\nNo drift: every required API is enabled.\n");
    } else {
        let _ = write!(
            text,
            "\n{} required API(s) disabled. Enable them with:\n  navigator-deploy ops gcp setup --project-id {project_id}\n",
            missing.len()
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{
        body_partial_json, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::client::{GcpClient, GcpService, StaticToken};
    use super::{drift_report, enable_services, missing, REQUIRED_SERVICES};

    #[test]
    fn required_services_exclude_the_retired_config_connector_api() {
        assert!(
            !REQUIRED_SERVICES.contains(&"configconnector.googleapis.com"),
            "setup must not enable an API the deployment does not use"
        );
    }

    /// The provisioner creates no managed database, so it must not turn on the
    /// control plane for one. Enabling an unused API is not free of
    /// consequence: it advertises a capability, and it is what a re-added
    /// stage would quietly rely on. Matched by prefix rather than by the exact
    /// retired service id, so a sibling database API cannot slip in either.
    #[test]
    fn required_services_enable_no_managed_database_control_plane() {
        let database_apis: Vec<&&str> = REQUIRED_SERVICES
            .iter()
            .filter(|service| service.starts_with("sql") || service.starts_with("spanner"))
            .collect();
        assert!(
            database_apis.is_empty(),
            "the managed-database stage is retired; setup must not enable {database_apis:?}"
        );
    }

    #[test]
    fn required_services_cover_the_deployment_control_planes() {
        for service in [
            "cloudresourcemanager.googleapis.com",
            "iamcredentials.googleapis.com",
            "sts.googleapis.com",
            "gkehub.googleapis.com",
            "monitoring.googleapis.com",
            "cloudtrace.googleapis.com",
            "identitytoolkit.googleapis.com",
            "drive.googleapis.com",
            "admin.googleapis.com",
            "aiplatform.googleapis.com",
            "gmail.googleapis.com",
            // Without this the project cannot hold the key its
            // `secrets.enc.yaml` is encrypted against, so the deployment can
            // never be written into the tree at all.
            "cloudkms.googleapis.com",
        ] {
            assert!(
                REQUIRED_SERVICES.contains(&service),
                "deployment setup must enable {service}"
            );
        }
        assert_eq!(REQUIRED_SERVICES.len(), 24);
    }

    #[tokio::test]
    async fn posts_batch_enable_in_bounded_batches() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/proj/services:batchEnable"))
            .and(body_partial_json(
                json!({ "serviceIds": &REQUIRED_SERVICES[..20] }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "operations/first",
                "done": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/proj/services:batchEnable"))
            .and(body_partial_json(
                json!({ "serviceIds": &REQUIRED_SERVICES[20..] }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "operations/second",
                "done": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, server.uri());
        enable_services(&client, "proj").await.unwrap();
    }

    #[tokio::test]
    async fn waits_for_lro_when_initial_response_is_not_done() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/proj/services:batchEnable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "operations/op1",
                "done": false
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/operations/op1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "operations/op1",
                "done": true
            })))
            .expect(2)
            .mount(&server)
            .await;

        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, server.uri());
        enable_services(&client, "proj").await.unwrap();
    }

    #[tokio::test]
    async fn bails_on_non_2xx_from_batch_enable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/proj/services:batchEnable"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, server.uri());
        let err = enable_services(&client, "proj").await.unwrap_err();
        assert!(format!("{err}").contains("403"), "got {err}");
    }

    #[tokio::test]
    async fn dry_run_records_one_post_per_batch_and_no_polling() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, "http://127.0.0.1:1")
            .with_dry_run();
        enable_services(&client, "proj").await.unwrap();
        let calls = client.recorded_calls();
        assert_eq!(
            calls.len(),
            2,
            "dry-run should record one POST per bounded batch, got {calls:?}"
        );
        for call in calls {
            assert_eq!(call.method, "POST");
            assert!(call.url.ends_with("/services:batchEnable"));
        }
    }

    #[tokio::test]
    async fn missing_reports_required_apis_absent_across_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/projects/proj/services"))
            .and(query_param("filter", "state:ENABLED"))
            .and(query_param("pageToken", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "services": [{
                    "name": "projects/123/services/storage.googleapis.com",
                    "state": "ENABLED"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/projects/proj/services"))
            .and(query_param("filter", "state:ENABLED"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "services": [{
                    "name": "projects/123/services/compute.googleapis.com",
                    "config": { "name": "compute.googleapis.com" },
                    "state": "ENABLED"
                }],
                "nextPageToken": "next"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, server.uri());
        let required = [
            "compute.googleapis.com",
            "gmail.googleapis.com",
            "storage.googleapis.com",
        ];
        let absent = missing(&client, "proj", &required).await.unwrap();
        assert_eq!(absent, ["gmail.googleapis.com"]);
    }

    #[tokio::test]
    async fn missing_surfaces_a_non_2xx_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/projects/proj/services"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let client = GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, server.uri());
        let err = missing(&client, "proj", REQUIRED_SERVICES)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("403"), "got {err}");
    }

    #[test]
    fn drift_report_marks_each_api_and_names_the_fix() {
        let required = ["compute.googleapis.com", "gmail.googleapis.com"];
        let drift = drift_report("proj", &required, &["gmail.googleapis.com".to_owned()]);
        assert!(drift.contains("[ok      ] compute.googleapis.com"));
        assert!(drift.contains("[MISSING ] gmail.googleapis.com"));
        assert!(drift.contains("navigator-deploy ops gcp setup --project-id proj"));

        let clean = drift_report("proj", &required, &[]);
        assert!(!clean.contains("MISSING"));
        assert!(clean.contains("No drift"));
    }
}
