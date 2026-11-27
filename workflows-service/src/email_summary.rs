//! Durable execution for one archived support-email summary.
//!
//! The webhook submits only an opaque receipt ID and immutable run
//! configuration. Every archive read, provider request, and Slack delivery is
//! a named DBOS step whose persisted result is reused on recovery.

use std::sync::Arc;
use std::time::Duration;

use cloud::{GeminiVertexAdapter, MetadataTokenSource, StorageService, VertexError, VertexRequest};

use serde::{Deserialize, Serialize};
use store::email_receipts::EmailReceipt;

use uuid::Uuid;

use workflows::{
    build_prompt, deliver_summary, normalize_email, parse_summary, scoped_email_digest, sender,
    DeliveryResult, EmailSummaryRunConfig, ProviderDelivery, Sender, SlackBot,
    SummaryDeliveryError, SummaryDeliveryMessage, SummaryProvider, DEFAULT_MAX_INPUT_CHARS,
    DEFAULT_MAX_OUTPUT_TOKENS,
};

use crate::workflow_step_error::WorkflowStepError;

const DEFAULT_METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const PROVIDER_ATTEMPTS: usize = 3;
const SUMMARY_OUTPUT_MAX_CHARS: usize = 8_000;

/// The provider transport, authenticated with the worker's Workload Identity
/// token source. Gemini is the only summary provider; see `cloud::vertex`.
#[derive(Clone)]
pub struct SummaryProviders {
    pub gemini: GeminiVertexAdapter,
}

impl SummaryProviders {
    /// Build the production transports only when the shared summary feature
    /// configuration is enabled and complete. The default-off row leaves the
    /// service registered but provider-disabled.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable seam for [`Self::from_env`]. `GOOGLE_METADATA_URL` and
    /// `NAVIGATOR_SUMMARY_VERTEX_BASE_URL` point the transports at local test
    /// doubles and are honoured only under `NAVIGATOR_CI_HARNESS=1`; a hosted
    /// profile that sets either is refused.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> anyhow::Result<Option<Self>> {
        if workflows::EmailSummaryConfig::from_lookup(&get)?.is_none() {
            return Ok(None);
        }
        let metadata_url = workflows::harness_override(&get, "GOOGLE_METADATA_URL")
            .map_err(anyhow::Error::msg)?
            .unwrap_or_else(|| DEFAULT_METADATA_TOKEN_URL.to_string());
        let base_url = workflows::harness_override(&get, "NAVIGATOR_SUMMARY_VERTEX_BASE_URL")
            .map_err(anyhow::Error::msg)?;
        let token_source = Arc::new(MetadataTokenSource::new(metadata_url)?);
        let mut gemini = GeminiVertexAdapter::new(token_source)?;
        if let Some(base_url) = base_url {
            gemini = gemini.with_base_url(base_url);
        }
        Ok(Some(Self { gemini }))
    }
}

/// Outcome of the `check-sender` step. A Person row alone is not authorization
/// to send content to a model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum SenderGate {
    /// Kept in the serialized workflow contract for already journaled runs;
    /// current admission requires an explicit verified identity approval.
    Known,
    Unknown {
        reason: String,
    },
}

