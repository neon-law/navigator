//! Notation business logic shared by both engine adapters.
//!
//! The Restate virtual object (`notation_service.rs`) and the DBOS port
//! (`dbos_notation.rs`) run the *same* state machines: the pure transition
//! rules, the immutable `notation_events` journal append, the funnel events,
//! the review-notification plan and its sends, and the matter close. This
//! module is that one body of logic, phrased in domain terms —
//! [`NotationLogicError`] separates a deterministic caller-visible failure
//! (bad spec, no transition, unknown notation) from a store/transport failure
//! (`SurrealDB`, the email provider), because the adapters disagree on what
//! those mean: Restate surfaces both as terminal handler errors, DBOS retries
//! the store class and fails terminally on the deterministic class.
//!
//! Identifiers only cross the engine boundary — `notation_id`, person and
//! project ids, state names, conditions. Mailboxes and project codes are
//! resolved inside the steps that use them, never frozen into a plan.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use workflows::{email::OutboundEmail, EmailService, QuestionnaireSpec, StateName, WorkflowSpec};

/// Why a piece of notation logic failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NotationLogicError {
    /// A deterministic outcome a replay would reproduce — the caller sees
    /// the failure, no engine retries it.
    #[error("{0}")]
    Terminal(String),
    /// A store or transport failure that a retry on fresh infrastructure
    /// may clear.
    #[error("{0}")]
    Store(String),
}

/// Pure state-machine step: given a spec, a current state, and a
/// condition, return the next state name. A terminal source, an unknown
/// state, or a missing condition is a [`NotationLogicError::Terminal`]
/// (deterministic — a replay would reproduce it), not a retryable one.
pub(crate) fn advance(
    spec: &WorkflowSpec,
    from: &StateName,
    condition: &str,
) -> Result<StateName, NotationLogicError> {
    if spec.is_terminal(from) {
        return Err(NotationLogicError::Terminal(format!(
            "`{}` is terminal",
            from.as_str()
        )));
    }
    let transitions = spec.transitions_from(from).ok_or_else(|| {
        NotationLogicError::Terminal(format!("unknown state `{}`", from.as_str()))
    })?;
    transitions.lookup(condition).cloned().ok_or_else(|| {
        NotationLogicError::Terminal(format!(
            "no transition from `{}` on condition `{condition}`",
            from.as_str()
        ))
    })
}

/// Recover a back event that was appended before a worker crash but whose
/// journaled side effect was not checkpointed. The runtime state still names
/// `current`; the matching last event proves that the requested Back already
/// took effect, so retries reuse its target instead of appending again.
pub(crate) fn questionnaire_back_target(
    current: &StateName,
    events: &[(String, String, String, String)],
) -> Option<StateName> {
    if let Some((from, to, condition, _)) = events.last() {
        if condition == "back" && from == current.as_str() {
            return Some(StateName::from(to.as_str()));
        }
    }
    if current == &StateName::begin() || current == &StateName::end() {
        return None;
    }
    let mut path = vec![StateName::begin()];
    for (_, to, condition, _) in events {
        if condition == "back" {
            if path.len() > 1 {
                path.pop();
            }
        } else {
            path.push(StateName::from(to.as_str()));
        }
    }
    if path.last() != Some(current) || path.len() < 2 {
        return None;
    }
    path.iter().rev().nth(1).cloned()
}

/// The two human notifications emitted by a workflow transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewNotificationHop {
    Lawyer,
    Client,
}

impl ReviewNotificationHop {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lawyer => "lawyer",
            Self::Client => "client",
        }
    }
}

/// Recipient selection is journaled separately from each send. That freezes
/// the DRI/fallback decision for a transition, while a fallback list can still
/// be sent one recipient at a time with one journal entry per email.
///
/// Opaque ids only. The engine adapters persist this value in their journals
/// so a replay reuses it, and the journal is an operator debugging surface: a
/// mailbox names a client (`portal::retainer_walk::send_intake`) and a Project
/// code names who retained the firm. Ids freeze the decision just as well, and
/// the address and the code are resolved inside the send step, whose own
/// journaled value is `()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewNotificationPlan {
    pub(crate) project_id: uuid::Uuid,
    pub(crate) recipient_ids: Vec<uuid::Uuid>,
}

