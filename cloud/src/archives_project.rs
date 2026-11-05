//! Fail-closed verification of the Iceberg archive lane's GCP project.
//!
//! The offsite archive lives in a GCP project separate from the deployment
//! (`docs/iceberg-archive.md`). `NAVIGATOR_ARCHIVES_BUCKET` already names the
//! offsite bucket — GCS resolves the bucket's owning project server-side — so
//! pointing the writer at it is an IAM and provisioning concern, not a client
//! change. What code must guarantee is the reverse direction: a bucket
//! misconfigured back into the deployment project must never receive archive
//! writes. [`verify_archives_project`] is the seam for that: when
//! [`ARCHIVES_GCP_PROJECT_ENV`] is set, the archives lane probes the bucket's
//! owning project through the storage credential and refuses to return a
//! handle whose project cannot be confirmed to match.
//!
//! The comparison is bounded by what the writer credential can see. The
//! workflows-service identity holds exactly `roles/storage.objectCreator` and
//! `roles/storage.objectViewer` on the archive bucket — no Cloud Resource
//! Manager access — so the only project identity GCS reports to it is the
//! bucket's numeric project number. The env var therefore accepts either
//! form:
//!
//! - **numeric** (the archive project's number): verified by direct equality
//!   against the probe.
//! - **project id** (e.g. `neon-law-archive`): not resolvable to a number
//!   under a storage-only credential, so the check verifies the property it
//!   can — the bucket's project differs from the deployment project's number,
//!   read from the GKE metadata server (no IAM required). The id is then
//!   carried into the Slack digest annotation. If the metadata server is
//!   unreachable, nothing is confirmed and the lane fails closed.
//!
//! Every unverifiable shape (probe error, numeric mismatch, non-numeric value
//! with no deployment number) is an error, never a pass: the failure mode the
//! seam exists to prevent is archive objects landing in the deployment
//! project.

use crate::{StorageError, StorageService};

/// The optional seam naming the GCP project the Iceberg archive bucket lives
/// in. Absent by default — setting it turns on the fail-closed verification
/// in [`verify_archives_project`] and adds the archive project to the Slack
/// digest. It is a GCS-lane setting: a non-GCS backend has no owning-project
/// concept to confirm against, and refuses.
pub const ARCHIVES_GCP_PROJECT_ENV: &str = "NAVIGATOR_ARCHIVES_GCP_PROJECT_ID";

/// The GKE metadata server. A parameter on
/// [`verify_archives_project_with`] so tests aim it at a dead port instead
/// of the network.
const METADATA_BASE: &str = "http://metadata.google.internal";

/// Verify the archive lane's bucket project when
/// [`ARCHIVES_GCP_PROJECT_ENV`] is set, reading every env var through `get`.
/// Absent (or empty) env: the seam is off and any handle passes.
pub async fn verify_archives_project<F>(
    storage: &dyn StorageService,
    get: F,
) -> Result<(), StorageError>
where
    F: Fn(&str) -> Option<String>,
{
    verify_archives_project_with(storage, get, METADATA_BASE).await
}

/// [`verify_archives_project`] with the metadata-server base injected — the
/// test seam for the non-numeric env shape.
pub async fn verify_archives_project_with<F>(
    storage: &dyn StorageService,
    get: F,
    metadata_base: &str,
) -> Result<(), StorageError>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(expected) = get(ARCHIVES_GCP_PROJECT_ENV).filter(|v| !v.trim().is_empty()) else {
        return Ok(());
    };
    let expected = expected.trim();
    if let Ok(number) = expected.parse::<i64>() {
        // The numeric form verifies by direct comparison — no metadata call.
        let bucket_project = storage.bucket_project_number().await?;
        if number == bucket_project {
            Ok(())
        } else {
            Err(project_mismatch(expected, bucket_project))
        }
    } else {
        // The id form needs the deployment project's number for the
        // offsite-property check.
        let bucket_project = storage.bucket_project_number().await?;
        let deployment = deployment_project_number(metadata_base).await;
        confirm_id_archive_project(expected, bucket_project, deployment)
    }
}

