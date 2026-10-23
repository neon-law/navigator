//! The daily Notation draft reset, independent of the durable engine that
//! runs it.
//!
//! The draft predicate and the deletes are [`store::notation_reset`]'s. This
//! module owns what one run discovers, the counts it reports, and the one
//! message it posts to the deployment's internal messages channel. The
//! Restate workflow (`workflows-service`) and the DBOS workflow
//! ([`crate::dbos_engine::notation_reset`]) both run this core.
//!
//! Every message names the environment, counts, a duration, and the ids that
//! failed to delete; never a client, a matter, or a Project code.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::notation_reset::{NotationResetError, ResetOutcome};
use store::surreal::SurrealDb;
use uuid::Uuid;

/// The run's key for the UTC day containing `now`.
#[must_use]
pub fn run_key(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d").to_string()
}

/// How the reset names its deployment in Slack. A simulated-matters
/// deployment is `staging` whatever its profile says; a `dev` boot is `dev`.
#[must_use]
pub fn environment_label(
    environment: store::DeploymentEnvironment,
    simulated: bool,
) -> &'static str {
    match (environment, simulated) {
        (store::DeploymentEnvironment::Dev, _) => "dev",
        (store::DeploymentEnvironment::Production, true) => "staging",
        (store::DeploymentEnvironment::Production, false) => "production",
    }
}

/// What one run found to delete. Ids only.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DraftRoster {
    pub notations: Vec<Uuid>,
    pub previews: Vec<Uuid>,
}

/// Counts for one run. Safe for the journal and for the messages channel.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NotationResetReport {
    /// Notation drafts deleted with their dependents.
    pub notations_deleted: usize,
    /// Stored preview drafts deleted.
    pub previews_deleted: usize,
    /// Candidates that had advanced past draft by the time they were read
    /// again inside the delete, and were kept.
    pub kept: usize,
    /// Candidates whose delete failed after its retries. Ids only.
    pub failures: Vec<Uuid>,
    pub duration_ms: u64,
}

impl NotationResetReport {
    /// Count one Notation delete's outcome.
    pub fn absorb(&mut self, outcome: ResetOutcome) {
        match outcome {
            ResetOutcome::Deleted => self.notations_deleted += 1,
            ResetOutcome::Kept => self.kept += 1,
            ResetOutcome::AlreadyGone => {}
        }
    }
}

/// Every draft id on the deployment, Notations and stored previews.
///
/// # Errors
///
/// Fails when either list cannot be read.
pub async fn discover_drafts(surreal: &SurrealDb) -> Result<DraftRoster, NotationResetError> {
    Ok(DraftRoster {
        notations: store::notation_reset::list_drafts(surreal).await?,
        previews: store::notation_reset::list_preview_drafts(surreal).await?,
    })
}

/// The messages channel a run posts to: the staging channel on a
/// simulated-matters deployment, the real one otherwise.
#[must_use]
pub fn channel_id<'a>(simulated: bool, messages: &'a str, staging_messages: &'a str) -> &'a str {
    if simulated {
        staging_messages
    } else {
        messages
    }
}

/// Milliseconds from `started` to `finished`, never negative.
#[must_use]
pub fn elapsed_ms(started: DateTime<Utc>, finished: DateTime<Utc>) -> u64 {
    u64::try_from((finished - started).num_milliseconds().max(0)).unwrap_or(u64::MAX)
}

fn render_duration(duration_ms: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // A run lasts seconds, not 2^53 ms.
    let seconds = duration_ms as f64 / 1000.0;
    format!("{seconds:.1}s")
}

/// The one message a completed run posts. Counts, a duration, and failing
/// ids — nothing that names a client, a matter, or a Project.
#[must_use]
pub fn render_report(environment: &str, report: &NotationResetReport) -> String {
    let failures = if report.failures.is_empty() {
        "none".to_string()
    } else {
        let ids: Vec<String> = report.failures.iter().map(Uuid::to_string).collect();
        format!("{} — {}", ids.len(), ids.join(", "))
    };
    format!(
        "*Notation reset complete* ({environment})\nDrafts deleted: {} notations, {} previews. Kept because no longer a draft: {}.\nDuration: {}.\nFailures: {failures}.",
        report.notations_deleted,
        report.previews_deleted,
        report.kept,
        render_duration(report.duration_ms),
    )
}

