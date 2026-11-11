//! The push pipeline's consumption tail — journal → analysis → Slack.
//!
//! The receiver ([`crate::forgejo_webhook`]) verifies and journals a push;
//! this module is what consumes the receipt. [`JjPushPipeline`] is the
//! real [`PushConsumer`]: it resolves the Project the push landed on,
//! analyzes the pushed change through the Firm's Forgejo API
//! ([`cloud::forgejo_push_analysis`]), persists the analysis on the
//! receipt's journal row (so a redelivery never re-reads the API), renders
//! it as Block Kit ([`workflows::notify_blocks`]), and delivers to the
//! Project's Slack channel through the existing `SlackBot` machinery
//! ([`workflows::SlackBotClient::post_blocks`]).
//!
//! ## Every failure is a failed receipt, never a dropped event
//!
//! Each phase checks the last durable state and re-runs only the
//! unfinished tail. Analysis reads the receipt's persisted columns first;
//! delivery reads nothing durable to check, so it runs every consumption.
//! Any failure is returned and the receiver records it on the receipt and
//! answers 5xx so Forgejo redelivers: a push whose Slack was not yet
//! configured is delivered once the Firm connects Slack, not lost. A
//! deployment without the Firm's Forgejo credential fails the same way —
//! journaling and analysis keep working, and the journal reports why
//! nothing was delivered.
//!
//! ## Credentials
//!
//! Both the Forgejo token and the Slack bot token are the Firm's own,
//! resolved per Project from the encrypted integration secrets
//! ([`store::firm_secrets::resolve_project_credential`]). A deployment
//! whose Firm-integration runtime is unconfigured fails the delivery with
//! that fact named — it never falls back to a shared credential, the same
//! refusal `crate::integrations` makes at its boundary.
//!
//! The Project's Slack channel is the one the integrations door
//! (`integrations_api::ensure_one_channel`) already provisions and
//! records. A Project whose channel was never provisioned gets one
//! created through the same naming and `cloud::ensure_private_channel`
//! machinery, and the id is recorded — the pipeline adopts the same
//! channel the door would have made, never a second one.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use cloud::forgejo_push_analysis::{self, PushAnalysis};
use store::postgres::forgejo_push_webhook_journal::{self, ForgejoPushReceipt};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use workflows::notify::{SlackBot, SlackBotClient};
use workflows::notify_blocks::{push_blocks, ChangeNotice, PushNotice};

use crate::forgejo_webhook::PushConsumer;

/// The per-Project credentials the pipeline's tail needs: the Firm's
/// Forgejo token (to read the pushed commit) and Slack bot token (to
/// deliver). Production resolves both from the Firm's encrypted
/// integration secrets; tests inject fixed tokens
/// ([`FixedPushCredentials`]).
#[async_trait]
pub trait PushCredentials: Send + Sync {
    /// # Errors
    /// A clear, retryable reason the Firm's Forgejo token is unavailable
    /// for this Project — it is recorded on the receipt verbatim.
    async fn forgejo_token(&self, project_id: Uuid) -> Result<String, String>;

    /// # Errors
    /// A clear, retryable reason the Firm's Slack bot token is
    /// unavailable for this Project.
    async fn slack_bot_token(&self, project_id: Uuid) -> Result<String, String>;
}

/// The production resolver: the Firm's own secrets, read fresh per
/// delivery — a revoked credential stops working on the next push.
pub struct FirmPushCredentials {
    surreal: SurrealDb,
    kms: Arc<dyn cloud::RuntimeKms>,
}

impl FirmPushCredentials {
    #[must_use]
    pub fn new(surreal: SurrealDb, kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self { surreal, kms }
    }

    async fn credential(
        &self,
        project_id: Uuid,
        provider: store::firm_secrets::IntegrationProvider,
        kind: store::firm_secrets::IntegrationSecretKind,
        what: &str,
    ) -> Result<String, String> {
        store::firm_secrets::resolve_project_credential(
            &self.surreal,
            project_id,
            provider,
            kind,
            self.kms.as_ref(),
        )
        .await
        .map(|credential| credential.expose_for_provider_call().to_owned())
        .map_err(|error| format!("the Firm's {what} is unavailable: {error}"))
    }
}

#[async_trait]
impl PushCredentials for FirmPushCredentials {
    async fn forgejo_token(&self, project_id: Uuid) -> Result<String, String> {
        self.credential(
            project_id,
            store::firm_secrets::IntegrationProvider::Forgejo,
            store::firm_secrets::IntegrationSecretKind::ForgejoAccessToken,
            "forgejo_access_token secret",
        )
        .await
    }

