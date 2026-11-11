//! How long a deployment keeps its logs.
//!
//! Logs and backups are different things with different windows. A log is a
//! record of what happened — stdout, access logs, ship transcripts — kept so an
//! operator can investigate after the fact. A backup is a copy of state kept so
//! it can be restored, and its window is set by the restore it serves (see
//! [`super::buckets::POSTGRES_BACKUPS_RETENTION_DAYS`]). Neither number is
//! derived from the other.
//!
//! Two stores hold a deployment's logs, and both keep them for
//! [`LOG_RETENTION_DAYS`]:
//!
//! - Cloud Logging's `_Default` bucket, which receives every container's
//!   stdout. Google's default is 30 days; setup raises it.
//! - The `<project>-logs` GCS bucket, which receives the log sink export and
//!   ship transcripts. Its lifecycle rule comes from
//!   [`super::buckets::BucketKind::retention_days`].
//!
//! Cloud Logging's `_Required` bucket (audit logs, 400 days, locked by Google)
//! is not ours to change and is left alone.

use super::client::GcpClient;
use super::error::{SetupError, SetupResult};

/// Days a deployment keeps its logs, in Cloud Logging and in its logs bucket.
pub const LOG_RETENTION_DAYS: u32 = 180;

/// Set the project's Cloud Logging `_Default` bucket to keep entries for
/// [`LOG_RETENTION_DAYS`].
///
/// `buckets update` is idempotent: rerunning setup writes the same value.
/// `--project` is passed explicitly because the operator's ambient `gcloud`
/// project is production by default.
pub async fn ensure_default_log_retention(client: &GcpClient, project_id: &str) -> SetupResult<()> {
    let days = LOG_RETENTION_DAYS.to_string();
    let result = client
        .shell_out(
            "gcloud",
            &[
                "logging",
                "buckets",
                "update",
                "_Default",
                "--location",
                "global",
                "--retention-days",
                &days,
                "--project",
                project_id,
            ],
        )
        .await?;
    if result.succeeded() {
        return Ok(());
    }
    Err(SetupError::ShellFailed {
        operation: "set Cloud Logging _Default retention",
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
    fn logs_are_kept_for_180_days() {
        assert_eq!(LOG_RETENTION_DAYS, 180);
    }

    #[test]
    fn log_retention_is_not_the_backup_window() {
        // Backups follow their restore; logs follow investigation. A change to
        // one must not silently move the other.
        assert_ne!(
            LOG_RETENTION_DAYS,
            super::super::buckets::POSTGRES_BACKUPS_RETENTION_DAYS
        );
    }

    #[tokio::test]
    async fn default_bucket_retention_is_set_in_the_deployments_own_project() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        ensure_default_log_retention(&client, "example-stg")
            .await
            .unwrap();
        let calls = client.recorded_calls();
        assert_eq!(calls.len(), 1);
        let command = &calls[0].url;
        assert!(
            command.contains("logging buckets update _Default"),
            "{command}"
        );
        assert!(command.contains("--location global"), "{command}");
        assert!(command.contains("--retention-days 180"), "{command}");
        assert!(command.contains("--project example-stg"), "{command}");
    }
}
