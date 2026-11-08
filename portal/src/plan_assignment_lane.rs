//! The plan-assignment intake lane: recognize assignment mail from a
//! fixed-fee plan provider (ARAG, LegalShield) on the SendGrid inbound
//! path, parse it with the store's pure parser, and archive the parsed
//! summary for matter preparation.
//!
//! Nothing here creates a matter or replies to the member. A parsed
//! assignment is a summary for a person to confirm; an unparseable
//! message from a plan provider archives the parse failure with its
//! reason and stays in the triage backlog — no plan mail is dropped, and
//! every outcome is auditable in object storage under a content-derived
//! key that survives redelivery.

use std::sync::Arc;

use cloud::StorageService;
use serde::Serialize;

use crate::inbound_email::InboundEmail;

/// What one lane run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneOutcome {
    /// The sender is not a plan provider — nothing to record.
    NotPlanMail,
    /// The notice parsed; its summary is archived for matter preparation.
    Parsed {
        /// The provider spelling recorded beside the summary.
        provider: &'static str,
        /// The archive object key holding the summary.
        archive_key: String,
    },
    /// Plan mail the parser could not read; the failure and its reason
    /// are archived and the message waits in triage for a person.
    Unparseable {
        /// Why the parser refused, for the audit record.
        reason: String,
        /// The archive object key holding the failure record.
        archive_key: String,
    },
}

/// One archived lane record: the outcome plus everything a lawyer needs
/// to prepare or confirm the assignment without reopening the raw mail.
#[derive(Debug, Serialize)]
struct LaneRecord<'a> {
    outcome: &'a str,
    provider: &'a str,
    message_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignment: Option<&'a store::plan_assignment::Assignment>,
}

/// Run the lane for one inbound email. Storage failures propagate as a
/// `String` so the caller treats a broken archive like any other failed
/// intake stage — the raw mail is already durable by then.
pub(crate) async fn run(
    storage: &Arc<dyn StorageService>,
    email: &InboundEmail,
) -> Result<LaneOutcome, String> {
    let Some(provider) = store::plan_assignment::provider_for_sender(&email.from) else {
        return Ok(LaneOutcome::NotPlanMail);
    };
    // Content-derived: a provider redelivery of the same raw message
    // lands on the same key instead of a second object.
    let digest = workflows::scoped_email_digest("plan-assignment", provider.as_str(), &email.raw);
    let archive_key = format!("inbound/plan-assignments/{digest}.json");

    match store::plan_assignment::parse_for_provider(provider, &email.text) {
        Ok(assignment) => {
            archive(
                storage,
                &archive_key,
                &LaneRecord {
                    outcome: "parsed",
                    provider: provider.as_str(),
                    message_id: email.message_id.as_deref(),
                    reason: None,
                    assignment: Some(&assignment),
                },
            )
            .await?;
            Ok(LaneOutcome::Parsed {
                provider: provider.as_str(),
                archive_key,
            })
        }
        Err(error) => {
            let reason = error.to_string();
            archive(
                storage,
                &archive_key,
                &LaneRecord {
                    outcome: "unparseable",
                    provider: provider.as_str(),
                    message_id: email.message_id.as_deref(),
                    reason: Some(&reason),
                    assignment: None,
                },
            )
            .await?;
            Ok(LaneOutcome::Unparseable {
                reason,
                archive_key,
            })
        }
    }
}

async fn archive(
    storage: &Arc<dyn StorageService>,
    archive_key: &str,
    record: &LaneRecord<'_>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    storage
        .put(archive_key, &bytes, "application/json")
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{run, LaneOutcome};
    use crate::inbound_email::InboundEmail;
    use store::plan_assignment::fixtures;

    fn plan_email(from: &str, text: String) -> InboundEmail {
        InboundEmail {
            from: from.into(),
            subject: "Assignment".into(),
            text,
            message_id: Some("<plan-1@mail.example>".into()),
            ..InboundEmail::default()
        }
    }

    #[tokio::test]
    async fn a_recognized_notice_parses_and_archives_its_summary() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "ARAG <assignments@notice.arag.com>",
            fixtures::arag_assignment_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Parsed {
            provider,
            archive_key,
        } = outcome
        else {
            panic!("expected a parsed outcome, got {outcome:?}");
        };
        assert_eq!(provider, "arag");
        let archived = storage.get(&archive_key).await.expect("archive readable");
        let record: serde_json::Value = serde_json::from_slice(&archived.bytes).unwrap();
        assert_eq!(record["outcome"], "parsed");
        assert_eq!(record["provider"], "arag");
        assert_eq!(record["assignment"]["member_name"], "Dana Example");
        assert_eq!(record["assignment"]["member_number"], "AR-004512");
    }

    #[tokio::test]
    async fn a_legal_shield_notice_parses_under_its_own_labels() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "LegalShield <assignments@notice.legalshield.com>",
            fixtures::legal_shield_assignment_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Parsed {
            provider,
            archive_key,
        } = outcome
        else {
            panic!("expected a parsed outcome, got {outcome:?}");
        };
        assert_eq!(provider, "legal_shield");
        let archived = storage.get(&archive_key).await.expect("archive readable");
        let record: serde_json::Value = serde_json::from_slice(&archived.bytes).unwrap();
        assert_eq!(record["assignment"]["member_name"], "Harper Sample");
    }

    #[tokio::test]
    async fn unparseable_plan_mail_archives_the_reason_and_stays_in_triage() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email("mail@legalshield.com", "please call us".into());
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Unparseable {
            reason,
            archive_key,
        } = outcome
        else {
            panic!("expected an unparseable outcome, got {outcome:?}");
        };
        assert!(
            reason.contains("member name"),
            "reason names the field: {reason}"
        );
        let archived = storage.get(&archive_key).await.expect("archive readable");
        let record: serde_json::Value = serde_json::from_slice(&archived.bytes).unwrap();
        assert_eq!(record["outcome"], "unparseable");
        assert_eq!(record["reason"], reason);
        assert_eq!(record["assignment"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_sender_off_the_provider_domains_is_not_plan_mail() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "Dana Example <dana@example.com>",
            fixtures::arag_assignment_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        assert_eq!(outcome, LaneOutcome::NotPlanMail);
    }

    #[tokio::test]
    async fn a_redelivered_notice_reuses_the_same_archive_key() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "ARAG <assignments@notice.arag.com>",
            fixtures::arag_assignment_text(),
        );
        let first = run(&storage, &email).await.expect("first run");
        let second = run(&storage, &email).await.expect("redelivery");
        let LaneOutcome::Parsed { archive_key: a, .. } = first else {
            panic!("expected parsed, got {first:?}");
        };
        let LaneOutcome::Parsed { archive_key: b, .. } = second else {
            panic!("expected parsed, got {second:?}");
        };
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn a_lookalike_domain_never_reaches_the_lane() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "ARAG <assignments@arag.com.example>",
            fixtures::arag_assignment_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        assert_eq!(outcome, LaneOutcome::NotPlanMail);
    }
}