    async fn slack_bot_token(&self, project_id: Uuid) -> Result<String, String> {
        self.credential(
            project_id,
            store::firm_secrets::IntegrationProvider::Slack,
            store::firm_secrets::IntegrationSecretKind::SlackBotToken,
            "Slack bot token secret",
        )
        .await
    }
}

/// Fixed tokens for tests: the wiremock harness injects its own and the
/// unconfigured-Slack test injects `None`.
pub struct FixedPushCredentials {
    pub forgejo_token: String,
    /// `None` stands in for a Firm that has not connected Slack.
    pub slack_bot_token: Option<String>,
}

#[async_trait]
impl PushCredentials for FixedPushCredentials {
    async fn forgejo_token(&self, _project_id: Uuid) -> Result<String, String> {
        Ok(self.forgejo_token.clone())
    }

    async fn slack_bot_token(&self, _project_id: Uuid) -> Result<String, String> {
        self.slack_bot_token.clone().ok_or_else(|| {
            "the Firm's Slack bot token is not configured; \
                            connect Slack for the Project and the receipt redelivers"
                .to_owned()
        })
    }
}

/// The pipeline's consumption tail, mounted as the receiver's consumer in
/// production (`hosting`'s state construction).
pub struct JjPushPipeline {
    /// The push journal — where the analysis is persisted and the
    /// receipt's durable state is read from.
    journal: Arc<PostgresDb>,
    surreal: SurrealDb,
    credentials: Arc<dyn PushCredentials>,
    /// `NAVIGATOR_FORGEJO_ROOT_URL`: the Forgejo origin the analysis
    /// reads and the commit links name. `None` fails analysis with the
    /// fact named — a deployment without a Forgejo host still journals.
    forgejo_root_url: Option<String>,
    /// The Slack Web API origin override for tests; `None` is production.
    slack_api_base: Option<String>,
}

impl JjPushPipeline {
    /// The deployment's pipeline: Firm secrets from the encrypted store,
    /// Forgejo origin from the environment.
    #[must_use]
    pub fn for_deployment(
        journal: Arc<PostgresDb>,
        surreal: SurrealDb,
        kms: Arc<dyn cloud::RuntimeKms>,
    ) -> Self {
        // Clone before the move: the struct literal consumes `surreal`.
        let credentials = Arc::new(FirmPushCredentials::new(surreal.clone(), kms));
        Self::with_credentials(
            journal,
            surreal,
            credentials,
            forgejo_root_url_from_env(),
            None,
        )
    }

    #[must_use]
    pub fn with_credentials(
        journal: Arc<PostgresDb>,
        surreal: SurrealDb,
        credentials: Arc<dyn PushCredentials>,
        forgejo_root_url: Option<String>,
        slack_api_base: Option<String>,
    ) -> Self {
        Self {
            journal,
            surreal,
            credentials,
            forgejo_root_url,
            slack_api_base,
        }
    }

    /// The pushed change's analysis — from the receipt's persisted
    /// columns when a previous consumption got that far, so a redelivery
    /// or post-crash replay never re-reads the Forgejo API. `None` when
    /// the receipt stopped naming a push ref — marked skipped, terminal.
    async fn analyze(
        &self,
        receipt: &ForgejoPushReceipt,
        project_id: Uuid,
    ) -> Result<Option<PushAnalysis>, String> {
        if let Some(persisted) = persisted_analysis(receipt) {
            return Ok(Some(persisted));
        }

        let (firm_slug, _) = repo_coordinates(&receipt.repo_full_name)?;
        let token = self.credentials.forgejo_token(project_id).await?;
        let root = self.forgejo_root_url.as_deref().ok_or_else(|| {
            "NAVIGATOR_FORGEJO_ROOT_URL is not configured; the pushed commit cannot be read"
                .to_owned()
        })?;
        let forge = cloud::ForgejoForge::new(root, firm_slug, &token)
            .map_err(|error| format!("building the Firm's Forgejo client: {error}"))?;

        let analyzed = forgejo_push_analysis::analyze_push(
            &forge,
            &receipt.repo_full_name,
            &receipt.ref_updates,
        )
        .await
        .map_err(|error| format!("analyzing the pushed change: {error}"))?;

        let Some(analysis) = analyzed else {
            // The receiver hands over push/* receipts only; a payload that
            // no longer names a push ref is skipped, terminal in the journal.
            forgejo_push_webhook_journal::mark_skipped(&self.journal, receipt.id)
                .await
                .map_err(|error| format!("marking the receipt skipped: {error}"))?;
            tracing::info!(
                receipt_id = %receipt.id,
                "forgejo push pipeline: no push/* ref update; receipt skipped"
            );
            return Ok(None);
        };

        let journaled = journal_analysis(&analysis);
        forgejo_push_webhook_journal::record_analysis(&self.journal, receipt.id, &journaled)
            .await
            .map_err(|error| format!("recording the analysis: {error}"))?;
        Ok(Some(analysis))
    }

