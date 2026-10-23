//! Per-deployment Google service account and GKE Workload Identity bindings.
//!
//! The embedded manifests bind both runtime Kubernetes service accounts to
//! this GSA. Provisioning it here keeps `ops gcp setup` honest for an empty
//! project: Secret Manager and bucket access need no undocumented manual IAM.

use super::client::{GcpClient, ShellResult};
use super::error::{SetupError, SetupResult};
use super::SetupConfig;

const PROJECT_ROLES: &[&str] = &[
    "roles/secretmanager.secretAccessor",
    "roles/aiplatform.user",
];
/// The release workflow gets object creation only on one deployment's logs
/// bucket. `ObjectCreator` cannot read, list, overwrite, or delete transcripts.
pub(super) const RELEASE_SHIP_ACCOUNT_ID: &str = "navigator-release-ship";
const RELEASE_TRANSCRIPT_ROLE: &str = "roles/storage.objectCreator";
#[cfg(test)]
const RELEASE_TRANSCRIPT_PERMISSIONS: &[&str] = &[
    "orgpolicy.policy.get",
    "resourcemanager.projects.get",
    "resourcemanager.projects.list",
    "storage.folders.create",
    "storage.managedFolders.create",
    "storage.multipartUploads.abort",
    "storage.multipartUploads.create",
    "storage.multipartUploads.listParts",
    "storage.objects.create",
    "storage.objects.createContext",
];
const KUBERNETES_SERVICE_ACCOUNTS: &[&str] = &["navigator-web", "workflows-service"];
/// The Kubernetes service account behind the WAL archive and base-backup
/// workloads. Object admin, because replacing the in-progress `.partial` WAL
/// segment needs delete.
const POSTGRES_BACKUP_KUBERNETES_ACCOUNT: &str = "navigator-postgres-backup";
const POSTGRES_BACKUP_ROLE: &str = "roles/storage.objectAdmin";

/// Create the runtime GSA and idempotently bind its direct GCP access. The GKE
/// workload identity bindings are a separate step because the cluster must
/// first create the project's `<project>.svc.id.goog` pool.
pub async fn ensure_runtime_identity(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    buckets: &[&str],
) -> SetupResult<()> {
    let account_id = &config.google_service_account_id;
    let gsa = format!("{account_id}@{project_id}.iam.gserviceaccount.com");

    create_service_account(
        client,
        project_id,
        account_id,
        &format!("Navigator deployment {account_id}"),
        "create deployment Google service account",
    )
    .await?;
    create_service_account(
        client,
        project_id,
        &config.drive_service_account_id,
        &format!(
            "Navigator Workspace Drive {}",
            config.drive_service_account_id
        ),
        "create deployment Workspace Drive service account",
    )
    .await?;
    bind_project_roles(client, project_id, &gsa).await?;
    bind_bucket_roles(client, &gsa, buckets).await?;
    bind_self_signing(client, project_id, &gsa).await
}

async fn create_service_account(
    client: &GcpClient,
    project_id: &str,
    account_id: &str,
    display_name: &str,
    operation: &'static str,
) -> SetupResult<()> {
    classify(
        client
            .shell_out(
                "gcloud",
                &[
                    "iam",
                    "service-accounts",
                    "create",
                    account_id,
                    "--project",
                    project_id,
                    "--display-name",
                    display_name,
                ],
            )
            .await?,
        operation,
    )
}

async fn bind_project_roles(client: &GcpClient, project_id: &str, gsa: &str) -> SetupResult<()> {
    for role in PROJECT_ROLES {
        classify(
            client
                .shell_out(
                    "gcloud",
                    &[
                        "projects",
                        "add-iam-policy-binding",
                        project_id,
                        "--member",
                        &format!("serviceAccount:{gsa}"),
                        "--role",
                        role,
                        "--condition=None",
                    ],
                )
                .await?,
            "bind deployment project role",
        )?;
    }
    Ok(())
}

async fn bind_bucket_roles(client: &GcpClient, gsa: &str, buckets: &[&str]) -> SetupResult<()> {
    for bucket in buckets {
        classify(
            client
                .shell_out(
                    "gcloud",
                    &[
                        "storage",
                        "buckets",
                        "add-iam-policy-binding",
                        &format!("gs://{bucket}"),
                        "--member",
                        &format!("serviceAccount:{gsa}"),
                        "--role",
                        "roles/storage.objectAdmin",
                    ],
                )
                .await?,
            "bind deployment bucket role",
        )?;
    }
    Ok(())
}

