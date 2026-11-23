//! A source-store identity obtained only from the GKE metadata server.
use std::{fmt, sync::Arc};

use google_cloud_auth::{
    project::{create_token_source_from_project, Config, Project, ProjectInfo},
    token_source::TokenSource,
};
use google_cloud_storage::client::ClientConfig;

use crate::StorageError;

#[derive(Clone)]
struct MetadataProvider(Arc<dyn TokenSource>);

impl fmt::Debug for MetadataProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArchiveMetadataIdentity")
    }
}
impl google_cloud_token::TokenSourceProvider for MetadataProvider {
    fn token_source(&self) -> Arc<dyn google_cloud_token::TokenSource> {
        Arc::new(self.clone())
    }
}
#[async_trait::async_trait]
impl google_cloud_token::TokenSource for MetadataProvider {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let token = self.0.token().await.map_err(|_| unavailable())?;
        if !token.valid() {
            return Err(unavailable().into());
        }
        Ok(token.value())
    }
}

fn unavailable() -> StorageError {
    StorageError::Unsupported("archive metadata identity unavailable")
}

pub(super) async fn configuration() -> Result<ClientConfig, StorageError> {
    // Construct the metadata project explicitly: no ADC file discovery, process mutation or CLI fallback.
    let project = Project::FromMetadataServer(ProjectInfo { project_id: None });
    let source = create_token_source_from_project(
        &project,
        Config::default().with_scopes(&["https://www.googleapis.com/auth/devstorage.read_write"]),
    )
    .await
    .map_err(|_| unavailable())?;
    if !source.token().await.map_err(|_| unavailable())?.valid() {
        return Err(unavailable());
    }
    Ok(ClientConfig {
        token_source_provider: Some(Box::new(MetadataProvider(source.into()))),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    #[tokio::test]
    async fn metadata_source_ignores_adc_and_never_falls_back() {
        const CHILD: &str = "NAVIGATOR_ARCHIVE_METADATA_CHILD";
        const TEST: &str =
            "gcs::archive_identity::tests::metadata_source_ignores_adc_and_never_falls_back";
        if let Ok(mode) = std::env::var(CHILD) {
            let result =
                crate::GcsStorage::archive_from_gke_metadata("example-sealed-source".into()).await;
            assert_eq!(result.is_ok(), mode == "success");
            if mode == "success" {
                let config = super::configuration().await.unwrap();
                let provider = config.token_source_provider.unwrap();
                assert_eq!(
                    provider.token_source().token().await.unwrap(),
                    "Bearer synthetic-metadata-access-token"
                );
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let gcloud = root.path().join("gcloud");
        std::fs::write(&gcloud, b"#!/bin/sh\nprintf invoked > \"$NAVIGATOR_GCLOUD_SENTINEL\"\nprintf 'synthetic-not-a-credential\\n'\n").unwrap();
        std::fs::set_permissions(&gcloud, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sentinel = root.path().join("gcloud-used");
        let server = wiremock::MockServer::start().await;
        for (mode, status, response) in [
            (
                "success",
                200,
                serde_json::json!({"access_token":"synthetic-metadata-access-token","token_type":"Bearer","expires_in":3600}),
            ),
            (
                "denied",
                403,
                serde_json::json!({"error":"synthetic metadata denial"}),
            ),
        ] {
            server.reset().await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(
                    "/computeMetadata/v1/instance/service-accounts/default/token",
                ))
                .and(wiremock::matchers::header("Metadata-Flavor", "Google"))
                .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(response))
                .expect(if mode == "success" { 2 } else { 1 })
                .mount(&server)
                .await;
            let result = tokio::process::Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .args(["--exact", TEST, "--nocapture"])
                .current_dir(root.path())
                .kill_on_drop(true)
                .env(CHILD, mode)
                .env("PATH", root.path())
                .env(
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    root.path().join("missing-adc.json"),
                )
                .env(
                    "GOOGLE_APPLICATION_CREDENTIALS_JSON",
                    "synthetic-invalid-adc",
                )
                .env(
                    "GCE_METADATA_HOST",
                    server.uri().trim_start_matches("http://"),
                )
                .env("NAVIGATOR_GCLOUD_SENTINEL", &sentinel)
                .output()
                .await
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(!sentinel.exists());
            server.verify().await;
        }
    }
}