    /// The Project's Slack channel id, recorded or provisioned. A
    /// provisioned channel is recorded so the next push adopts it instead
    /// of creating a second one. Provisioning runs through the
    /// `SlackService` machinery (`cloud::ensure_private_channel`), a
    /// separate client from the `SlackBot` that delivers — same token.
    async fn project_channel(
        &self,
        slack: &dyn cloud::SlackService,
        project_id: Uuid,
        project_code: &str,
    ) -> Result<String, String> {
        let project = store::projects::find_by_id(&self.surreal, project_id)
            .await
            .map_err(|error| format!("reading the Project: {error}"))?
            .ok_or_else(|| format!("no Navigator Project with code {project_code}"))?;
        if let Some(channel_id) = project.internal_slack_channel_id {
            return Ok(channel_id);
        }

        let channel_name =
            workflows::project_channel_name(cloud::labels_slack_from_staging_env(), project_code);
        let (channel, created) = cloud::ensure_private_channel(slack, &channel_name, &[])
            .await
            .map_err(|error| format!("provisioning the Project's Slack channel: {error}"))?;
        let channel_url = cloud::slack_channel_url(&channel.id);
        store::projects::set_internal_slack_channel(
            &self.surreal,
            project_id,
            &channel.id,
            &channel_url,
        )
        .await
        .map_err(|error| format!("recording the Project's Slack channel: {error}"))?;
        tracing::info!(
            project = %project_code,
            channel_id = %channel.id,
            created,
            "forgejo push pipeline: provisioned the Project's Slack channel"
        );
        Ok(channel.id)
    }

    /// Deliver one Block Kit message to the Project's channel. The
    /// receipt's push time rides the context block; the Forgejo commit
    /// link, when the origin is configured, rides it too — for operators;
    /// lawyers read the header and the section.
    async fn deliver(
        &self,
        receipt: &ForgejoPushReceipt,
        project_id: Uuid,
        analysis: &PushAnalysis,
    ) -> Result<(), String> {
        let token = self.credentials.slack_bot_token(project_id).await?;
        // The same token, two clients, both pointed at the same origin:
        // the bot delivers blocks, the service provisions a channel when
        // none is recorded.
        let slack = match self.slack_api_base.as_deref() {
            Some(base) => SlackBotClient::with_base_url(token.clone(), base),
            None => SlackBotClient::new(token.clone()),
        };
        let service = match self.slack_api_base.as_deref() {
            Some(base) => cloud::SlackClient::with_base_url(token, base),
            None => cloud::SlackClient::new(token),
        };

        let (firm_slug, project_code) = repo_coordinates(&receipt.repo_full_name)?;
        let channel_id = self
            .project_channel(&service, project_id, project_code)
            .await?;

        let commit_url = self.commit_url(receipt, firm_slug, project_code);
        let blocks = push_blocks(&PushNotice {
            project_code,
            timestamp: &receipt
                .delivered_at
                .format("%Y-%m-%d %H:%M UTC")
                .to_string(),
            changes: vec![ChangeNotice {
                analysis,
                commit_url: commit_url.as_deref(),
                image_file_id: None,
            }],
        });
        slack
            .post_blocks(&channel_id, &blocks)
            .await
            .map_err(|error| format!("Slack delivery failed: {error}"))?;
        Ok(())
    }
}

#[async_trait]
impl PushConsumer for JjPushPipeline {
    /// Consume one verified push receipt: analysis (persisted), then
    /// delivery. Errors are retryable reasons — the receiver records them
    /// on the receipt and answers 5xx so Forgejo redelivers.
    async fn consume(&self, receipt: &ForgejoPushReceipt) -> Result<(), String> {
        let (_, project_code) = repo_coordinates(&receipt.repo_full_name)?;
        let project_id = self.project_id(project_code).await?;

        let Some(analysis) = self.analyze(receipt, project_id).await? else {
            return Ok(());
        };
        self.deliver(receipt, project_id, &analysis).await
    }
}

impl JjPushPipeline {
    async fn project_id(&self, project_code: &str) -> Result<Uuid, String> {
        store::projects::id_for_code(&self.surreal, project_code)
            .await
            .ok_or_else(|| {
                format!(
                    "no Navigator Project with code {project_code}; the push is \
                     redelivered until the Project exists"
                )
            })
    }