/// The archive project's id form cannot be resolved to a number by a
/// storage-only credential, so confirm the verifiable property: the bucket
/// is not in the deployment project. `None` deployment number (metadata
/// server unreachable) confirms nothing and fails closed.
fn confirm_id_archive_project(
    expected: &str,
    bucket_project: i64,
    deployment_project: Option<i64>,
) -> Result<(), StorageError> {
    match deployment_project {
        Some(deployment) if deployment == bucket_project => Err(StorageError::Gcs {
            key: "<archive-project>".into(),
            message: format!(
                "{ARCHIVES_GCP_PROJECT_ENV} is set to {expected:?}, but the archive bucket's \
                 project ({bucket_project}) is the deployment project — archive writes refused"
            ),
        }),
        Some(_) => Ok(()),
        None => Err(StorageError::Gcs {
            key: "<archive-project>".into(),
            message: format!(
                "{ARCHIVES_GCP_PROJECT_ENV} is set to {expected:?} (a project id), and the \
                 deployment project number could not be read from the metadata server, so the \
                 archive bucket's project cannot be confirmed — archive writes refused. Set the \
                 archive project's number to verify by direct comparison"
            ),
        }),
    }
}

fn project_mismatch(expected: &str, bucket_project: i64) -> StorageError {
    StorageError::Gcs {
        key: "<archive-project>".into(),
        message: format!(
            "{ARCHIVES_GCP_PROJECT_ENV} is set to {expected}, but the archive bucket's project \
             is {bucket_project} — archive writes refused"
        ),
    }
}