/// Ensure the deployment's release identity can write only fresh ship
/// transcripts under `ship/` in this deployment's own logs bucket.
pub async fn ensure_ship_transcript_writer(
    client: &GcpClient,
    project_id: &str,
    logs_bucket: &str,
) -> SetupResult<()> {
    let account_id = RELEASE_SHIP_ACCOUNT_ID;
    let gsa = format!("{account_id}@{project_id}.iam.gserviceaccount.com");
    create_service_account(
        client,
        project_id,
        account_id,
        "Navigator release ship",
        "create release ship service account",
    )
    .await?;

    // `gcloud ... add-iam-policy-binding` merges this binding into the current
    // bucket policy; rerunning setup therefore leaves an existing grant alone.
    let condition = format!(
        "expression=resource.type == 'storage.googleapis.com/Object' && \
         resource.name.startsWith('projects/_/buckets/{logs_bucket}/objects/ship/'),\
         title=navigator-ship-transcripts,description=Limit release writes to new ship transcripts"
    );
    classify(
        client
            .shell_out(
                "gcloud",
                &[
                    "storage",
                    "buckets",
                    "add-iam-policy-binding",
                    &format!("gs://{logs_bucket}"),
                    "--member",
                    &format!("serviceAccount:{gsa}"),
                    "--role",
                    RELEASE_TRANSCRIPT_ROLE,
                    "--condition",
                    &condition,
                ],
            )
            .await?,
        "bind release transcript writer role",
    )
}

/// Grant the PostgreSQL backup Kubernetes account, and only it, object admin on
/// this deployment's backups bucket. The member is the GKE Workload Identity
/// principal directly; no Google service account stands in between.
pub async fn ensure_postgres_backup_writer(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    backups_bucket: &str,
) -> SetupResult<()> {
    let member = format!(
        "serviceAccount:{project_id}.svc.id.goog[{}/{POSTGRES_BACKUP_KUBERNETES_ACCOUNT}]",
        config.kubernetes_namespace
    );
    // `gcloud ... add-iam-policy-binding` merges this binding into the current
    // bucket policy; rerunning setup therefore leaves an existing grant alone.
    classify(
        client
            .shell_out(
                "gcloud",
                &[
                    "storage",
                    "buckets",
                    "add-iam-policy-binding",
                    &format!("gs://{backups_bucket}"),
                    "--member",
                    &member,
                    "--role",
                    POSTGRES_BACKUP_ROLE,
                ],
            )
            .await?,
        "bind PostgreSQL backup bucket role",
    )
}

pub async fn bind_kubernetes_accounts(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
) -> SetupResult<()> {
    let gsa = format!(
        "{}@{project_id}.iam.gserviceaccount.com",
        config.google_service_account_id
    );
    for ksa in KUBERNETES_SERVICE_ACCOUNTS {
        let member = format!(
            "serviceAccount:{project_id}.svc.id.goog[{}/{ksa}]",
            config.kubernetes_namespace
        );
        classify(
            client
                .shell_out(
                    "gcloud",
                    &[
                        "iam",
                        "service-accounts",
                        "add-iam-policy-binding",
                        &gsa,
                        "--project",
                        project_id,
                        "--role",
                        "roles/iam.workloadIdentityUser",
                        "--member",
                        &member,
                    ],
                )
                .await?,
            "bind deployment Kubernetes service account",
        )?;
    }
    Ok(())
}

async fn bind_self_signing(client: &GcpClient, project_id: &str, gsa: &str) -> SetupResult<()> {
    classify(
        client
            .shell_out(
                "gcloud",
                &[
                    "iam",
                    "service-accounts",
                    "add-iam-policy-binding",
                    gsa,
                    "--project",
                    project_id,
                    "--role",
                    "roles/iam.serviceAccountTokenCreator",
                    "--member",
                    &format!("serviceAccount:{gsa}"),
                ],
            )
            .await?,
        "bind deployment self-signing role",
    )
}