    /// The pushed change's Forgejo commit page, for operators — only when
    /// the origin is configured.
    fn commit_url(
        &self,
        receipt: &ForgejoPushReceipt,
        firm_slug: &str,
        project_code: &str,
    ) -> Option<String> {
        let root = self.forgejo_root_url.as_deref()?.trim_end_matches('/');
        let sha = receipt.ref_updates.as_array()?.iter().find_map(|update| {
            let ref_name = update.get("ref")?.as_str()?;
            forgejo_push_analysis::change_id_from_ref(ref_name)?;
            update.get("after").and_then(serde_json::Value::as_str)
        })?;
        Some(format!("{root}/{firm_slug}/{project_code}/commit/{sha}"))
    }
}

/// The receipt's persisted analysis, when its columns are filled —
/// `None` means analysis has not run (or did not persist).
#[must_use]
fn persisted_analysis(receipt: &ForgejoPushReceipt) -> Option<PushAnalysis> {
    let change_id = receipt.change_id.as_deref()?;
    let stats = serde_json::from_value(receipt.stats.clone()?).ok()?;
    Some(PushAnalysis {
        change_id: change_id.to_owned(),
        author: receipt.author.clone()?,
        description_summary: receipt.description_summary.clone()?,
        stats,
        conflict_signal: receipt.conflict_signal?,
    })
}

/// The analysis as the journal's columns hold it — the crate-boundary
/// shape [`forgejo_push_webhook_journal::record_analysis`] persists.
#[must_use]
fn journal_analysis(analysis: &PushAnalysis) -> forgejo_push_webhook_journal::PushAnalysis {
    forgejo_push_webhook_journal::PushAnalysis {
        change_id: analysis.change_id.clone(),
        author: analysis.author.clone(),
        description_summary: analysis.description_summary.clone(),
        stats: serde_json::to_value(analysis.stats).unwrap_or_default(),
        conflict_signal: analysis.conflict_signal,
    }
}

/// The repository coordinates a receipt names: `{firm-slug}/{project-code}`,
/// the namespace Forgejo hosts Projects under. A name without both halves
/// cannot be resolved — a signed payload that malformed is a real anomaly.
fn repo_coordinates(repo_full_name: &str) -> Result<(&str, &str), String> {
    repo_full_name
        .split_once('/')
        .ok_or_else(|| format!("malformed repository name {repo_full_name:?}"))
}

/// The Forgejo origin from the environment; unset or empty is unconfigured.
fn forgejo_root_url_from_env() -> Option<String> {
    std::env::var(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{persisted_analysis, repo_coordinates};

    use store::postgres::forgejo_push_webhook_journal::{ForgejoPushReceipt, ReceiptStatus};
    use uuid::Uuid;

    fn receipt(change_id: Option<&str>) -> ForgejoPushReceipt {
        let now = chrono::Utc::now();
        ForgejoPushReceipt {
            id: Uuid::new_v4(),
            delivered_at: now,
            repo_full_name: "acme-law/sample-estate".to_owned(),
            ref_updates: serde_json::json!([{
                "ref": "refs/heads/push/qvrpxyq",
                "before": "0000000000000000000000000000000000000000",
                "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1",
            }]),
            event_key: "event-key".to_owned(),
            payload: "{}".to_owned(),
            status: ReceiptStatus::Analyzed,
            failure_detail: None,
            change_id: change_id.map(str::to_owned),
            author: change_id.map(|_| "T-bot (tbot@neonlaw.org)".to_owned()),
            description_summary: change_id.map(|_| "Amend the fee schedule".to_owned()),
            stats: change_id
                .map(|_| serde_json::json!({ "insertions": 12, "deletions": 3, "files": 1 })),
            conflict_signal: change_id.map(|_| false),
            inserted_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_receipts_persisted_columns_rebuild_its_analysis() {
        let analysis =
            persisted_analysis(&receipt(Some("qvrpxyq"))).expect("the columns are the analysis");
        assert_eq!(analysis.change_id, "qvrpxyq");
        assert_eq!(analysis.author, "T-bot (tbot@neonlaw.org)");
        assert_eq!(analysis.description_summary, "Amend the fee schedule");
        assert_eq!(analysis.stats.insertions, 12);
        assert_eq!(analysis.stats.deletions, 3);
        assert_eq!(analysis.stats.files, 1);
        assert!(!analysis.conflict_signal);
    }

    #[test]
    fn a_receipt_without_analysis_columns_has_none() {
        assert!(persisted_analysis(&receipt(None)).is_none());
    }

    #[test]
    fn repository_coordinates_split_on_the_one_slash() {
        assert_eq!(
            repo_coordinates("acme-law/sample-estate").unwrap(),
            ("acme-law", "sample-estate")
        );
        assert!(repo_coordinates("sample-estate").is_err());
    }
}
