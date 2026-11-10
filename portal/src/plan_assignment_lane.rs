//! The plan-assignment intake lane: recognize mail from a fixed-fee plan
//! provider (ARAG, `LegalEASE`, `LegalShield`) on the `SendGrid` inbound
//! path, read its Plan Case Number with the store's pure parser, and
//! archive the parsed summary for matter preparation.
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
        /// The parsed assignment, for staging the attorney review record.
        assignment: store::plan_assignment::Assignment,
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

    match store::plan_assignment::parse(provider, &email.subject, &email.text) {
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
                assignment: assignment.clone(),
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

/// The persisted landing time of one ingested message — the origin of the
/// plan's acceptance clock. The message row is written before the lane runs,
/// so an unreadable row or timestamp skips staging rather than fabricating a
/// clock the SLA deadline would silently trust.
async fn landing_time(
    db: &store::surreal::SurrealDb,
    message_id: uuid::Uuid,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let message = store::inbox::message_by_id(db, message_id)
        .await
        .ok()
        .flatten()?;
    chrono::DateTime::parse_from_rfc3339(&message.occurred_at)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// Stage a parsed notice for attorney review, anchored to the ingested
/// message. The firm is resolved exactly as ingest resolved it; without
/// PostgreSQL the plan and fee quote stay unresolved for the attorney to
/// price on the review screen. A staging failure never fails the intake —
/// the raw mail is already durable and triage still holds the message — so
/// the outcome is logged and returned as `None`.
pub(crate) async fn stage_for_review(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
    inbound: &store::inbox::IngestedInbound,
    outcome: &LaneOutcome,
) -> Option<store::plan_intake::StagedAssignment> {
    let LaneOutcome::Parsed {
        provider,
        assignment,
        ..
    } = outcome
    else {
        return None;
    };
    let firm_id = match crate::inbox::inbound_firm_id(db, email).await {
        Ok(firm_id) => firm_id,
        Err(error) => {
            tracing::warn!(error = %error, "plan-assignment staging skipped: no firm");
            return None;
        }
    };
    let Some(landing_at) = landing_time(db, inbound.message_id).await else {
        tracing::warn!(
            message_id = %inbound.message_id,
            "plan-assignment staging skipped: no persisted landing time"
        );
        return None;
    };
    let input = store::plan_intake::StageInput {
        firm_id,
        conversation_id: inbound.conversation_id,
        source_message_id: inbound.message_id,
        landing_at,
        provider: (*provider).to_string(),
        assignment: assignment.clone(),
    };
    // PostgreSQL is resolved on demand: an unconfigured or unreachable
    // database stages the notice with an unresolved plan and fee quote.
    let pg = store::postgres::PostgresDb::from_env().await.ok();
    match store::plan_intake::stage(db, pg.as_ref(), &input).await {
        Ok(staged) => {
            tracing::info!(
                staged_id = %staged.id,
                message_id = %inbound.message_id,
                "plan-assignment staged for attorney review"
            );
            Some(staged)
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                message_id = %inbound.message_id,
                "plan-assignment staging failed; the message stays in triage"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run, LaneOutcome};
    use crate::inbound_email::InboundEmail;
    use store::plan_assignment::fixtures;

    fn plan_email(from: &str, subject: &str, text: String) -> InboundEmail {
        InboundEmail {
            from: from.into(),
            subject: subject.into(),
            text,
            message_id: Some("<plan-1@mail.example>".into()),
            ..InboundEmail::default()
        }
    }

    #[tokio::test]
    async fn an_arag_case_request_parses_and_archives_its_summary() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "ARAG <caserequest@aragconnect.com>",
            fixtures::ARAG_SUBJECT,
            fixtures::arag_case_request_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Parsed {
            provider,
            archive_key,
            assignment: _,
        } = outcome
        else {
            panic!("expected a parsed outcome, got {outcome:?}");
        };
        assert_eq!(provider, "arag");
        let archived = storage.get(&archive_key).await.expect("archive readable");
        let record: serde_json::Value = serde_json::from_slice(&archived.bytes).unwrap();
        assert_eq!(record["outcome"], "parsed");
        assert_eq!(record["provider"], "arag");
        assert_eq!(record["assignment"]["case_number"], "4900000-001");
        assert_eq!(record["assignment"]["member_name"], "Dana Example");
    }

    #[tokio::test]
    async fn a_legal_ease_case_review_parses_under_its_own_shape() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "LegalEASE <no-reply@legaleaseplan.com>",
            fixtures::LEGAL_EASE_SUBJECT,
            fixtures::legal_ease_case_review_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Parsed {
            provider,
            archive_key,
            assignment: _,
        } = outcome
        else {
            panic!("expected a parsed outcome, got {outcome:?}");
        };
        assert_eq!(provider, "legal_ease");
        let archived = storage.get(&archive_key).await.expect("archive readable");
        let record: serde_json::Value = serde_json::from_slice(&archived.bytes).unwrap();
        assert_eq!(record["assignment"]["case_number"], "400001");
        assert_eq!(record["assignment"]["member_number"], "9900001");
    }

    #[tokio::test]
    async fn unparseable_plan_mail_archives_the_reason_and_stays_in_triage() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "mail@legalshield.com",
            "Member question",
            "please call us".into(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        let LaneOutcome::Unparseable {
            reason,
            archive_key,
        } = outcome
        else {
            panic!("expected an unparseable outcome, got {outcome:?}");
        };
        assert!(
            reason.contains("case number"),
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
            fixtures::ARAG_SUBJECT,
            fixtures::arag_case_request_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        assert_eq!(outcome, LaneOutcome::NotPlanMail);
    }

    #[tokio::test]
    async fn a_redelivered_notice_reuses_the_same_archive_key() {
        let storage = crate::test_support::fs_storage().await;
        let email = plan_email(
            "ARAG <caserequest@aragconnect.com>",
            fixtures::ARAG_SUBJECT,
            fixtures::arag_case_request_text(),
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
            "ARAG <caserequest@aragconnect.com.example>",
            fixtures::ARAG_SUBJECT,
            fixtures::arag_case_request_text(),
        );
        let outcome = run(&storage, &email).await.expect("lane runs");
        assert_eq!(outcome, LaneOutcome::NotPlanMail);
    }
}