/// Which human a workflow transition notifies, derived from the hop it makes.
pub(crate) fn review_notification_hop(
    from: &StateName,
    condition: &str,
    next: &StateName,
) -> Option<ReviewNotificationHop> {
    if next.as_str().starts_with("lawyer_review") {
        return Some(ReviewNotificationHop::Lawyer);
    }
    if from.as_str().starts_with("lawyer_review")
        && !next.as_str().starts_with("reask__")
        && matches!(condition, "approved" | "_")
    {
        return Some(ReviewNotificationHop::Client);
    }
    None
}

/// The wording one notification carries.
///
/// Authored here rather than in `neon/locales/`, because those catalogs are
/// the firm's *page* copy and say so: the shared catalog is exported to the
/// other repository entry by entry, and `neon::locales` asserts that every key
/// it authors is read by a shipped page. Transactional email is read by no
/// page, so a key for it fails that gate. Copy with no catalog belongs in the
/// module that renders it.
#[derive(Clone, Copy)]
pub(crate) struct ReviewCopy {
    pub(crate) subject: &'static str,
    pub(crate) body: &'static str,
}

const LAWYER_REVIEW_COPY: ReviewCopy = ReviewCopy {
    subject: "A draft is ready for your review",
    body: "A draft is ready for your review.",
};
const CLIENT_REVIEW_COPY: ReviewCopy = ReviewCopy {
    subject: "Your reviewed draft is ready",
    body: "Your reviewed draft is ready.",
};

pub(crate) const fn review_copy(hop: ReviewNotificationHop) -> ReviewCopy {
    match hop {
        ReviewNotificationHop::Lawyer => LAWYER_REVIEW_COPY,
        ReviewNotificationHop::Client => CLIENT_REVIEW_COPY,
    }
}

/// The funnel step caused by one durable workflow transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkflowFunnelStep {
    IntakeComplete,
    ReviewEntered,
    Sent(telemetry::FunnelChannel),
}

pub(crate) fn workflow_funnel_step(
    _from: &StateName,
    _condition: &str,
    next: &StateName,
) -> Option<WorkflowFunnelStep> {
    if next.as_str().starts_with("lawyer_review") {
        return Some(WorkflowFunnelStep::ReviewEntered);
    }
    None
}

pub(crate) fn review_recipient_ids(
    rows: &[store::projects::PersonProjectRole],
    hop: ReviewNotificationHop,
) -> Vec<uuid::Uuid> {
    match hop {
        ReviewNotificationHop::Lawyer => {
            let eligible = rows.iter().filter(|row| {
                !store::projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
            });
            // The role tier is resolved after the row query. A DRI marker is
            // the first choice; person lookup rejects stale links and filters
            // the result to the licensed lawyer tiers.
            let dris: Vec<uuid::Uuid> = eligible
                .clone()
                .filter(|row| row.is_lawyer_dri)
                .map(|row| row.person_id)
                .collect();
            if dris.is_empty() {
                eligible.map(|row| row.person_id).collect()
            } else {
                dris
            }
        }
        ReviewNotificationHop::Client => rows
            .iter()
            .filter(|row| row.participation == "client")
            .find(|row| row.is_client_dri)
            .or_else(|| rows.iter().find(|row| row.participation == "client"))
            .map(|row| vec![row.person_id])
            .unwrap_or_default(),
    }
}

pub(crate) fn review_fallback_ids(rows: &[store::projects::PersonProjectRole]) -> Vec<uuid::Uuid> {
    rows.iter()
        .filter(|row| {
            !store::projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
        })
        .map(|row| row.person_id)
        .collect()
}