/// The message a run posts when it could not even discover the drafts.
#[must_use]
pub fn render_failure(environment: &str, duration_ms: u64) -> String {
    format!(
        "*Notation reset failed* ({environment})\nDiscovering drafts failed before any delete; nothing was removed.\nDuration: {}.",
        render_duration(duration_ms)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().expect("timestamp")
    }

    #[test]
    fn the_key_is_the_utc_date() {
        assert_eq!(run_key(at("2026-10-10T04:20:00Z")), "2026-10-10");
        assert_eq!(
            run_key(at("2026-10-10T04:20:00Z")),
            run_key(at("2026-10-10T23:59:59Z"))
        );
    }

    #[test]
    fn the_environment_label_separates_staging_from_production() {
        assert_eq!(
            environment_label(store::DeploymentEnvironment::Production, false),
            "production"
        );
        assert_eq!(
            environment_label(store::DeploymentEnvironment::Production, true),
            "staging"
        );
        assert_eq!(
            environment_label(store::DeploymentEnvironment::Dev, true),
            "dev"
        );
    }

    #[test]
    fn a_simulated_deployment_posts_to_the_staging_channel() {
        assert_eq!(channel_id(true, "C-MESSAGES", "C-STAGING"), "C-STAGING");
        assert_eq!(channel_id(false, "C-MESSAGES", "C-STAGING"), "C-MESSAGES");
    }

    #[test]
    fn elapsed_time_is_never_negative() {
        let start = at("2026-10-10T04:20:00Z");
        assert_eq!(elapsed_ms(start, at("2026-10-10T04:20:01.5Z")), 1_500);
        assert_eq!(elapsed_ms(start, at("2026-10-10T04:19:00Z")), 0);
    }

    #[test]
    fn the_outcome_carries_environment_counts_duration_and_failing_ids_only() {
        let failing: Uuid = "00000000-0000-0000-0000-0000000000ab".parse().unwrap();
        let message = render_report(
            "staging",
            &NotationResetReport {
                notations_deleted: 3,
                previews_deleted: 1,
                kept: 2,
                failures: vec![failing],
                duration_ms: 1_234,
            },
        );
        assert_eq!(
            message,
            "*Notation reset complete* (staging)\nDrafts deleted: 3 notations, 1 previews. Kept because no longer a draft: 2.\nDuration: 1.2s.\nFailures: 1 — 00000000-0000-0000-0000-0000000000ab."
        );
        let zero = render_report("production", &NotationResetReport::default());
        assert!(
            zero.contains("Drafts deleted: 0 notations, 0 previews"),
            "{zero}"
        );
        assert!(zero.contains("Failures: none."), "{zero}");
        let failed = render_failure("production", 300);
        assert!(
            failed.starts_with("*Notation reset failed* (production)"),
            "{failed}"
        );
        assert!(failed.contains("Duration: 0.3s."), "{failed}");
    }

    /// Nothing the reset posts can carry a client, a matter, a Project code,
    /// or an address — the message is built from counts and ids alone.
    #[test]
    fn the_messages_name_no_client_matter_or_project() {
        let report = NotationResetReport {
            failures: vec![Uuid::nil()],
            ..Default::default()
        };
        for message in [
            render_report("production", &report),
            render_failure("staging", 0),
        ] {
            let lower = message.to_ascii_lowercase();
            assert!(!lower.contains('@'), "{message}");
            assert!(!lower.contains("sample-"), "{message}");
            assert!(!lower.contains("client"), "{message}");
            assert!(!lower.contains("matter"), "{message}");
            assert!(!lower.contains("project"), "{message}");
        }
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains('@'));
        assert!(!json.contains("code"));
        assert!(!json.contains("name"));
    }
}