/// Fail closed until the receipt is linked to an explicitly approved,
/// provider-verified sender identity in the owning Firm and provider account.
/// A Person row is not evidence of that approval.
pub async fn check_sender(
    storage: Arc<dyn StorageService>,
    db: &store::application_store::ApplicationStore,
    receipt: &EmailReceipt,
    matter_project_id: Option<Uuid>,
) -> Result<SenderGate, WorkflowStepError> {
    let Some(matter_project_id) = matter_project_id else {
        return Ok(SenderGate::Unknown {
            reason: "message is not assigned to a Project".to_string(),
        });
    };
    let object = storage
        .get(&receipt.archive_key)
        .await
        .map_err(WorkflowStepError::retryable)?;
    let reason = match sender(&object.bytes) {
        Some(Sender::Email(email)) => {
            match (receipt.firm_id, receipt.provider_account.as_deref()) {
                (Some(firm_id), Some(account)) => {
                    if !receipt.sender_authenticated {
                        return Ok(SenderGate::Unknown {
                            reason: "sender domain did not pass provider authentication"
                                .to_string(),
                        });
                    }
                    let person = store::persons::find_by_email_ci(db, &email)
                        .await
                        .map_err(|error| WorkflowStepError::retryable(anyhow::Error::new(error)))?;
                    match person.filter(|person| person.email_confirmed) {
                        Some(person) => {
                            if store::summary_sender_approvals::is_approved(
                                db,
                                firm_id,
                                matter_project_id,
                                account,
                                person.id,
                                &email,
                            )
                            .await
                            .map_err(|error| {
                                WorkflowStepError::retryable(anyhow::Error::new(error))
                            })? {
                                return Ok(SenderGate::Known);
                            }
                            "sender identity is not approved for this provider account"
                        }
                        None => "sender identity is not approved for this provider account",
                    }
                }
                _ => "sender identity is not approved for this provider account",
            }
        }
        Some(Sender::Phone(_)) => "phone identity is not approved for this provider account",
        None => "no readable sender",
    };
    Ok(SenderGate::Unknown {
        reason: reason.to_string(),
    })
}

pub(crate) async fn check_sender_step(
    storage: Arc<dyn StorageService>,
    application_store: &store::application_store::ApplicationStore,
    receipt: &EmailReceipt,
    matter_project_id: Option<Uuid>,
) -> Result<SenderGate, WorkflowStepError> {
    let started = std::time::Instant::now();
    let result = check_sender(storage, application_store, receipt, matter_project_id).await;
    telemetry::record_email_stage(
        &receipt.id.to_string(),
        telemetry::EmailStage::SenderGate,
        match &result {
            Ok(SenderGate::Known) => telemetry::EmailOutcome::Succeeded,
            Ok(SenderGate::Unknown { .. }) => telemetry::EmailOutcome::Skipped,
            Err(_) => telemetry::EmailOutcome::Failed,
        },
        started.elapsed(),
    );
    if matches!(result, Ok(SenderGate::Unknown { .. })) {
        telemetry::record_email_stage(
            &receipt.id.to_string(),
            telemetry::EmailStage::Summary,
            telemetry::EmailOutcome::Skipped,
            Duration::ZERO,
        );
    }
    result
}

/// Invocation result contains identifiers and status only. Provider content
/// is retained in the journaled result and the bounded private Slack message,
/// never in worker logs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EmailSummaryReport {
    pub receipt_id: uuid::Uuid,
    pub delivery_state: String,
}

fn request_for(
    receipt: &EmailReceipt,
    config: &EmailSummaryRunConfig,
    project_id: &str,
    prompt: String,
) -> VertexRequest {
    VertexRequest {
        project_id: project_id.to_string(),
        model: config.model.clone(),
        location: config.location.clone(),
        prompt_version: config.prompt_version.clone(),
        input_digest: receipt.raw_digest.clone(),
        prompt,
        max_output_tokens: config.max_output_tokens.min(DEFAULT_MAX_OUTPUT_TOKENS),
        response_schema: Some(workflows::summary_response_schema()),
        system_instruction: None,
    }
}

async fn call_provider(
    providers: &SummaryProviders,
    provider: SummaryProvider,
    request: &VertexRequest,
) -> Result<cloud::VertexResponse, VertexError> {
    match provider {
        SummaryProvider::Gemini => providers.gemini.generate_content(request).await,
    }
}

fn retryable(error: &VertexError) -> bool {
    matches!(
        error,
        VertexError::Retryable { .. } | VertexError::TransportRetryable
    )
}