pub(crate) async fn resolve_review_notification_plan(
    surreal: &store::surreal::SurrealDb,
    notation_id: uuid::Uuid,
    hop: ReviewNotificationHop,
) -> Result<ReviewNotificationPlan, NotationLogicError> {
    let notation = store::notations::find_by_id(surreal, notation_id)
        .await
        .map_err(|e| {
            NotationLogicError::Store(format!("review notification notation: {e}"))
        })?
        .ok_or_else(|| {
            NotationLogicError::Terminal("review notification notation not found".into())
        })?;
    let project = store::projects::find_by_id(surreal, notation.project_id)
        .await
        .map_err(|e| {
            NotationLogicError::Store(format!("review notification project: {e}"))
        })?
        .ok_or_else(|| {
            NotationLogicError::Terminal("review notification project not found".into())
        })?;
    let rows = store::projects::participations_for_project(surreal, project.id)
        .await
        .map_err(|e| {
            NotationLogicError::Store(format!("review notification participants: {e}"))
        })?;
    let candidate_ids = review_recipient_ids(&rows, hop);
    let mut candidates = Vec::new();
    for person_id in candidate_ids {
        if let Some(person) = store::persons::find_by_id(surreal, person_id)
            .await
            .map_err(|e| NotationLogicError::Store(format!("review notification person: {e}")))?
        {
            candidates.push(person);
        }
    }
    let recipient_ids: Vec<uuid::Uuid> = match hop {
        ReviewNotificationHop::Lawyer => candidates
            .into_iter()
            .filter(|person| person.role.is_lawyer_tier())
            .map(|person| person.id)
            .collect(),
        ReviewNotificationHop::Client => candidates
            .into_iter()
            .next()
            .map(|person| person.id)
            .into_iter()
            .collect(),
    };
    let recipient_ids = if matches!(hop, ReviewNotificationHop::Lawyer) && recipient_ids.is_empty()
    {
        let mut fallback = Vec::new();
        for person_id in review_fallback_ids(&rows) {
            if let Some(person) = store::persons::find_by_id(surreal, person_id)
                .await
                .map_err(|e| {
                    NotationLogicError::Store(format!(
                        "review notification fallback person: {e}"
                    ))
                })?
            {
                if person.role.is_lawyer_tier() {
                    fallback.push(person.id);
                }
            }
        }
        fallback
    } else {
        recipient_ids
    };

    if recipient_ids.is_empty() {
        tracing::warn!(
            target: "audit",
            audit = true,
            notation_id = %notation_id,
            project_id = %project.id,
            recipient_role = "none",
            hop = hop.as_str(),
            outcome = "skipped_no_recipient",
            "review notification skipped: no recipient"
        );
    }

    Ok(ReviewNotificationPlan {
        project_id: project.id,
        recipient_ids,
    })
}

/// What one send needs that the journaled plan deliberately does not carry.
pub(crate) struct ReviewNotificationTarget {
    pub(crate) address: String,
    pub(crate) project_code: String,
}

/// Read the recipient's mailbox and the matter's code at send time.
///
/// Both are client identifiers, so they are resolved here rather than frozen
/// into [`ReviewNotificationPlan`]: this runs inside the send step, whose
/// journaled value is `()`.
pub(crate) async fn review_notification_target(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
    recipient_id: uuid::Uuid,
) -> Result<ReviewNotificationTarget, NotationLogicError> {
    let person = store::persons::find_by_id(surreal, recipient_id)
        .await
        .map_err(|e| {
            NotationLogicError::Store(format!("review notification recipient: {e}"))
        })?
        .ok_or_else(|| {
            NotationLogicError::Terminal("review notification recipient not found".into())
        })?;
    let project = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|e| NotationLogicError::Store(format!("review notification project: {e}")))?
        .ok_or_else(|| {
            NotationLogicError::Terminal("review notification project not found".into())
        })?;
    Ok(ReviewNotificationTarget {
        address: person.email,
        project_code: project.code,
    })
}