pub(super) fn classify(result: ShellResult, operation: &'static str) -> SetupResult<()> {
    if result.succeeded() || result.is_already_exists() {
        return Ok(());
    }
    Err(SetupError::ShellFailed {
        operation,
        command: result.command_line,
        exit: result.exit,
        stderr: result.stderr,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::devx::gcp::client::StaticToken;

    #[test]
    fn release_transcript_role_has_no_object_read_list_overwrite_or_delete_permission() {
        assert_eq!(RELEASE_TRANSCRIPT_ROLE, "roles/storage.objectCreator");
        assert!(RELEASE_TRANSCRIPT_PERMISSIONS.contains(&"storage.objects.create"));
        for forbidden in [
            "storage.objects.get",
            "storage.objects.list",
            "storage.objects.update",
            "storage.objects.delete",
        ] {
            assert!(!RELEASE_TRANSCRIPT_PERMISSIONS.contains(&forbidden));
        }
    }

    #[tokio::test]
    async fn ship_transcript_grant_is_per_deployment_and_prefix_scoped() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        ensure_ship_transcript_writer(&client, "example-prod", "example-prod-logs")
            .await
            .unwrap();
        ensure_ship_transcript_writer(&client, "example-prod", "example-prod-logs")
            .await
            .unwrap();
        let calls = client.recorded_calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[1].url, calls[3].url, "the repeated binding is stable");
        assert!(calls[0].url.contains(
            "gcloud iam service-accounts create navigator-release-ship --project example-prod"
        ));
        let command = calls[1].url.as_str();
        assert!(command.contains("gs://example-prod-logs"));
        assert!(command.contains("navigator-release-ship@example-prod.iam.gserviceaccount.com"));
        assert!(command.contains(RELEASE_TRANSCRIPT_ROLE));
        assert!(command.contains("projects/_/buckets/example-prod-logs/objects/ship/"));
    }

    #[tokio::test]
    async fn postgres_backup_grant_names_only_the_backup_kubernetes_account() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        let config = SetupConfig {
            kubernetes_namespace: "example-a".into(),
            ..SetupConfig::default()
        };
        ensure_postgres_backup_writer(
            &client,
            "example-prod",
            &config,
            "example-prod-postgres-backups",
        )
        .await
        .unwrap();

        let calls = client.recorded_calls();
        assert_eq!(calls.len(), 1, "one bucket binding, no GSA: {calls:?}");
        let command = calls[0].url.as_str();
        assert!(command.contains(
            "gcloud storage buckets add-iam-policy-binding gs://example-prod-postgres-backups"
        ));
        assert!(command.contains(
            "serviceAccount:example-prod.svc.id.goog[example-a/navigator-postgres-backup]"
        ));
        assert!(command.contains("roles/storage.objectAdmin"));
        assert!(!command.contains("--condition"));
    }

    #[tokio::test]
    async fn dry_run_records_per_deployment_identity_and_bindings() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        let config = SetupConfig {
            google_service_account_id: "example-a-web".into(),
            drive_service_account_id: "example-a-drive".into(),
            kubernetes_namespace: "example-a".into(),
            ..SetupConfig::default()
        };

        ensure_runtime_identity(
            &client,
            "neon-law-test",
            &config,
            &[
                "example-a-assets",
                "example-a-documents",
                "example-a-exports",
                "example-a-logs",
                "example-a-applications",
            ],
        )
        .await
        .unwrap();
        bind_kubernetes_accounts(&client, "neon-law-test", &config)
            .await
            .unwrap();

        let plan = client
            .recorded_calls()
            .iter()
            .map(|call| format!("{} {}", call.url, call.body.as_deref().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plan.contains("example-a-web"));
        assert!(plan.contains("example-a-drive"));
        assert!(plan.contains("roles/secretmanager.secretAccessor"));
        assert!(plan.contains("roles/aiplatform.user"));
        assert!(plan.contains("roles/storage.objectAdmin"));
        assert!(plan.contains("gs://example-a-applications"));
        assert!(!plan.contains("allUsers"));
        assert!(!plan.contains("roles/storage.objectViewer"));
        assert!(plan.contains("neon-law-test.svc.id.goog[example-a/navigator-web]"));
        assert!(plan.contains("neon-law-test.svc.id.goog[example-a/workflows-service]"));
        assert!(plan.contains("roles/iam.serviceAccountTokenCreator"));
        assert_eq!(client.recorded_calls().len(), 12);
    }
}