/// The `run` handler's per-provider step, outside `ctx.run`'s journaling —
/// pub so a full-pipeline test (`server`'s intake -> archive -> handler
/// coverage, ENG-889) can call the same digest-checking, provider-calling
/// logic the real workflow runs, without needing a live Restate broker.
pub async fn summarize_provider(
    storage: Arc<dyn StorageService>,
    providers: Option<SummaryProviders>,
    provider: SummaryProvider,
    receipt: EmailReceipt,
    config: EmailSummaryRunConfig,
    project_id: String,
) -> Result<ProviderDelivery, WorkflowStepError> {
    summarize_provider_inner(
        storage, None, providers, provider, receipt, config, project_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn summarize_provider_authorized(
    storage: Arc<dyn StorageService>,
    application_store: store::application_store::ApplicationStore,
    providers: Option<SummaryProviders>,
    provider: SummaryProvider,
    receipt: EmailReceipt,
    config: EmailSummaryRunConfig,
    project_id: String,
    matter_project_id: Option<Uuid>,
) -> Result<ProviderDelivery, WorkflowStepError> {
    summarize_provider_inner(
        storage,
        Some((application_store, matter_project_id)),
        providers,
        provider,
        receipt,
        config,
        project_id,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn summarize_provider_inner(
    storage: Arc<dyn StorageService>,
    authorization: Option<(store::application_store::ApplicationStore, Option<Uuid>)>,
    providers: Option<SummaryProviders>,
    provider: SummaryProvider,
    receipt: EmailReceipt,
    config: EmailSummaryRunConfig,
    project_id: String,
) -> Result<ProviderDelivery, WorkflowStepError> {
    let model = config.model.clone();
    let Some(providers) = providers else {
        return Ok(ProviderDelivery::Failed {
            model,
            status: "provider_not_configured".to_string(),
        });
    };

    let object = storage
        .get(&receipt.archive_key)
        .await
        .map_err(WorkflowStepError::retryable)?;
    let input = match normalize_email(
        &object.bytes,
        config.max_input_chars.min(DEFAULT_MAX_INPUT_CHARS),
    ) {
        Ok(input) => input,
        Err(error) => {
            return Ok(ProviderDelivery::Failed {
                model,
                status: format!("invalid_input:{error}"),
            });
        }
    };
    // Prove the archived bytes are the ones this receipt admitted, under the
    // receipt's own deployment and mailbox scope, and that the immutable run
    // configuration was captured for the same receipt.
    let archived_digest = scoped_email_digest(
        &receipt.deployment,
        &receipt.receiving_mailbox,
        &object.bytes,
    );
    if archived_digest != receipt.raw_digest || config.input_digest != receipt.raw_digest {
        return Ok(ProviderDelivery::Failed {
            model,
            status: "input_digest_mismatch".to_string(),
        });
    }
    let prompt = build_prompt(&input, &config);
    let request = request_for(&receipt, &config, &project_id, prompt);
    let mut last_error = None;
    for attempt in 0..PROVIDER_ATTEMPTS {
        // Recheck after validation and immediately before each provider
        // dispatch. Approval or Client DRI assignment may have changed since
        // durable workflow admission, or during a retry delay.
        if let Some((application_store, matter_project_id)) = &authorization {
            if let SenderGate::Unknown { reason } = check_sender(
                Arc::clone(&storage),
                application_store,
                &receipt,
                *matter_project_id,
            )
            .await?
            {
                return Ok(ProviderDelivery::Skipped { reason });
            }
        }
        let attempt_started = std::time::Instant::now();
        match call_provider(&providers, provider, &request).await {
            Ok(response) => {
                return Ok(summary_outcome(response, model));
            }
            Err(error) if retryable(&error) && attempt + 1 < PROVIDER_ATTEMPTS => {
                telemetry::record_email_stage(
                    &receipt.id.to_string(),
                    telemetry::EmailStage::Summary,
                    telemetry::EmailOutcome::Retry,
                    attempt_started.elapsed(),
                );
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }
    Ok(ProviderDelivery::Failed {
        model,
        status: format!(
            "provider_error:{}",
            last_error.map_or_else(|| "unknown".into(), |e| e.to_string())
        ),
    })
}

/// Turn one provider response into a delivery outcome. A candidate that did
/// not finish with `STOP` is named as truncated and never parsed; statuses
/// carry the finish reason and token counts only, never provider text.
fn summary_outcome(response: cloud::VertexResponse, model: String) -> ProviderDelivery {
    let counts = |response: &cloud::VertexResponse| {
        [
            ("output_tokens", response.output_tokens),
            ("thought_tokens", response.thought_tokens),
        ]
        .into_iter()
        .filter_map(|(name, tokens)| tokens.map(|tokens| format!(" {name}={tokens}")))
        .collect::<String>()
    };
    if let Some(reason) = response.finish_reason.as_deref().filter(|r| *r != "STOP") {
        return ProviderDelivery::Failed {
            model,
            status: format!(
                "output_truncated:finish_reason={reason}{}",
                counts(&response)
            ),
        };
    }
    match parse_summary(&response.text, SUMMARY_OUTPUT_MAX_CHARS) {
        Ok(result) => ProviderDelivery::Succeeded {
            model: response.model,
            result,
        },
        Err(error) => {
            let reason = response
                .finish_reason
                .as_deref()
                .map_or_else(String::new, |reason| format!(" finish_reason={reason}"));
            ProviderDelivery::Failed {
                model,
                status: format!("invalid_output:{error}{reason}{}", counts(&response)),
            }
        }
    }
}

async fn delivery_error(
    error: SummaryDeliveryError,
    db: &store::application_store::ApplicationStore,
    receipt_id: uuid::Uuid,
) -> Result<String, WorkflowStepError> {
    if matches!(error, SummaryDeliveryError::Slack(_)) {
        if let Some(delivery) = store::email_deliveries::find(db, receipt_id)
            .await
            .map_err(WorkflowStepError::retryable)?
        {
            if delivery.state == store::email_deliveries::UNKNOWN {
                return Ok("unknown".to_string());
            }
        }
        return Err(WorkflowStepError::terminal(error.to_string()));
    }
    Err(WorkflowStepError::retryable(error))
}

/// Everything the `deliver-slack` step needs, so the handler stays short.
/// Shared by the Restate handler and the DBOS twin (`dbos_email_summary`).
#[derive(Clone)]
pub(crate) struct DeliveryTarget {
    pub(crate) application_store: store::application_store::ApplicationStore,
    pub(crate) slack: Arc<dyn SlackBot>,
    pub(crate) storage: Arc<dyn StorageService>,
    pub(crate) receipt: EmailReceipt,
    pub(crate) config: EmailSummaryRunConfig,
    pub(crate) channel_id: String,
}

impl DeliveryTarget {
    /// Post the summary and thread the full body under it. The body is shown
    /// only when the archive still matches the receipt's digest.
    pub(crate) async fn deliver(
        self,
        gemini: ProviderDelivery,
    ) -> Result<String, WorkflowStepError> {
        let receipt = &self.receipt;
        let object = self
            .storage
            .get(&receipt.archive_key)
            .await
            .map_err(WorkflowStepError::retryable)?;
        let input = normalize_email(
            &object.bytes,
            self.config.max_input_chars.min(DEFAULT_MAX_INPUT_CHARS),
        )
        .ok();
        let archived_digest = scoped_email_digest(
            &receipt.deployment,
            &receipt.receiving_mailbox,
            &object.bytes,
        );
        let original_message = if archived_digest == receipt.raw_digest
            && self.config.input_digest == receipt.raw_digest
        {
            input.map_or_else(String::new, |input| input.body)
        } else {
            String::new()
        };
        let message = SummaryDeliveryMessage {
            receipt_id: receipt.id,
            gemini,
            original_message,
        };
        match deliver_summary(
            &self.application_store,
            self.slack,
            receipt.id,
            &self.channel_id,
            &message,
        )
        .await
        {
            Ok(DeliveryResult::Sent(_) | DeliveryResult::AlreadyConfirmed) => {
                Ok("confirmed".to_string())
            }
            Ok(DeliveryResult::Unknown) => Ok("unknown".to_string()),
            Err(error) => delivery_error(error, &self.application_store, receipt.id).await,
        }
    }
}

/// The root span of an `EmailSummary::run` invocation. Identifiers only.
#[cfg(test)]
fn run_span(receipt_id: uuid::Uuid, invocation_id: impl std::fmt::Display) -> tracing::Span {
    tracing::info_span!(
        "email_summary.run",
        %receipt_id,
        invocation_id = %invocation_id,
        navigator.keep = true,
    )
}

#[cfg(test)]
mod tests {
    use super::{request_for, retryable, run_span, summarize_provider, SummaryProviders};
    use async_trait::async_trait;
    use chrono::Utc;
    use cloud::{StaticTokenSource, StorageError, StoredObject, VertexError};
    use std::sync::Arc;
    use store::email_receipts::EmailReceipt;
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use workflows::{EmailSummaryRunConfig, ProviderDelivery, SummaryProvider};

    #[test]
    fn the_run_root_span_is_kept_by_the_collector() {
        let fields = crate::span_capture::fields_of(|| run_span(Uuid::now_v7(), "inv_1"));
        assert_eq!(
            fields.get("navigator.keep").map(String::as_str),
            Some("true")
        );
    }

    #[derive(Clone)]
    struct MemoryStorage {
        bytes: Option<Vec<u8>>,
    }

    #[async_trait]
    impl cloud::StorageService for MemoryStorage {
        async fn put(
            &self,
            _key: &str,
            _bytes: &[u8],
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Unsupported("test"))
        }

        async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
            self.bytes
                .clone()
                .map(|bytes| StoredObject {
                    key: key.to_string(),
                    bytes,
                    content_type: "message/rfc822".to_string(),
                })
                .ok_or_else(|| StorageError::NotFound(key.to_string()))
        }

        async fn delete(&self, _key: &str) -> Result<(), StorageError> {
            Err(StorageError::Unsupported("test"))
        }

        async fn signed_url(
            &self,
            _key: &str,
            _expires_in: std::time::Duration,
        ) -> Result<String, StorageError> {
            Err(StorageError::Unsupported("test"))
        }
    }

    fn receipt(raw_digest: &str) -> EmailReceipt {
        let now = Utc::now();
        EmailReceipt {
            id: Uuid::now_v7(),
            receiving_mailbox: "support@example.com".to_string(),
            deployment: "staging".to_string(),
            firm_id: Some(Uuid::now_v7()),
            provider_account: Some("sendgrid-example".to_string()),
            sender_authenticated: true,
            raw_digest: raw_digest.to_string(),
            source_message_id: None,
            archive_key: "inbound/example.eml".to_string(),
            letter_id: Uuid::now_v7(),
            processing_state: "pending".to_string(),
            delivery_state: "not_attempted".to_string(),
            inserted_at: now,
            updated_at: now,
        }
    }

    /// The digest intake stores for `archive` under [`receipt`]'s scope.
    fn intake_digest(archive: &[u8]) -> String {
        workflows::scoped_email_digest("staging", "support@example.com", archive)
    }

    const ARCHIVE: &[u8] = b"From: sender@example.com\r\n\
        To: intake@example.com\r\n\
        Content-Type: text/plain\r\n\r\n\
        A bounded summary body\r\n";

    const GEMINI_PATH: &str = "/v1/projects/synthetic-project/locations/global/publishers/google/models/summary-model:generateContent";

    fn config(provider: SummaryProvider, digest: &str) -> EmailSummaryRunConfig {
        EmailSummaryRunConfig::new(provider, "summary-model", "global", "summary-v1", digest)
            .expect("valid test config")
    }

    fn providers(base_url: &str) -> SummaryProviders {
        let token = Arc::new(StaticTokenSource::new("test-token"));
        SummaryProviders {
            gemini: cloud::GeminiVertexAdapter::new(token)
                .expect("gemini adapter")
                .with_base_url(base_url),
        }
    }

    fn summary_env<'a>(extra: &[(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let mut values = vec![
            ("NAVIGATOR_SUMMARY_ENABLED", "1"),
            (
                "NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS",
                "support@example.com",
            ),
            ("SENDGRID_INBOUND_PUBLIC_KEY", "key"),
            ("NAVIGATOR_DEPLOYMENT_ID", "staging"),
            (
                workflows::trigger::DBOS_INGRESS_URL_ENV,
                "https://worker.example.test",
            ),
            (
                workflows::trigger::DBOS_INGRESS_TOKEN_ENV,
                "synthetic-worker-token",
            ),
            ("NAVIGATOR_GCP_PROJECT_ID", "synthetic-project"),
            ("NAVIGATOR_SUMMARY_CHANNEL_ID", "C-SYNTHETIC"),
            (
                "NAVIGATOR_SUMMARY_FIRM_ID",
                "00000000-0000-4000-8000-000000000001",
            ),
            ("NAVIGATOR_SUMMARY_PROVIDER_ACCOUNT", "sendgrid-example"),
        ];
        values.extend_from_slice(extra);
        move |key: &str| {
            values
                .iter()
                .rev()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn providers_are_absent_when_the_lane_is_disabled() {
        assert!(SummaryProviders::from_lookup(|_| None).unwrap().is_none());
    }

    #[test]
    fn the_vertex_overrides_are_refused_outside_the_ci_harness() {
        let hosted = summary_env(&[
            (
                "NAVIGATOR_SUMMARY_VERTEX_BASE_URL",
                "http://fixture.invalid",
            ),
            ("GOOGLE_METADATA_URL", "http://fixture.invalid/token"),
        ]);
        assert!(SummaryProviders::from_lookup(hosted).is_err());

        let harness = summary_env(&[
            (
                "NAVIGATOR_SUMMARY_VERTEX_BASE_URL",
                "http://fixture.invalid",
            ),
            ("GOOGLE_METADATA_URL", "http://fixture.invalid/token"),
            ("NAVIGATOR_CI_HARNESS", "1"),
            (
                workflows::trigger::DBOS_INGRESS_URL_ENV,
                "http://127.0.0.1:8080",
            ),
        ]);
        assert!(SummaryProviders::from_lookup(harness).unwrap().is_some());

        let only_metadata = summary_env(&[("GOOGLE_METADATA_URL", "http://fixture.invalid/token")]);
        assert!(SummaryProviders::from_lookup(only_metadata).is_err());
    }

    #[test]
    fn only_transient_vertex_failures_retry() {
        assert!(retryable(&VertexError::TransportRetryable));
        assert!(retryable(&VertexError::Retryable {
            status: None,
            retry_after: None,
        }));
        assert!(!retryable(&VertexError::Configuration));
        assert!(!retryable(&VertexError::InvalidModelOutput));
    }

    #[test]
    fn request_carries_the_receipt_digest_and_bounded_output_limit() {
        let digest = "a".repeat(64);
        let receipt = receipt(&digest);
        let config = config(SummaryProvider::Gemini, &digest).with_limits(4_000, 99_999);
        let request = request_for(&receipt, &config, "synthetic-project", "prompt".to_string());
        assert_eq!(request.project_id, "synthetic-project");
        assert_eq!(request.input_digest, digest);
        assert_eq!(
            request.max_output_tokens,
            workflows::DEFAULT_MAX_OUTPUT_TOKENS
        );
        assert_eq!(
            request.response_schema,
            Some(workflows::summary_response_schema())
        );
        assert_eq!(request.prompt, "prompt");
    }

    #[tokio::test]
    async fn missing_provider_configuration_is_a_bounded_failure() {
        let digest = "a".repeat(64);
        let result = summarize_provider(
            Arc::new(MemoryStorage { bytes: None }),
            None,
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("missing provider is represented in the summary");
        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status == "provider_not_configured"
        ));
    }

    #[tokio::test]
    async fn malformed_archive_is_not_sent_to_a_provider() {
        let digest = "a".repeat(64);
        let server = MockServer::start().await;
        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(b"not a MIME message".to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("malformed archive is represented in the summary");
        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status.starts_with("invalid_input:")
        ));
    }

    #[tokio::test]
    async fn digest_mismatch_is_rejected_before_provider_call() {
        let archive = b"From: sender@example.com\r\n\
            To: intake@example.com\r\n\
            Content-Type: text/plain\r\n\r\n\
            A bounded summary body\r\n";
        let digest = "a".repeat(64);
        let different = "b".repeat(64);
        let server = MockServer::start().await;
        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&different),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("digest mismatch is represented in the summary");
        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status == "input_digest_mismatch"
        ));
    }

    #[tokio::test]
    async fn valid_gemini_output_becomes_a_succeeded_delivery() {
        let archive = b"From: sender@example.com\r\n\
            To: intake@example.com\r\n\
            Content-Type: text/plain\r\n\r\n\
            A bounded summary body\r\n";
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/synthetic-project/locations/global/publishers/google/models/summary-model:generateContent"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"candidates":[{"content":{"parts":[{"text":"{\"summary\":\"Useful\",\"requested_actions\":[],\"sender_stated_dates\":[],\"missing_information\":[]}"}]}}]}"#,
            ))
            .mount(&server)
            .await;
        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("provider response is represented in the summary");
        assert!(matches!(
            result,
            ProviderDelivery::Succeeded { model, result }
                if model == "summary-model" && result.summary == "Useful"
        ));
    }

    #[tokio::test]
    async fn archive_storage_errors_are_returned_without_provider_access() {
        let digest = "a".repeat(64);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage { bytes: None }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn invalid_provider_json_becomes_a_bounded_failure() {
        let archive = b"From: sender@example.com\r\n\
            To: intake@example.com\r\n\
            Content-Type: text/plain\r\n\r\n\
            A bounded summary body\r\n";
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"candidates":[{"content":{"parts":[{"text":"{}"}]}}]}"#),
            )
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("invalid output is represented in the summary");

        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status.starts_with("invalid_output:")
        ));
    }

    #[tokio::test]
    async fn the_request_sends_the_summary_schema_and_a_truncated_candidate_is_named() {
        let archive = ARCHIVE;
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "generationConfig": {
                    "responseSchema": workflows::summary_response_schema()
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "{\"summary\": \"cut o" }] },
                    "finishReason": "MAX_TOKENS"
                }],
                "usageMetadata": { "candidatesTokenCount": 12, "thoughtsTokenCount": 116 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("truncation is represented in the summary");

        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. }
                if status == "output_truncated:finish_reason=MAX_TOKENS output_tokens=12 thought_tokens=116"
        ));
    }

    #[tokio::test]
    async fn an_invalid_object_reports_the_finish_reason_and_token_counts() {
        let archive = ARCHIVE;
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "{}" }] },
                    "finishReason": "STOP"
                }],
                "usageMetadata": { "candidatesTokenCount": 2 }
            })))
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("invalid output is represented in the summary");

        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. }
                if status == "invalid_output:email summary model output is invalid finish_reason=STOP output_tokens=2"
        ));
    }

    #[tokio::test]
    async fn retryable_provider_failures_are_retried_then_quarantined() {
        let archive = b"From: sender@example.com\r\n\
            To: intake@example.com\r\n\
            Content-Type: text/plain\r\n\r\n\
            A bounded summary body\r\n";
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("provider failure is represented in the summary");

        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status.starts_with("provider_error:")
        ));
    }

    #[tokio::test]
    async fn permanent_provider_failures_are_not_retried() {
        let archive = b"From: sender@example.com\r\n\
            To: intake@example.com\r\n\
            Content-Type: text/plain\r\n\r\n\
            A bounded summary body\r\n";
        let digest = intake_digest(archive);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let result = summarize_provider(
            Arc::new(MemoryStorage {
                bytes: Some(archive.to_vec()),
            }),
            Some(providers(&server.uri())),
            SummaryProvider::Gemini,
            receipt(&digest),
            config(SummaryProvider::Gemini, &digest),
            "synthetic-project".to_string(),
        )
        .await
        .expect("provider failure is represented in the summary");

        assert!(matches!(
            result,
            ProviderDelivery::Failed { status, .. } if status.starts_with("provider_error:")
        ));
    }

    #[tokio::test]
    async fn a_valid_archive_reaches_the_gemini_adapter() {
        let digest = intake_digest(ARCHIVE);
        let server = MockServer::start().await;
        let json = r#"{\"summary\":\"Useful\",\"requested_actions\":[],\"sender_stated_dates\":[],\"missing_information\":[]}"#;
        Mock::given(method("POST"))
            .and(path(GEMINI_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"candidates":[{{"content":{{"parts":[{{"text":"{json}"}}]}}}}]}}"#
            )))
            .expect(1)
            .mount(&server)
            .await;
        {
            let provider = SummaryProvider::Gemini;
            let result = summarize_provider(
                Arc::new(MemoryStorage {
                    bytes: Some(ARCHIVE.to_vec()),
                }),
                Some(providers(&server.uri())),
                provider,
                receipt(&digest),
                config(provider, &digest),
                "synthetic-project".to_string(),
            )
            .await
            .expect("provider response is represented in the summary");
            assert!(
                matches!(&result, ProviderDelivery::Succeeded { result, .. } if result.summary == "Useful"),
                "{provider:?} did not succeed"
            );
        }
    }

    #[tokio::test]
    async fn tampered_archive_bytes_are_rejected_before_provider_call() {
        let digest = intake_digest(ARCHIVE);
        let mut tampered = ARCHIVE.to_vec();
        tampered.extend_from_slice(b"appended\r\n");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        {
            let provider = SummaryProvider::Gemini;
            let result = summarize_provider(
                Arc::new(MemoryStorage {
                    bytes: Some(tampered.clone()),
                }),
                Some(providers(&server.uri())),
                provider,
                receipt(&digest),
                config(provider, &digest),
                "synthetic-project".to_string(),
            )
            .await
            .expect("tampering is represented in the summary");
            assert!(matches!(
                result,
                ProviderDelivery::Failed { status, .. } if status == "input_digest_mismatch"
            ));
        }
    }

    #[tokio::test]
    async fn archive_outside_the_receipt_scope_is_rejected() {
        // Same bytes, but the receipt claims another deployment or mailbox:
        // the scoped digest no longer matches, so nothing reaches Vertex.
        let digest = intake_digest(ARCHIVE);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let mut other_deployment = receipt(&digest);
        other_deployment.deployment = "production".to_string();
        let mut other_mailbox = receipt(&digest);
        other_mailbox.receiving_mailbox = "other@example.com".to_string();
        let mut raw_only = receipt(&digest);
        raw_only.raw_digest = workflows::normalize_email(ARCHIVE, 1_000)
            .expect("valid MIME")
            .content_digest;
        for receipt in [other_deployment, other_mailbox, raw_only] {
            let result = summarize_provider(
                Arc::new(MemoryStorage {
                    bytes: Some(ARCHIVE.to_vec()),
                }),
                Some(providers(&server.uri())),
                SummaryProvider::Gemini,
                receipt.clone(),
                config(SummaryProvider::Gemini, &receipt.raw_digest),
                "synthetic-project".to_string(),
            )
            .await
            .expect("scope mismatch is represented in the summary");
            assert!(matches!(
                result,
                ProviderDelivery::Failed { status, .. } if status == "input_digest_mismatch"
            ));
        }
    }
}