/// The deployment project's number from the GKE metadata server — readable
/// by every pod with no IAM involved. `None` when unavailable (off-cluster,
/// metadata disabled): the caller treats that as "cannot confirm".
async fn deployment_project_number(base: &str) -> Option<i64> {
    let url = format!("{base}/computeMetadata/v1/project/numeric-project-id");
    let body = reqwest::Client::new()
        .get(url)
        .header("Metadata-Flavor", "Google")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    body.trim().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        confirm_id_archive_project, verify_archives_project_with, ARCHIVES_GCP_PROJECT_ENV,
    };
    use crate::{StorageError, StorageService};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A storage double whose `bucket_project_number` reports the given
    /// number and counts its invocations. Every other operation panics —
    /// this module verifies configuration, and an object write during the
    /// check would be the bug it exists to prevent.
    struct ProjectProbe {
        project: i64,
        probes: AtomicUsize,
    }

    impl ProjectProbe {
        fn of(project: i64) -> Arc<Self> {
            Arc::new(Self {
                project,
                probes: AtomicUsize::new(0),
            })
        }

        fn probe_count(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl StorageService for ProjectProbe {
        async fn put(&self, _: &str, _: &[u8], _: &str) -> Result<(), StorageError> {
            panic!("no archive write may precede the project verification")
        }

        async fn get(&self, _: &str) -> Result<crate::StoredObject, StorageError> {
            panic!("no archive read may precede the project verification")
        }

        async fn signed_url(
            &self,
            _: &str,
            _: std::time::Duration,
        ) -> Result<String, StorageError> {
            panic!("no signing may precede the project verification")
        }

        async fn delete(&self, _: &str) -> Result<(), StorageError> {
            panic!("the archives lane never deletes")
        }

        async fn bucket_project_number(&self) -> Result<i64, StorageError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Ok(self.project)
        }
    }

    fn lookup<'a>(pairs: &[(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    const DEAD_METADATA: &str = "http://127.0.0.1:1";
    const DEPLOYMENT_PROJECT: i64 = 123_456_789;

    /// A metadata-server mock reporting the deployment project's number.
    async fn metadata_server() -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/computeMetadata/v1/project/numeric-project-id",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(DEPLOYMENT_PROJECT.to_string()),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn absent_env_leaves_the_lane_unverified() {
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        verify_archives_project_with(storage.as_ref(), lookup(&[]), DEAD_METADATA)
            .await
            .expect("no env var, no check");
        assert_eq!(storage.probe_count(), 0, "no probe without the seam");
    }

    #[tokio::test]
    async fn empty_env_is_treated_as_absent() {
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "  ")]),
            DEAD_METADATA,
        )
        .await
        .expect("an empty value disables the seam");
        assert_eq!(storage.probe_count(), 0);
    }

    #[tokio::test]
    async fn matching_numeric_project_verifies() {
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "123456789")]),
            DEAD_METADATA,
        )
        .await
        .expect("the probed project matches");
        assert_eq!(storage.probe_count(), 1);
    }

    #[tokio::test]
    async fn mismatched_numeric_project_fails_closed() {
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        let error = verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "987654321")]),
            DEAD_METADATA,
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("987654321") && message.contains("123456789"),
            "names both projects: {message}"
        );
        assert_eq!(storage.probe_count(), 1);
    }

    #[tokio::test]
    async fn numeric_check_never_touches_the_metadata_server() {
        // The numeric form verifies by direct comparison; a dead metadata
        // base must not influence it.
        let storage = ProjectProbe::of(42);
        verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "42")]),
            DEAD_METADATA,
        )
        .await
        .expect("numeric verification is self-contained");
    }

    #[tokio::test]
    async fn id_form_fails_closed_without_a_deployment_number() {
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        let error = verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "neon-law-archive")]),
            DEAD_METADATA,
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("cannot be confirmed"),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn id_form_passes_when_the_bucket_is_offsite() {
        // Bucket project 999, deployment project 123456789: different, so
        // the verifiable property holds and the id annotates the digest.
        let metadata = metadata_server().await;
        let storage = ProjectProbe::of(999);
        verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "neon-law-archive")]),
            &metadata.uri(),
        )
        .await
        .expect("offsite bucket with a readable deployment project");
    }

    #[tokio::test]
    async fn id_form_rejects_a_deployment_project_bucket() {
        // The bucket probe reports the deployment project's own number:
        // the in-project shape this seam exists to refuse.
        let metadata = metadata_server().await;
        let storage = ProjectProbe::of(DEPLOYMENT_PROJECT);
        let error = verify_archives_project_with(
            storage.as_ref(),
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "neon-law-archive")]),
            &metadata.uri(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("deployment project"),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn unsupported_backend_probe_fails_closed() {
        // A backend with no owning-project concept cannot confirm anything
        // while the seam is set — including `fs` and `s3` lanes.
        let root = tempfile::tempdir().unwrap();
        let storage = crate::FsStorage::new(root.path().to_path_buf())
            .await
            .unwrap();
        let error = verify_archives_project_with(
            &storage,
            lookup(&[(ARCHIVES_GCP_PROJECT_ENV, "123456789")]),
            DEAD_METADATA,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, StorageError::Unsupported(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn id_form_confirms_offsite_projects_without_iam() {
        assert!(confirm_id_archive_project("neon-law-archive", 999, Some(123_456_789)).is_ok());
        // The deployment-equality arm fails regardless of the id spelled —
        // the numbers are what the credential can compare.
        assert!(
            confirm_id_archive_project("x", DEPLOYMENT_PROJECT, Some(DEPLOYMENT_PROJECT)).is_err()
        );
    }

    /// A minimal GCS mock: the bucket-get answers with a body carrying the
    /// given project number, and every other route 404s.
    async fn gcs_bucket_server(project_number: i64) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/storage/v1/b/archive-bucket"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "archive-bucket",
                    "name": "archive-bucket",
                    "projectNumber": project_number.to_string(),
                    "metageneration": "1",
                    "location": "US",
                    "locationType": "multi-region",
                    "storageClass": "STANDARD",
                    "etag": "CAE=",
                    "requesterPays": false,
                })),
            )
            .mount(&server)
            .await;
        server
    }

    /// The acceptance proof for the seam: with `NAVIGATOR_ARCHIVES_GCP_PROJECT_ID`
    /// set to a project the resolved bucket does not belong to, lane
    /// resolution fails before any object write reaches the wire.
    #[tokio::test]
    async fn archives_lane_refuses_to_write_when_the_project_check_fails() {
        let gcs = gcs_bucket_server(DEPLOYMENT_PROJECT).await;
        let Err(error) = crate::archives_from_lookup(lookup(&[
            ("NAVIGATOR_STORAGE_BACKEND", "gcs"),
            ("NAVIGATOR_STORAGE_ENDPOINT", &gcs.uri()),
            ("NAVIGATOR_ARCHIVES_BUCKET", "archive-bucket"),
            (ARCHIVES_GCP_PROJECT_ENV, "987654321"),
        ]))
        .await
        else {
            panic!("the lane must refuse to resolve");
        };
        assert!(
            error.to_string().contains("archive writes refused"),
            "got {error:?}"
        );
        // Every request the mock saw is the bucket probe — no object
        // upload (POST /upload/…) was attempted.
        let requests = gcs.received_requests().await.unwrap_or_default();
        assert!(
            requests
                .iter()
                .all(|request| request.url.path().starts_with("/storage/v1/b/")),
            "only the bucket probe may reach the wire: {requests:?}"
        );
    }

    /// The mirror image: a matching number resolves the handle normally.
    #[tokio::test]
    async fn archives_lane_resolves_when_the_project_matches() {
        let gcs = gcs_bucket_server(987_654_321).await;
        crate::archives_from_lookup(lookup(&[
            ("NAVIGATOR_STORAGE_BACKEND", "gcs"),
            ("NAVIGATOR_STORAGE_ENDPOINT", &gcs.uri()),
            ("NAVIGATOR_ARCHIVES_BUCKET", "archive-bucket"),
            (ARCHIVES_GCP_PROJECT_ENV, "987654321"),
        ]))
        .await
        .expect("the bucket's project matches the seam");
    }
}