pub(crate) async fn send_review_notification(
    email: Arc<dyn EmailService>,
    notation_id: uuid::Uuid,
    project_id: uuid::Uuid,
    project_code: &str,
    recipient_id: uuid::Uuid,
    recipient_email: &str,
    hop: ReviewNotificationHop,
) -> Result<(), NotationLogicError> {
    let copy = review_copy(hop);
    let base_url = workflows::email::base_url_from_env();
    let path = match hop {
        ReviewNotificationHop::Lawyer => {
            format!("/app/lawyer/notations/{notation_id}/review")
        }
        ReviewNotificationHop::Client => format!("/app/projects/{project_code}"),
    };
    let body = format!(
        "{}\n\n{}{}",
        copy.body,
        base_url.trim_end_matches('/'),
        path
    );
    // Every person-addressed Navigator email carries the inline-styled firm
    // layout as its `text/html` alternative beside the plain part, so a rich
    // client shows the wordmark rather than a bare URL.
    let html = workflows::email::render_email_html(&body, &base_url);
    let outbound = OutboundEmail::new(recipient_email, copy.subject, body)
        .with_html(html)
        .with_template(format!("notation-review-{}", hop.as_str()))
        .with_person(recipient_id.to_string());
    match email.send(outbound).await {
        Ok(_) => {
            tracing::info!(
                target: "audit",
                audit = true,
                notation_id = %notation_id,
                project_id = %project_id,
                recipient_role = hop.as_str(),
                hop = hop.as_str(),
                outcome = "sent",
                "review notification sent"
            );
            Ok(())
        }
        Err(error) => {
            // The variant, never the message: `EmailError::InvalidRecipient`
            // carries the address it refused, and telemetry leaves the firm's
            // trust boundary. The two words still separate a bad mailbox from
            // a provider outage, which is the whole diagnostic question, and
            // they ride the adapter error rather than a new audit field — the
            // collector's allow-list is fail-closed, so a key added here would
            // export as a blank until it is allow-listed too.
            let reason = match error {
                workflows::email::EmailError::InvalidRecipient(_) => "invalid recipient",
                workflows::email::EmailError::Transport(_) => "transport",
            };
            tracing::info!(
                target: "audit",
                audit = true,
                notation_id = %notation_id,
                project_id = %project_id,
                recipient_role = hop.as_str(),
                hop = hop.as_str(),
                outcome = "failed",
                "review notification failed"
            );
            Err(NotationLogicError::Terminal(format!(
                "review notification send failed: {reason}"
            )))
        }
    }
}

pub(crate) async fn record_notation_funnel_step(
    surreal: &store::surreal::SurrealDb,
    notation_id: uuid::Uuid,
    step: WorkflowFunnelStep,
) -> Result<(), NotationLogicError> {
    let notation = store::notations::find_by_id(surreal, notation_id)
        .await
        .map_err(|e| NotationLogicError::Store(format!("funnel notation: {e}")))?
        .ok_or_else(|| NotationLogicError::Terminal("funnel notation not found".into()))?;
    let notation_id = notation_id.to_string();
    let project_id = notation.project_id.to_string();
    let event = match step {
        WorkflowFunnelStep::IntakeComplete => telemetry::FunnelEvent::IntakeComplete {
            notation_id: &notation_id,
            project_id: &project_id,
        },
        WorkflowFunnelStep::ReviewEntered => telemetry::FunnelEvent::ReviewEntered {
            notation_id: &notation_id,
            project_id: &project_id,
        },
        WorkflowFunnelStep::Sent(channel) => telemetry::FunnelEvent::Sent {
            notation_id: &notation_id,
            project_id: &project_id,
            channel,
        },
    };
    telemetry::record_funnel_event(event);
    Ok(())
}

/// Validate a questionnaire spec the way both start handlers do: the yaml
/// must round-trip before it is stored, so a later signal can't fail
/// mid-flight.
pub(crate) fn validate_questionnaire_spec(spec_yaml: &str) -> Result<(), NotationLogicError> {
    QuestionnaireSpec::from_yaml(spec_yaml)
        .map(|_| ())
        .map_err(|e| NotationLogicError::Terminal(format!("questionnaire spec: {e}")))
}

/// Validate a workflow spec the same way.
pub(crate) fn validate_workflow_spec(spec_yaml: &str) -> Result<(), NotationLogicError> {
    WorkflowSpec::from_yaml(spec_yaml)
        .map(|_| ())
        .map_err(|e| NotationLogicError::Terminal(format!("workflow spec: {e}")))
}
